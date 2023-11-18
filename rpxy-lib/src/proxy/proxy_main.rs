use super::{passthrough_response, socket::bind_tcp_socket, synthetic_error_response, EitherBody};
use crate::{
  certs::CryptoSource, error::*, globals::Globals, handler::HttpMessageHandler, hyper_executor::LocalExecutor, log::*,
  utils::ServerNameBytesExp,
};
use derive_builder::{self, Builder};
use http::{Request, StatusCode};
use hyper::{
  body::Incoming,
  rt::{Read, Write},
  service::service_fn,
};
use hyper_util::{client::legacy::connect::Connect, rt::TokioIo, server::conn::auto::Builder as ConnectionBuilder};
use std::{net::SocketAddr, sync::Arc};
use tokio::time::{timeout, Duration};

#[derive(Clone, Builder)]
/// Proxy main object
pub struct Proxy<U>
where
  // T: Connect + Clone + Sync + Send + 'static,
  U: CryptoSource + Clone + Sync + Send + 'static,
{
  pub listening_on: SocketAddr,
  pub tls_enabled: bool, // TCP待受がTLSかどうか
  /// hyper server receiving http request
  pub http_server: Arc<ConnectionBuilder<LocalExecutor>>,
  // pub msg_handler: Arc<HttpMessageHandler<U>>,
  pub msg_handler: Arc<HttpMessageHandler<U>>,
  pub globals: Arc<Globals<U>>,
}

/// Wrapper function to handle request
async fn serve_request<U>(
  req: Request<Incoming>,
  // handler: Arc<HttpMessageHandler<T, U>>,
  handler: Arc<HttpMessageHandler<U>>,
  client_addr: SocketAddr,
  listen_addr: SocketAddr,
  tls_enabled: bool,
  tls_server_name: Option<ServerNameBytesExp>,
) -> Result<hyper::Response<EitherBody>>
where
  U: CryptoSource + Clone + Sync + Send + 'static,
{
  match handler
    .handle_request(req, client_addr, listen_addr, tls_enabled, tls_server_name)
    .await?
  {
    Ok(res) => passthrough_response(res),
    Err(e) => synthetic_error_response(StatusCode::from(e)),
  }
}

impl<U> Proxy<U>
where
  // T: Connect + Clone + Sync + Send + 'static,
  U: CryptoSource + Clone + Sync + Send,
{
  /// Serves requests from clients
  pub(super) fn serve_connection<I>(
    &self,
    stream: I,
    peer_addr: SocketAddr,
    tls_server_name: Option<ServerNameBytesExp>,
  ) where
    I: Read + Write + Send + Unpin + 'static,
  {
    let request_count = self.globals.request_count.clone();
    if request_count.increment() > self.globals.proxy_config.max_clients {
      request_count.decrement();
      return;
    }
    debug!("Request incoming: current # {}", request_count.current());

    let server_clone = self.http_server.clone();
    let msg_handler_clone = self.msg_handler.clone();
    let timeout_sec = self.globals.proxy_config.proxy_timeout;
    let tls_enabled = self.tls_enabled;
    let listening_on = self.listening_on;
    self.globals.runtime_handle.clone().spawn(async move {
      timeout(
        timeout_sec + Duration::from_secs(1),
        server_clone.serve_connection_with_upgrades(
          stream,
          service_fn(move |req: Request<Incoming>| {
            serve_request(
              req,
              msg_handler_clone.clone(),
              peer_addr,
              listening_on,
              tls_enabled,
              tls_server_name.clone(),
            )
          }),
        ),
      )
      .await
      .ok();

      request_count.decrement();
      debug!("Request processed: current # {}", request_count.current());
    });
  }

  /// Start without TLS (HTTP cleartext)
  async fn start_without_tls(&self) -> Result<()> {
    let listener_service = async {
      let tcp_socket = bind_tcp_socket(&self.listening_on)?;
      let tcp_listener = tcp_socket.listen(self.globals.proxy_config.tcp_listen_backlog)?;
      info!("Start TCP proxy serving with HTTP request for configured host names");
      while let Ok((stream, client_addr)) = tcp_listener.accept().await {
        self.serve_connection(TokioIo::new(stream), client_addr, None);
      }
      Ok(()) as Result<()>
    };
    listener_service.await?;
    Ok(())
  }

  /// Entrypoint for HTTP/1.1 and HTTP/2 servers
  pub async fn start(&self) -> Result<()> {
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
