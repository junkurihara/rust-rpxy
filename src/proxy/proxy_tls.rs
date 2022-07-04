use super::proxy_main::{LocalExecutor, Proxy};
use crate::{constants::CERTS_WATCH_DELAY_SECS, error::*, log::*};
#[cfg(feature = "h3")]
use futures::StreamExt;
use futures::{future::FutureExt, join, select};
use hyper::{client::connect::Connect, server::conn::Http};
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
            let backend_serve = if let Some(backend_serve) = self.backends.apps.get(svn){
              backend_serve
            } else {
              info!("No configuration for the server name {} given in client_hello", svn);
              continue;
            };

            if backend_serve.tls_cert_path.is_none() { // at least cert does exit
              debug!("SNI indicates a site that doesn't support TLS.");
              continue;
            }
            let server_config = if let Some(p) = backend_serve.get_tls_server_config(){
              p
            } else {
              error!("Failed to load server config");
              continue;
            };
            // Finally serve the TLS connection
            if let Ok(stream) = start.into_stream(Arc::new(server_config)).await {
              self.clone().client_serve(stream, server.clone(), _client_addr).await
            }
          }
          complete => break
        }
      }
      Ok(()) as Result<()>
    };

    /////////////////////// TODO:!!!!!
    #[cfg(feature = "h3")]
    let listener_service_h3 = async {
      // TODO: とりあえずデフォルトのserver_cryptoが必要になりそう
      let backend_serve = self.backends.apps.get("localhost").unwrap();
      let server_crypto = backend_serve.get_tls_server_config().unwrap();
      let server_config_h3 = quinn::ServerConfig::with_crypto(Arc::new(server_crypto));

      let (endpoint, mut incoming) =
        quinn::Endpoint::server(server_config_h3, self.listening_on).unwrap();
      debug!("HTTP/3 UDP listening on {}", endpoint.local_addr().unwrap());

      while let Some(mut conn) = incoming.next().await {
        debug!("HTTP/3 connection incoming");
        let hsd = conn.handshake_data().await;
        let hsd_downcast = hsd
          .unwrap()
          .downcast::<quinn::crypto::rustls::HandshakeData>()
          .unwrap();
        debug!("HTTP/3 SNI: {:?}", hsd_downcast.server_name);
        // TODO: ServerConfig::set_server_configでSNIに応じて再セット

        let fut = self.clone().client_serve_h3(conn);
        self.globals.runtime_handle.spawn(async {
          if let Err(e) = fut.await {
            error!("connection failed: {reason}", reason = e.to_string())
          }
        });
      }
    };

    #[cfg(not(feature = "h3"))]
    {
      join!(listener_service, cert_service).0
    }
    #[cfg(feature = "h3")]
    {
      join!(listener_service, cert_service, listener_service_h3).0
    }
  }
}
