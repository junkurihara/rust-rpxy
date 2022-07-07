use super::proxy_main::{LocalExecutor, Proxy};
use crate::{constants::*, error::*, log::*};
#[cfg(feature = "h3")]
use futures::StreamExt;
use futures::{future::FutureExt, select};
use hyper::{client::connect::Connect, server::conn::Http};
use rustls::ServerConfig;
use std::sync::Arc;
use tokio::{net::TcpListener, time::Duration};

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub async fn cert_service(&self) {
    info!("Start cert watch service");
    loop {
      for (server_name, backend) in self.backends.apps.iter() {
        if backend.tls_cert_key_path.is_some() && backend.tls_cert_path.is_some() {
          if let Err(_e) = backend.update_server_config().await {
            warn!("Failed to update certs for {}: {}", server_name, _e);
          }
        }
      }
      tokio::time::sleep(Duration::from_secs(CERTS_WATCH_DELAY_SECS.into())).await;
    }
  }

  // TCP Listener Service, i.e., http/2 and http/1.1
  pub async fn listener_service(&self, server: Http<LocalExecutor>) -> Result<()> {
    let tcp_listener = TcpListener::bind(&self.listening_on).await?;
    info!("Start TCP proxy serving with HTTPS request for configured host names");

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
  }

  #[cfg(feature = "h3")]
  async fn parse_sni_and_get_config_h3(
    &self,
    peeked_conn: &mut quinn::Connecting,
  ) -> Option<quinn::ServerConfig> {
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
    let server_name = hsd_downcast.server_name?;
    info!(
      "HTTP/3 connection incoming (SNI {:?}): Overwrite ServerConfig",
      server_name
    );
    let new_server_crypto = self.fetch_server_crypto(&server_name)?;
    Some(quinn::ServerConfig::with_crypto(Arc::new(
      new_server_crypto,
    )))
  }

  #[cfg(feature = "h3")]
  pub async fn listener_service_h3(&self) -> Result<()> {
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
    let initial_app_name = tls_app_names.get(0).ok_or_else(|| anyhow!(""))?.as_str();
    debug!(
      "HTTP/3 SNI multiplexer initial app_name: {}",
      initial_app_name
    );
    let backend_serve = self
      .backends
      .apps
      .get(initial_app_name)
      .ok_or_else(|| anyhow!(""))?;
    while backend_serve.get_tls_server_config().is_none() {
      tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let server_crypto = backend_serve
      .get_tls_server_config()
      .ok_or_else(|| anyhow!(""))?;
    let server_config_h3 = quinn::ServerConfig::with_crypto(Arc::new(server_crypto));

    let (endpoint, incoming) = quinn::Endpoint::server(server_config_h3, self.listening_on)?;
    info!("Start UDP proxy serving with HTTP/3 request for configured host names");

    let mut p = incoming.peekable();
    loop {
      // TODO: Not sure if this properly works to handle multiple "server_name"s to host multiple hosts.
      // peek() should work for that.
      let peeked_conn = std::pin::Pin::new(&mut p)
        .peek_mut()
        .await
        .ok_or_else(|| anyhow!("Failed to peek"))?;
      let is_acceptable =
        if let Some(new_server_config) = self.parse_sni_and_get_config_h3(peeked_conn).await {
          // Set ServerConfig::set_server_config for given SNI
          endpoint.set_server_config(Some(new_server_config));
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
        break;
      }
    }
    endpoint.wait_idle().await;
    Ok(()) as Result<()>
  }

  pub async fn start_with_tls(self, server: Http<LocalExecutor>) -> Result<()> {
    #[cfg(not(feature = "h3"))]
    {
      select! {
        _= cert_service => {
          error!("Cert service for TLS exited");
        },
        _ = listener_service => {
          error!("TCP proxy service for TLS exited");
        },

      };
      Ok(())
    }
    #[cfg(feature = "h3")]
    {
      if self.globals.http3 {
        tokio::select! {
          _= self.cert_service() => {
            error!("Cert service for TLS exited");
          },
          _ = self.listener_service(server) => {
            error!("TCP proxy service for TLS exited");
          },
          _= self.listener_service_h3() => {
            error!("UDP proxy service for QUIC exited");
          },
        };
        Ok(())
      } else {
        tokio::select! {
          _= self.cert_service() => {
            error!("Cert service for TLS exited");
          },
          _ = self.listener_service(server) => {
            error!("TCP proxy service for TLS exited");
          },

        };
        Ok(())
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
