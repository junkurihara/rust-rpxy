use super::proxy_main::{LocalExecutor, Proxy};
use crate::{constants::CERTS_WATCH_DELAY_SECS, error::*, log::*};
use futures::{future::FutureExt, join, select};
use hyper::{client::connect::Connect, server::conn::Http};
use std::{sync::Arc, time::Duration};
use tokio::net::TcpListener;

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub async fn start_with_tls(
    self,
    listener: TcpListener,
    server: Http<LocalExecutor>,
  ) -> Result<()> {
    let cert_service = async {
      info!("Start cert watch service for {}", self.listening_on);
      loop {
        for (hostname, backend) in self.backends.iter() {
          if backend.tls_cert_key_path.is_some() && backend.tls_cert_path.is_some() {
            if let Err(_e) = backend.update_server_config().await {
              warn!("Failed to update certs for {}", hostname);
            }
          }
        }
        tokio::time::sleep(Duration::from_secs(CERTS_WATCH_DELAY_SECS.into())).await;
      }
    };

    let listener_service = async {
      loop {
        select! {
          tcp_cnx = listener.accept().fuse() => {
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
            let backend_serve = if let Some(backend_serve) = self.backends.get(svn){
              backend_serve
            } else {
              info!("No configuration for the server name {} given in client_hello", svn);
              continue;
            };
            let server_config = backend_serve.get_tls_server_config();
            // Finally serve the TLS connection
            if let Ok(stream) = start.into_stream(Arc::new(server_config.unwrap())).await {
              self.clone().client_serve(stream, server.clone(), _client_addr).await
            }
          }
          complete => break
        }
      }
      Ok(()) as Result<()>
    };

    join!(listener_service, cert_service).0
  }
}
