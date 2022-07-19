use super::proxy_main::{LocalExecutor, Proxy};
use crate::{constants::*, error::*, log::*};
#[cfg(feature = "h3")]
use futures::StreamExt;
use futures::{future::FutureExt, select};
use hyper::{client::connect::Connect, server::conn::Http};
use rustls::ServerConfig;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::watch, time::Duration};

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  async fn cert_service(&self, server_crypto_tx: watch::Sender<Option<Arc<ServerConfig>>>) {
    info!("Start cert watch service");
    loop {
      if let Ok(server_crypto) = self
        .globals
        .backends
        .generate_server_crypto_with_cert_resolver()
        .await
      {
        if let Err(_e) = server_crypto_tx.send(Some(Arc::new(server_crypto))) {
          error!("Failed to populate server crypto");
          break;
        }
      } else {
        error!("Failed to update certs");
      }
      tokio::time::sleep(Duration::from_secs(CERTS_WATCH_DELAY_SECS.into())).await;
    }
  }

  // TCP Listener Service, i.e., http/2 and http/1.1
  async fn listener_service(
    &self,
    server: Http<LocalExecutor>,
    mut server_crypto_rx: watch::Receiver<Option<Arc<ServerConfig>>>,
  ) -> Result<()> {
    let tcp_listener = TcpListener::bind(&self.listening_on).await?;
    info!("Start TCP proxy serving with HTTPS request for configured host names");

    let mut server_crypto: Option<Arc<ServerConfig>> = None;
    loop {
      select! {
        tcp_cnx = tcp_listener.accept().fuse() => {
          // First check SNI
          let rustls_acceptor = rustls::server::Acceptor::new();
          if server_crypto.is_none() || tcp_cnx.is_err() || rustls_acceptor.is_err() {
            continue;
          }
          let (raw_stream, _client_addr) = tcp_cnx.unwrap();
          let acceptor = tokio_rustls::LazyConfigAcceptor::new(rustls_acceptor.unwrap(), raw_stream).await;
          if acceptor.is_err() {
            continue;
          }
          let start = acceptor.unwrap();

          let client_hello = start.client_hello();
          // Find server config for given SNI
          if client_hello.server_name().is_none(){
            info!("No SNI in ClientHello");
            continue;
          }
          let server_name = client_hello.server_name().unwrap().to_ascii_lowercase();
          debug!("SNI in ClientHello: {:?}", server_name);
          // Finally serve the TLS connection
          if let Ok(stream) = start.into_stream(server_crypto.clone().unwrap()).await {
            self.clone().client_serve(stream, server.clone(), _client_addr, Some(server_name.as_bytes()))
          }
        }
        _ = server_crypto_rx.changed().fuse() => {
          if server_crypto_rx.borrow().is_none() {
            break;
          }
          server_crypto = server_crypto_rx.borrow().clone();
        }
        complete => break
      }
    }
    Ok(()) as Result<()>
  }

  #[cfg(feature = "h3")]
  async fn listener_service_h3(
    &self,
    mut server_crypto_rx: watch::Receiver<Option<Arc<ServerConfig>>>,
  ) -> Result<()> {
    let server_crypto = self
      .globals
      .backends
      .generate_server_crypto_with_cert_resolver()
      .await?;

    let server_config_h3 = quinn::ServerConfig::with_crypto(Arc::new(server_crypto));
    let (endpoint, mut incoming) = quinn::Endpoint::server(server_config_h3, self.listening_on)?;
    info!("Start UDP proxy serving with HTTP/3 request for configured host names");

    let mut server_crypto: Option<Arc<ServerConfig>> = None;
    loop {
      select! {
        new_conn = incoming.next().fuse() => {
          if server_crypto.is_none() || new_conn.is_none() {
            continue;
          }
          let mut conn = new_conn.unwrap();
          let hsd = if let Ok(h) = conn.handshake_data().await {
            h
          } else {
            continue
          };
          let hsd_downcast = if let Ok(d) = hsd.downcast::<quinn::crypto::rustls::HandshakeData>() {
            d
          } else {
            continue;
          };
          let new_server_name = if let Some(sn) = hsd_downcast.server_name {
            sn.as_bytes().to_ascii_lowercase()
          } else {
            warn!("HTTP/3 no SNI is given");
            continue;
          };
          debug!(
            "HTTP/3 connection incoming (SNI {:?})",
            new_server_name
          );
          self.clone().client_serve_h3(conn, new_server_name.as_ref());
        }
        _ = server_crypto_rx.changed().fuse() => {
          if server_crypto_rx.borrow().is_none() {
            break;
          }
          server_crypto = server_crypto_rx.borrow().clone();
          if server_crypto.is_some(){
            debug!("Reload server crypto");
            endpoint.set_server_config(Some(quinn::ServerConfig::with_crypto(server_crypto.clone().unwrap())));
          }
        }
        complete => break
      }
    }
    endpoint.wait_idle().await;
    Ok(()) as Result<()>
  }

  pub async fn start_with_tls(self, server: Http<LocalExecutor>) -> Result<()> {
    let (tx, rx) = watch::channel::<Option<Arc<ServerConfig>>>(None);
    #[cfg(not(feature = "h3"))]
    {
      select! {
        _= self.cert_service(tx).fuse() => {
          error!("Cert service for TLS exited");
        },
        _ = self.listener_service(server, rx).fuse() => {
          error!("TCP proxy service for TLS exited");
        },
        complete => {
          error!("Something went wrong");
          return Ok(())
        }
      };
      Ok(())
    }
    #[cfg(feature = "h3")]
    {
      if self.globals.http3 {
        select! {
          _= self.cert_service(tx).fuse() => {
            error!("Cert service for TLS exited");
          },
          _ = self.listener_service(server, rx.clone()).fuse() => {
            error!("TCP proxy service for TLS exited");
          },
          _= self.listener_service_h3(rx).fuse() => {
            error!("UDP proxy service for QUIC exited");
          },
          complete => {
            error!("Something went wrong");
            return Ok(())
          }
        };
        Ok(())
      } else {
        select! {
          _= self.cert_service(tx).fuse() => {
            error!("Cert service for TLS exited");
          },
          _ = self.listener_service(server, rx).fuse() => {
            error!("TCP proxy service for TLS exited");
          },
          complete => {
            error!("Something went wrong");
            return Ok(())
          }
        };
        Ok(())
      }
    }
  }
}
