use super::socket::bind_tcp_socket;
use crate::{
  certs::CryptoSource, error::*, globals::Globals, handler::HttpMessageHandler, log::*, utils::ServerNameBytesExp,
};
use derive_builder::{self, Builder};
use hyper::{client::connect::Connect, server::conn::Http, service::service_fn, Body, Request};
use std::{net::SocketAddr, sync::Arc};
use tokio::{
  io::{AsyncRead, AsyncWrite},
  runtime::Handle,
  time::{timeout, Duration},
};

#[derive(Clone)]
pub struct LocalExecutor {
  runtime_handle: Handle,
}

impl LocalExecutor {
  fn new(runtime_handle: Handle) -> Self {
    LocalExecutor { runtime_handle }
  }
}

impl<F> hyper::rt::Executor<F> for LocalExecutor
where
  F: std::future::Future + Send + 'static,
  F::Output: Send,
{
  fn execute(&self, fut: F) {
    self.runtime_handle.spawn(fut);
  }
}

#[derive(Clone, Builder)]
pub struct Proxy<T, U>
where
  T: Connect + Clone + Sync + Send + 'static,
  U: CryptoSource + Clone + Sync + Send + 'static,
{
  pub listening_on: SocketAddr,
  pub tls_enabled: bool, // TCP待受がTLSかどうか
  pub msg_handler: HttpMessageHandler<T, U>,
  pub globals: Arc<Globals<U>>,
}

impl<T, U> Proxy<T, U>
where
  T: Connect + Clone + Sync + Send + 'static,
  U: CryptoSource + Clone + Sync + Send,
{
  pub(super) fn client_serve<I>(
    self,
    stream: I,
    server: Http<LocalExecutor>,
    peer_addr: SocketAddr,
    tls_server_name: Option<ServerNameBytesExp>,
  ) where
    I: AsyncRead + AsyncWrite + Send + Unpin + 'static,
  {
    let request_count = self.globals.request_count.clone();
    if request_count.increment() > self.globals.proxy_config.max_clients {
      request_count.decrement();
      return;
    }
    debug!("Request incoming: current # {}", request_count.current());

    self.globals.runtime_handle.clone().spawn(async move {
      timeout(
        self.globals.proxy_config.proxy_timeout + Duration::from_secs(1),
        server
          .serve_connection(
            stream,
            service_fn(move |req: Request<Body>| {
              self.msg_handler.clone().handle_request(
                req,
                peer_addr,
                self.listening_on,
                self.tls_enabled,
                tls_server_name.clone(),
              )
            }),
          )
          .with_upgrades(),
      )
      .await
      .ok();

      request_count.decrement();
      debug!("Request processed: current # {}", request_count.current());
    });
  }

  async fn start_without_tls(self, server: Http<LocalExecutor>) -> Result<()> {
    let listener_service = async {
      let tcp_socket = bind_tcp_socket(&self.listening_on)?;
      let tcp_listener = tcp_socket.listen(self.globals.proxy_config.tcp_listen_backlog)?;
      // let tcp_listener = TcpListener::bind(&self.listening_on).await?;
      info!("Start TCP proxy serving with HTTP request for configured host names");
      while let Ok((stream, _client_addr)) = tcp_listener.accept().await {
        self.clone().client_serve(stream, server.clone(), _client_addr, None);
      }
      Ok(()) as Result<()>
    };
    listener_service.await?;
    Ok(())
  }

  pub async fn start(self) -> Result<()> {
    let mut server = Http::new();
    server.http1_keep_alive(self.globals.proxy_config.keepalive);
    server.http2_max_concurrent_streams(self.globals.proxy_config.max_concurrent_streams);
    server.pipeline_flush(true);
    let executor = LocalExecutor::new(self.globals.runtime_handle.clone());
    let server = server.with_executor(executor);

    if self.tls_enabled {
      self.start_with_tls(server).await?;
    } else {
      self.start_without_tls(server).await?;
    }

    Ok(())
  }
}
