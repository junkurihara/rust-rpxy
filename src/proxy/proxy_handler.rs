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

// TODO: ここでbackendの名前単位でリクエストを分岐させる
pub async fn handle_request(
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

#[allow(clippy::unnecessary_wraps)]
fn http_error(status_code: StatusCode) -> Result<Response<Body>, http::Error> {
  let response = Response::builder()
    .status(status_code)
    .body(Body::empty())
    .unwrap();
  Ok(response)
}
