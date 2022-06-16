use crate::{error::*, globals::Globals, log::*};

use futures::{
  task::{Context, Poll},
  Future,
};
use hyper::http;
use hyper::server::conn::Http;
use hyper::{Body, HeaderMap, Method, Request, Response, StatusCode};
use std::{net::SocketAddr, pin::Pin, sync::Arc};
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
pub struct PacketAcceptor {
  pub listening_on: SocketAddr,
  pub globals: Arc<Globals>,
}

#[allow(clippy::type_complexity)]
impl hyper::service::Service<http::Request<Body>> for PacketAcceptor {
  type Response = Response<Body>;

  type Error = http::Error;
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

  fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
    Poll::Ready(Ok(()))
  }

  fn call(&mut self, req: Request<Body>) -> Self::Future {
    debug!("\nserve:{:?}\n{:?}", self.listening_on, req);
    // let globals = &self.doh.globals;
    // let self_inner = self.clone();
    // if req.uri().path() == globals.path {
    //   Box::pin(async move {
    //     let mut subscriber = None;
    //     if self_inner.doh.globals.enable_auth_target {
    //       subscriber = match auth::authenticate(
    //         &self_inner.doh.globals,
    //         &req,
    //         ValidationLocation::Target,
    //         &self_inner.peer_addr,
    //       ) {
    //         Ok((sub, aud)) => {
    //           debug!("Valid token or allowed ip: sub={:?}, aud={:?}", &sub, &aud);
    //           sub
    //         }
    //         Err(e) => {
    //           error!("{:?}", e);
    //           return Ok(e);
    //         }
    //       };
    //     }
    //     match *req.method() {
    //       Method::POST => self_inner.doh.serve_post(req, subscriber).await,
    //       Method::GET => self_inner.doh.serve_get(req, subscriber).await,
    //       _ => http_error(StatusCode::METHOD_NOT_ALLOWED),
    //     }
    //   })
    // } else if req.uri().path() == globals.odoh_configs_path {
    //   match *req.method() {
    //     Method::GET => Box::pin(async move { self_inner.doh.serve_odoh_configs().await }),
    //     _ => Box::pin(async { http_error(StatusCode::METHOD_NOT_ALLOWED) }),
    //   }
    // } else {
    //   #[cfg(not(feature = "odoh-proxy"))]
    //   {
    //     Box::pin(async { http_error(StatusCode::NOT_FOUND) })
    //   }
    //   #[cfg(feature = "odoh-proxy")]
    //   {
    //     if req.uri().path() == globals.odoh_proxy_path {
    //       Box::pin(async move {
    //         let mut subscriber = None;
    //         if self_inner.doh.globals.enable_auth_proxy {
    //           subscriber = match auth::authenticate(
    //             &self_inner.doh.globals,
    //             &req,
    //             ValidationLocation::Proxy,
    //             &self_inner.peer_addr,
    //           ) {
    //             Ok((sub, aud)) => {
    //               debug!("Valid token or allowed ip: sub={:?}, aud={:?}", &sub, &aud);
    //               sub
    //             }
    //             Err(e) => {
    //               error!("{:?}", e);
    //               return Ok(e);
    //             }
    //           };
    //         }
    //         // Draft:        https://datatracker.ietf.org/doc/html/draft-pauly-dprive-oblivious-doh-11
    //         // Golang impl.: https://github.com/cloudflare/odoh-server-go
    //         // Based on the draft and Golang implementation, only post method is allowed.
    //         match *req.method() {
    //           Method::POST => self_inner.doh.serve_odoh_proxy_post(req, subscriber).await,
    //           _ => http_error(StatusCode::METHOD_NOT_ALLOWED),
    //         }
    //       })
    //     } else {
    Box::pin(async { http_error(StatusCode::NOT_FOUND) })
    //     }
    // }
    // }
  }
}

impl PacketAcceptor {
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
        server.serve_connection(stream, self),
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

    let tls_enabled: bool;
    #[cfg(not(feature = "tls"))]
    {
      tls_enabled = false;
    }
    #[cfg(feature = "tls")]
    {
      tls_enabled =
        self.globals.tls_cert_path.is_some() && self.globals.tls_cert_key_path.is_some();
    }
    if tls_enabled {
      info!(
        "Start server listening on TCP with TLS: {:?}",
        tcp_listener.local_addr()?
      );
      #[cfg(feature = "tls")]
      self.start_with_tls(tcp_listener, server).await?;
    } else {
      info!(
        "Start server listening on TCP: {:?}",
        tcp_listener.local_addr()?
      );
      self.start_without_tls(tcp_listener, server).await?;
    }

    Ok(())
  }
}
