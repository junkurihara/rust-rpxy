// use super::proxy_handler::handle_request;
use crate::{backend::Backends, error::*, globals::Globals, log::*};
use hyper::{
  client::connect::Connect, server::conn::Http, service::service_fn, Body, Client, Request,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::{
  io::{AsyncRead, AsyncWrite},
  net::TcpListener,
  runtime::Handle,
  time::Duration,
};

#[derive(Clone, Debug)]
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

#[derive(Clone)]
pub struct Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub listening_on: SocketAddr,
  pub tls_enabled: bool, // TCP待受がTLSかどうか
  pub backends: Arc<Backends>,
  pub forwarder: Arc<Client<T>>,
  pub globals: Arc<Globals>,
}

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub async fn client_serve<I>(self, stream: I, server: Http<LocalExecutor>, peer_addr: SocketAddr)
  where
    I: AsyncRead + AsyncWrite + Send + Unpin + 'static,
  {
    let clients_count = self.globals.clients_count.clone();
    if clients_count.increment() > self.globals.max_clients {
      clients_count.decrement();
      return;
    }

    self.globals.runtime_handle.clone().spawn(async move {
      tokio::time::timeout(
        self.globals.timeout + Duration::from_secs(1),
        server
          .serve_connection(
            stream,
            service_fn(move |req: Request<Body>| self.clone().handle_request(req, peer_addr)),
          )
          .with_upgrades(),
      )
      .await
      .ok();

      clients_count.decrement();
      debug!("Client #: {}", clients_count.current());
    });
  }

  async fn start_without_tls(self, server: Http<LocalExecutor>) -> Result<()> {
    let listener_service = async {
      let tcp_listener = TcpListener::bind(&self.listening_on).await?;
      info!(
        "Start TCP proxy serving with HTTP request for configured host names: {:?}",
        tcp_listener.local_addr()?
      );
      while let Ok((stream, _client_addr)) = tcp_listener.accept().await {
        self
          .clone()
          .client_serve(stream, server.clone(), _client_addr)
          .await;
      }
      Ok(()) as Result<()>
    };
    listener_service.await?;
    Ok(())
  }

  pub async fn start(self) -> Result<()> {
    let mut server = Http::new();
    server.http1_keep_alive(self.globals.keepalive);
    server.http2_max_concurrent_streams(self.globals.max_concurrent_streams);
    server.pipeline_flush(true);
    let executor = LocalExecutor::new(self.globals.runtime_handle.clone());
    let server = server.with_executor(executor);

    if self.tls_enabled {
      // #[cfg(feature = "tls")]
      self.start_with_tls(server).await?;
    } else {
      self.start_without_tls(server).await?;
    }

    Ok(())
  }
}
