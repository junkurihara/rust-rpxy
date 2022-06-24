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
  Body, Client, HeaderMap, Method, Request, Response, StatusCode, Uri,
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
  tls_enabled: bool,
  globals: Arc<Globals>,
  backends: Arc<HashMap<String, Backend>>,
) -> Result<Response<Body>> {
  let headers = req.headers();

  // Here we start to handle with hostname
  // Find backend application for given hostname
  let (hostname, port) = parse_hostname_port(headers, tls_enabled)?;
  let path = req.uri().path();
  let backend = if let Some(be) = backends.get(hostname.as_str()) {
    be
  } else {
    return http_error(StatusCode::SERVICE_UNAVAILABLE);
  };

  // Redirect to https if tls_enabled is false and redirect_to_https is true
  if !tls_enabled && backend.redirect_to_https.unwrap_or(false) {
    if let Some(https_port) = globals.https_port {
      let dest = if https_port == 443 {
        format!("https://{}{}", hostname, path)
      } else {
        format!(
          "https://{}:{}{}",
          hostname,
          globals.https_port.unwrap(),
          path
        )
      };
      return https_redirection(dest);
    } else {
      return http_error(StatusCode::SERVICE_UNAVAILABLE);
    }
  }

  // Find reverse proxy for given path
  // let destination_uri = if backend.reverse_proxy.destination_uris.is_some() {
  //   if let (b) = backend.re
  // } else {
  //   backend.reverse_proxy.default_destination_uri.clone();
  // };

  debug!("path: {}", req.uri().path());
  // if req.version() == hyper::Version::HTTP_11 {
  //   Ok(Response::new(Body::from("Hello World")))
  // } else {
  // Note: it's usually better to return a Response
  // with an appropriate StatusCode instead of an Err.
  // Err("not HTTP/1.1, abort connection")
  // http_error(StatusCode::NOT_FOUND)
  https_redirection("https://www.google.com/".to_string())
  // }
  // });
}

fn http_error(status_code: StatusCode) -> Result<Response<Body>> {
  let response = Response::builder()
    .status(status_code)
    .body(Body::empty())
    .unwrap();
  Ok(response)
}

fn https_redirection(redirect_to: String) -> Result<Response<Body>> {
  let response = Response::builder()
    .status(StatusCode::MOVED_PERMANENTLY)
    .header("Location", redirect_to)
    .body(Body::empty())
    .unwrap();
  Ok(response)
}

fn parse_hostname_port(headers: &HeaderMap, tls_enabled: bool) -> Result<(String, u16)> {
  let hostname_port = headers
    .get("host")
    .ok_or_else(|| anyhow!("No host in request header"))?;
  let hp_as_uri = hostname_port.to_str().unwrap().parse::<Uri>().unwrap();

  let hostname = hp_as_uri
    .host()
    .ok_or_else(|| anyhow!("Failed to parse hostname"))?;

  let port = if let Some(p) = hp_as_uri.port() {
    p.as_u16()
  } else if tls_enabled {
    443
  } else {
    80
  };

  Ok((hostname.to_string(), port))
}
