use crate::{backend::Backend, error::*, globals::Globals, log::*};
use futures::{
  select,
  task::{Context, Poll},
  Future, FutureExt,
};
use hyper::{
  client::connect::Connect,
  http,
  server::conn::Http,
  service::{service_fn, Service},
  Body, Client, HeaderMap, Method, Request, Response, StatusCode,
};
use std::{collections::HashMap, net::SocketAddr, pin::Pin, sync::Arc};
use tokio::{
  io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
  net::TcpListener,
  runtime::Handle,
  time::Duration,
};

#[allow(clippy::unnecessary_wraps)]
fn http_error(status_code: StatusCode) -> Result<Response<Body>, http::Error> {
  let response = Response::builder()
    .status(status_code)
    .body(Body::empty())
    .unwrap();
  Ok(response)
}

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
  pub tls_enabled: bool,                       // TCP待受がTLSかどうか
  pub backends: Arc<HashMap<String, Backend>>, // TODO: hyper::uriで抜いたhostnameで引っ掛ける。Stringでいいのか？
  pub forwarder: Arc<Client<T>>,
  pub globals: Arc<Globals>,
}

// TODO: ここでbackendの名前単位でリクエストを分岐させる
async fn handle_request(
  req: Request<Body>,
  client_ip: SocketAddr,
  globals: Arc<Globals>,
) -> Result<Response<Body>, http::Error> {
  // http_error(StatusCode::NOT_FOUND)
  debug!("{:?}", req);
  // if req.version() == hyper::Version::HTTP_11 {
  //   Ok(Response::new(Body::from("Hello World")))
  // } else {
  // Note: it's usually better to return a Response
  // with an appropriate StatusCode instead of an Err.
  // Err("not HTTP/1.1, abort connection")
  http_error(StatusCode::NOT_FOUND)
  // }
  // });
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
        // server.serve_connection(stream, self),
        server.serve_connection(
          stream,
          service_fn(move |req: Request<Body>| {
            handle_request(req, peer_addr, self.globals.clone())
          }),
        ),
      )
      .await
      .ok();

      clients_count.decrement();
    });
  }

  async fn start_without_tls(
    self,
    listener: TcpListener,
    server: Http<LocalExecutor>,
  ) -> Result<()> {
    let listener_service = async {
      while let Ok((stream, _client_addr)) = listener.accept().await {
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
    let tcp_listener = TcpListener::bind(&self.listening_on).await?;

    let mut server = Http::new();
    server.http1_keep_alive(self.globals.keepalive);
    server.http2_max_concurrent_streams(self.globals.max_concurrent_streams);
    server.pipeline_flush(true);
    let executor = LocalExecutor::new(self.globals.runtime_handle.clone());
    let server = server.with_executor(executor);

    if self.tls_enabled {
      info!(
        "Start TCP proxy serving with HTTPS request for configured host names: {:?}",
        tcp_listener.local_addr()?
      );
      // #[cfg(feature = "tls")]
      self.start_with_tls(tcp_listener, server).await?;
    } else {
      info!(
        "Start TCP proxy serving with HTTP request for configured host names: {:?}",
        tcp_listener.local_addr()?
      );
      self.start_without_tls(tcp_listener, server).await?;
    }

    Ok(())
  }
}
