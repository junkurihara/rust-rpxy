use super::proxy_main::{LocalExecutor, Proxy};
use crate::{constants::*, error::*, log::*};
use hyper::{client::connect::Connect, server::conn::Http};
use rustls::ServerConfig;
use std::sync::Arc;
use tokio::{
  net::TcpListener,
  sync::watch,
  time::{sleep, Duration},
};
use tokio_rustls::TlsAcceptor;

#[cfg(feature = "http3")]
use futures::StreamExt;
#[cfg(feature = "http3")]
use quinn::{crypto::rustls::HandshakeData, Endpoint, ServerConfig as QuicServerConfig, TransportConfig};

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  async fn cert_service(&self, server_crypto_tx: watch::Sender<Option<Arc<ServerConfig>>>) {
    info!("Start cert watch service");
    loop {
      if let Ok(server_crypto) = self.globals.backends.generate_server_crypto_with_cert_resolver().await {
        if let Err(_e) = server_crypto_tx.send(Some(Arc::new(server_crypto))) {
          error!("Failed to populate server crypto");
          break;
        }
      } else {
        error!("Failed to update certs");
      }
      sleep(Duration::from_secs(CERTS_WATCH_DELAY_SECS.into())).await;
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

    // let mut server_crypto: Option<Arc<ServerConfig>> = None;
    let mut tls_acceptor: Option<TlsAcceptor> = None;
    loop {
      tokio::select! {
        tcp_cnx = tcp_listener.accept() => {
          if tls_acceptor.is_none() || tcp_cnx.is_err() {
            continue;
          }
          let (raw_stream, client_addr) = tcp_cnx.unwrap();
          let acceptor = tls_acceptor.clone().unwrap();
          let server_clone = server.clone();
          let self_inner = self.clone();

          let fut = async move {
            match acceptor.accept(raw_stream).await {
              Ok(stream) => {
                // Retrieve SNI
                let (_, conn) = stream.get_ref();
                let server_name = conn.sni_hostname();
                debug!("HTTP/2 or 1.1: SNI in ClientHello: {:?}", server_name);
                let server_name = server_name.map_or_else(|| None, |v| Some(v.as_bytes().to_ascii_lowercase()));
                if server_name.is_none(){
                  Err(anyhow!("No SNI is given"))
                } else {
                  self_inner.client_serve(stream, server_clone, client_addr, server_name); // TODO: don't want to pass copied value...
                  Ok(())
                }
              },
              Err(e) => {
                Err(anyhow!("Failed to accept TLS stream {}", e))
              }
            }
          };
          self.globals.runtime_handle.spawn( async move {
            if let Err(e) = fut.await {
              error!("{}", e);
            }
          });
        }
        _ = server_crypto_rx.changed() => {
          if server_crypto_rx.borrow().is_none() {
            break;
          }
          let server_crypto = server_crypto_rx.borrow().clone().unwrap();
          tls_acceptor = Some(TlsAcceptor::from(server_crypto));
        }
        else => break
      }
    }
    Ok(()) as Result<()>
  }

  #[cfg(feature = "http3")]
  async fn listener_service_h3(&self, mut server_crypto_rx: watch::Receiver<Option<Arc<ServerConfig>>>) -> Result<()> {
    let mut transport_config_quic = TransportConfig::default();
    transport_config_quic
      .max_concurrent_bidi_streams(self.globals.h3_max_concurrent_bidistream)
      .max_concurrent_uni_streams(self.globals.h3_max_concurrent_unistream);

    let server_crypto = self
      .globals
      .backends
      .generate_server_crypto_with_cert_resolver()
      .await?;

    let mut server_config_h3 = QuicServerConfig::with_crypto(Arc::new(server_crypto));
    server_config_h3.transport = Arc::new(transport_config_quic);
    server_config_h3.concurrent_connections(self.globals.h3_max_concurrent_connections);
    let (endpoint, mut incoming) = Endpoint::server(server_config_h3, self.listening_on)?;
    info!("Start UDP proxy serving with HTTP/3 request for configured host names");

    let mut server_crypto: Option<Arc<ServerConfig>> = None;
    loop {
      tokio::select! {
        new_conn = incoming.next() => {
          if server_crypto.is_none() || new_conn.is_none() {
            continue;
          }
          let mut conn = new_conn.unwrap();
          let hsd = match conn.handshake_data().await {
            Ok(h) => h,
            Err(_) => continue
          };

          let hsd_downcast = match hsd.downcast::<HandshakeData>() {
            Ok(d) => d,
            Err(_) => continue
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
          // TODO: server_nameをここで出してどんどん深く投げていくのは効率が悪い。connecting -> connectionsの後でいいのでは？
          // TODO: 通常のTLSと同じenumか何かにまとめたい
          let fut = self.clone().connection_serve_h3(conn, new_server_name);
          self.globals.runtime_handle.spawn(async move {
            // Timeout is based on underlying quic
            if let Err(e) = fut.await {
              warn!("QUIC or HTTP/3 connection failed: {}", e)
            }
          });
        }
        _ = server_crypto_rx.changed() => {
          if server_crypto_rx.borrow().is_none() {
            break;
          }
          server_crypto = server_crypto_rx.borrow().clone();
          if server_crypto.is_some(){
            debug!("Reload server crypto");
            endpoint.set_server_config(Some(QuicServerConfig::with_crypto(server_crypto.clone().unwrap())));
          }
        }
        // complete => break
      }
    }
    endpoint.wait_idle().await;
    Ok(()) as Result<()>
  }

  pub async fn start_with_tls(self, server: Http<LocalExecutor>) -> Result<()> {
    let (tx, rx) = watch::channel::<Option<Arc<ServerConfig>>>(None);
    #[cfg(not(feature = "http3"))]
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
    #[cfg(feature = "http3")]
    {
      if self.globals.http3 {
        tokio::select! {
          _= self.cert_service(tx) => {
            error!("Cert service for TLS exited");
          },
          _ = self.listener_service(server, rx.clone()) => {
            error!("TCP proxy service for TLS exited");
          },
          _= self.listener_service_h3(rx) => {
            error!("UDP proxy service for QUIC exited");
          },
          // complete => {
          //   error!("Something went wrong");
          //   return Ok(())
          // }
        };
        Ok(())
      } else {
        tokio::select! {
          _= self.cert_service(tx) => {
            error!("Cert service for TLS exited");
          },
          _ = self.listener_service(server, rx) => {
            error!("TCP proxy service for TLS exited");
          },
          // complete => {
          //   error!("Something went wrong");
          //   return Ok(())
          // }
        };
        Ok(())
      }
    }
  }
}
