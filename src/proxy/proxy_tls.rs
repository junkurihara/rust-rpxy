use super::{
  proxy_main::{LocalExecutor, Proxy},
  ServerNameLC,
};
use crate::{constants::*, error::*, log::*};
#[cfg(feature = "h3")]
use futures::StreamExt;
use futures::{future::FutureExt, select};
use hyper::{client::connect::Connect, server::conn::Http};
use rustc_hash::FxHashMap as HashMap;
use rustls::ServerConfig;
use std::sync::Arc;
use tokio::{net::TcpListener, sync::watch, time::Duration};

type ServerCryptoMap = HashMap<ServerNameLC, Arc<ServerConfig>>;

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  async fn cert_service(&self, server_crypto_tx: watch::Sender<Option<ServerCryptoMap>>) {
    info!("Start cert watch service");
    loop {
      let mut hm_server_config = HashMap::<ServerNameLC, Arc<ServerConfig>>::default();
      for (server_name_bytes, backend) in self.backends.apps.iter() {
        if backend.tls_cert_key_path.is_some() && backend.tls_cert_path.is_some() {
          match backend.update_server_config().await {
            Err(_e) => {
              error!(
                "Failed to update certs for {}: {}",
                &backend.server_name, _e
              );
              break;
            }
            Ok(server_config) => {
              hm_server_config.insert(server_name_bytes.to_vec(), Arc::new(server_config));
            }
          }
        }
      }
      if let Err(_e) = server_crypto_tx.send(Some(hm_server_config)) {
        error!("Failed to populate server crypto");
        break;
      }
      tokio::time::sleep(Duration::from_secs(CERTS_WATCH_DELAY_SECS.into())).await;
    }
  }

  // TCP Listener Service, i.e., http/2 and http/1.1
  async fn listener_service(
    &self,
    server: Http<LocalExecutor>,
    mut server_crypto_rx: watch::Receiver<Option<ServerCryptoMap>>,
  ) -> Result<()> {
    let tcp_listener = TcpListener::bind(&self.listening_on).await?;
    info!("Start TCP proxy serving with HTTPS request for configured host names");

    let mut server_crypto_map: Option<ServerCryptoMap> = None;
    loop {
      select! {
        tcp_cnx = tcp_listener.accept().fuse() => {
          // First check SNI
          let rustls_acceptor = rustls::server::Acceptor::new();
          if server_crypto_map.is_none() || tcp_cnx.is_err() || rustls_acceptor.is_err() {
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
          let server_crypto = server_crypto_map.as_ref().unwrap().get(server_name.as_bytes());
          if server_crypto.is_none() {
            debug!("No TLS serving app for {}", server_name);
            continue;
          };
          // Finally serve the TLS connection
          if let Ok(stream) = start.into_stream(server_crypto.unwrap().clone()).await {
            self.clone().client_serve(stream, server.clone(), _client_addr).await
          }
        }
        _ = server_crypto_rx.changed().fuse() => {
          if server_crypto_rx.borrow().is_none() {
            break;
          }
          server_crypto_map = server_crypto_rx.borrow().clone();
        }
        complete => break
      }
    }
    Ok(()) as Result<()>
  }

  #[cfg(feature = "h3")]
  async fn parse_sni_and_get_crypto_h3(
    &self,
    peeked_conn: &mut quinn::Connecting,
    server_crypto_map: &ServerCryptoMap,
  ) -> Option<Arc<ServerConfig>> {
    let hsd = if let Ok(h) = peeked_conn.handshake_data().await {
      h
    } else {
      return None;
    };
    let hsd_downcast = if let Ok(d) = hsd.downcast::<quinn::crypto::rustls::HandshakeData>() {
      d
    } else {
      return None;
    };
    let server_name = hsd_downcast.server_name?.to_ascii_lowercase();
    info!(
      "HTTP/3 connection incoming (SNI {:?}): Overwrite ServerConfig",
      server_name
    );
    server_crypto_map
      .get(&server_name.as_bytes().to_vec())
      .cloned()
  }

  #[cfg(feature = "h3")]
  async fn listener_service_h3(
    &self,
    mut server_crypto_rx: watch::Receiver<Option<ServerCryptoMap>>,
  ) -> Result<()> {
    // TODO: Work around to initially serve incoming connection
    // かなり適当。エラーが出たり出なかったり。原因がわからない…
    let next = self
      .backends
      .apps
      .iter()
      .filter(|&(_, backend)| {
        backend.tls_cert_key_path.is_some() && backend.tls_cert_path.is_some()
      })
      .map(|(name, _)| name)
      .next();
    ensure!(next.is_some(), "No TLS supported app");
    let initial_app_name = next.ok_or_else(|| anyhow!(""))?;
    debug!(
      "HTTP/3 SNI multiplexer initial app_name: {:?}",
      String::from_utf8(initial_app_name.to_vec())
    );
    let backend_serve = self
      .backends
      .apps
      .get(initial_app_name)
      .ok_or_else(|| anyhow!(""))?;

    let initial_server_crypto = backend_serve.update_server_config().await?;

    let server_config_h3 = quinn::ServerConfig::with_crypto(Arc::new(initial_server_crypto));
    let (endpoint, incoming) = quinn::Endpoint::server(server_config_h3, self.listening_on)?;
    info!("Start UDP proxy serving with HTTP/3 request for configured host names");

    let mut server_crypto_map: Option<ServerCryptoMap> = None;
    let mut p = incoming.peekable();
    loop {
      select! {
        // TODO: Not sure if this properly works to handle multiple "server_name"s to host multiple hosts.
        // peek() should work for that.
        peeked_conn = std::pin::Pin::new(&mut p).peek_mut().fuse() => {
          if server_crypto_map.is_none() || peeked_conn.is_none() {
            continue;
          }
          let peeked_conn = peeked_conn.unwrap();
          let is_acceptable =
            if let Some(new_server_crypto) = self.parse_sni_and_get_crypto_h3(peeked_conn, server_crypto_map.as_ref().unwrap()).await {
              // Set ServerConfig::set_server_config for given SNI
              endpoint.set_server_config(Some(quinn::ServerConfig::with_crypto(new_server_crypto)));
              true
            } else {
              false
            };
          // Then acquire actual connection
          let peekable_incoming = std::pin::Pin::new(&mut p);
          if let Some(conn) = peekable_incoming.get_mut().next().await {
            if is_acceptable {
              self.clone().client_serve_h3(conn).await;
            }
          } else {
            continue;
          }
        }
        _ = server_crypto_rx.changed().fuse() => {
          if server_crypto_rx.borrow().is_none() {
            break;
          }
          server_crypto_map = server_crypto_rx.borrow().clone();
        }
        complete => break
      }
    }
    endpoint.wait_idle().await;
    Ok(()) as Result<()>
  }

  pub async fn start_with_tls(self, server: Http<LocalExecutor>) -> Result<()> {
    let (tx, rx) = watch::channel::<Option<ServerCryptoMap>>(None);
    #[cfg(not(feature = "h3"))]
    {
      select! {
        _= self.cert_service(tx) => {
          error!("Cert service for TLS exited");
        },
        _ = self.listener_service(server, rx) => {
          error!("TCP proxy service for TLS exited");
        },
      };
      Ok(())
    }
    #[cfg(feature = "h3")]
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

        };
        Ok(())
      }
    }
  }
}
