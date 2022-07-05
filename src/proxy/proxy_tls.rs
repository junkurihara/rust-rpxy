use super::proxy_main::{LocalExecutor, Proxy};
use crate::{constants::CERTS_WATCH_DELAY_SECS, error::*, log::*};
#[cfg(feature = "h3")]
use futures::StreamExt;
use futures::{future::FutureExt, join, select};
use hyper::{client::connect::Connect, server::conn::Http};
use rustls::ServerConfig;
use std::{sync::Arc, time::Duration};
use tokio::net::TcpListener;

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub async fn start_with_tls(self, server: Http<LocalExecutor>) -> Result<()> {
    let cert_service = async {
      info!("Start cert watch service for {}", self.listening_on);
      loop {
        for (server_name, backend) in self.backends.apps.iter() {
          if backend.tls_cert_key_path.is_some() && backend.tls_cert_path.is_some() {
            if let Err(_e) = backend.update_server_config().await {
              warn!("Failed to update certs for {}", server_name);
            }
          }
        }
        tokio::time::sleep(Duration::from_secs(CERTS_WATCH_DELAY_SECS.into())).await;
      }
    };

    // TCP Listener Service, i.e., http/2 and http/1.1
    let listener_service = async {
      let tcp_listener = TcpListener::bind(&self.listening_on).await?;
      info!(
        "Start TCP proxy serving with HTTPS request for configured host names: {:?}",
        tcp_listener.local_addr()?
      );

      loop {
        select! {
          tcp_cnx = tcp_listener.accept().fuse() => {
            if tcp_cnx.is_err() {
              continue;
            }
            let (raw_stream, _client_addr) = tcp_cnx.unwrap();

            // First check SNI
            let rustls_acceptor = rustls::server::Acceptor::new().unwrap();
            let acceptor = tokio_rustls::LazyConfigAcceptor::new(rustls_acceptor, raw_stream).await;
            if acceptor.is_err() {
              continue;
            }
            let start = acceptor.unwrap();
            let client_hello = start.client_hello();
            debug!("SNI in ClientHello: {:?}", client_hello.server_name());
            // Find server config for given SNI
            let svn = if let Some(svn) = client_hello.server_name() {
              svn
            } else {
              info!("No SNI in ClientHello");
              continue;
            };
            let server_crypto = if let Some(p) = self.fetch_server_crypto(svn) {
              p
            } else {
              continue;
            };
            // Finally serve the TLS connection
            if let Ok(stream) = start.into_stream(Arc::new(server_crypto)).await {
              self.clone().client_serve(stream, server.clone(), _client_addr).await
            }
          }
          complete => break
        }
      }
      Ok(()) as Result<()>
    };

    ///////////////////////
    #[cfg(feature = "h3")]
    let listener_service_h3 = async {
      // TODO: Work around to initially serve incoming connection
      // かなり適当。エラーが出たり出なかったり。原因がわからない…
      let tls_app_names: Vec<String> = self
        .backends
        .apps
        .iter()
        .filter(|&(_, backend)| {
          backend.tls_cert_key_path.is_some() && backend.tls_cert_path.is_some()
        })
        .map(|(name, _)| name.to_string())
        .collect();
      ensure!(!tls_app_names.is_empty(), "No TLS supported app");
      let initial_app_name = tls_app_names.get(0).unwrap().as_str();
      debug!(
        "HTTP/3 SNI multiplexer initial app_name: {}",
        initial_app_name
      );
      let backend_serve = self.backends.apps.get(initial_app_name).unwrap();
      let server_crypto = backend_serve.get_tls_server_config().unwrap();
      let server_config_h3 = quinn::ServerConfig::with_crypto(Arc::new(server_crypto));

      let (endpoint, incoming) =
        quinn::Endpoint::server(server_config_h3, self.listening_on).unwrap();
      info!(
        "Start UDP proxy serving with HTTP/3 request for configured host names: {:?}",
        endpoint.local_addr()?
      );

      let mut p = incoming.peekable();
      loop {
        // TODO: Not sure if this properly works to handle multiple "server_name"s to host multiple hosts.
        // peek() should work for that.
        if let Some(peeked_conn) = std::pin::Pin::new(&mut p).peek_mut().await {
          let hsd = peeked_conn.handshake_data().await;
          let hsd_downcast = hsd?
            .downcast::<quinn::crypto::rustls::HandshakeData>()
            .unwrap();
          let svn = if let Some(sni) = hsd_downcast.server_name {
            sni
          } else {
            debug!("HTTP/3 no SNI is given");
            continue;
          };
          let new_server_crypto = if let Some(p) = self.fetch_server_crypto(&svn) {
            p
          } else {
            continue;
          };
          // Set ServerConfig::set_server_config for given SNI
          let mut new_server_config_h3 =
            quinn::ServerConfig::with_crypto(Arc::new(new_server_crypto));
          if svn == "localhost" {
            new_server_config_h3.concurrent_connections(512);
          }
          info!(
            "HTTP/3 connection incoming (SNI {:?}): Overwrite ServerConfig",
            svn
          );
          endpoint.set_server_config(Some(new_server_config_h3));
        }

        // Then acquire actual connection
        let peekable_incoming = std::pin::Pin::new(&mut p);
        if let Some(conn) = peekable_incoming.get_mut().next().await {
          let fut = self.clone().client_serve_h3(conn);
          self.globals.runtime_handle.spawn(async {
            if let Err(e) = fut.await {
              warn!("QUIC or HTTP/3 connection failed: {}", e)
            }
          });
        } else {
          break;
        }
      }
      endpoint.wait_idle().await;
      Ok(()) as Result<()>
    };

    #[cfg(not(feature = "h3"))]
    {
      join!(listener_service, cert_service).0
    }
    #[cfg(feature = "h3")]
    {
      if self.globals.http3 {
        join!(listener_service, cert_service, listener_service_h3).0
      } else {
        join!(listener_service, cert_service).0
      }
    }
  }

  fn fetch_server_crypto(&self, server_name: &str) -> Option<ServerConfig> {
    let backend_serve = if let Some(backend_serve) = self.backends.apps.get(server_name) {
      backend_serve
    } else {
      warn!(
        "No configuration for the server name {} given in client_hello",
        server_name
      );
      return None;
    };

    if backend_serve.tls_cert_path.is_none() {
      // at least cert does exit
      warn!("SNI indicates a site that doesn't support TLS.");
      return None;
    }
    if let Some(p) = backend_serve.get_tls_server_config() {
      Some(p)
    } else {
      error!("Failed to load server config");
      None
    }
  }
}
