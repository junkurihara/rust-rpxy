use super::socket::bind_tcp_socket;
use crate::{error::RpxyResult, globals::Globals, log::*};
use hot_reload::{ReloaderReceiver, ReloaderService};
use hyper_util::server::conn::auto::Builder as ConnectionBuilder;
use std::{net::SocketAddr, sync::Arc};

/// Proxy main object responsible to serve requests received from clients at the given socket address.
pub(crate) struct Proxy<E> {
  /// global context shared among async tasks
  pub globals: Arc<Globals>,
  /// listen socket address
  pub listening_on: SocketAddr,
  /// whether TLS is enabled or not
  pub tls_enabled: bool,
  /// hyper connection builder serving http request
  pub connection_builder: Arc<ConnectionBuilder<E>>,
}

impl<E> Proxy<E> {
  /// Start without TLS (HTTP cleartext)
  async fn start_without_tls(&self) -> RpxyResult<()> {
    let listener_service = async {
      let tcp_socket = bind_tcp_socket(&self.listening_on)?;
      let tcp_listener = tcp_socket.listen(self.globals.proxy_config.tcp_listen_backlog)?;
      info!("Start TCP proxy serving with HTTP request for configured host names");
      while let Ok((stream, client_addr)) = tcp_listener.accept().await {
        //   self.serve_connection(TokioIo::new(stream), client_addr, None);
      }
      Ok(()) as RpxyResult<()>
    };
    listener_service.await?;
    Ok(())
  }

  /// Start with TLS (HTTPS)
  pub(super) async fn start_with_tls(&self) -> RpxyResult<()> {
    // let (cert_reloader_service, cert_reloader_rx) = ReloaderService::<CryptoReloader<U>, ServerCryptoBase>::new(
    //   &self.globals.clone(),
    //   CERTS_WATCH_DELAY_SECS,
    //   !LOAD_CERTS_ONLY_WHEN_UPDATED,
    // )
    // .await
    // .map_err(|e| anyhow::anyhow!(e))?;
    loop {}
    Ok(())
  }

  /// Entrypoint for HTTP/1.1, 2 and 3 servers
  pub async fn start(&self) -> RpxyResult<()> {
    let proxy_service = async {
      if self.tls_enabled {
        self.start_with_tls().await
      } else {
        self.start_without_tls().await
      }
    };

    match &self.globals.term_notify {
      Some(term) => {
        tokio::select! {
          _ = proxy_service => {
            warn!("Proxy service got down");
          }
          _ = term.notified() => {
            info!("Proxy service listening on {} receives term signal", self.listening_on);
          }
        }
      }
      None => {
        proxy_service.await?;
        warn!("Proxy service got down");
      }
    }

    Ok(())
  }
}
