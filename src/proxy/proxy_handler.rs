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
  debug!("req: {:?}", req);
  // Here we start to handle with hostname
  // Find backend application for given hostname
  let (hostname, _port) = parse_hostname_port(&req, tls_enabled)?;
  let path = req.uri().path();
  let path_and_query = req.uri().path_and_query().unwrap().as_str();
  println!("{:?}", path_and_query);
  let backend = if let Some(be) = backends.get(hostname.as_str()) {
    be
  } else {
    return http_error(StatusCode::SERVICE_UNAVAILABLE);
  };

  // Redirect to https if tls_enabled is false and redirect_to_https is true
  if !tls_enabled && backend.redirect_to_https.unwrap_or(false) {
    debug!("Redirect to https: {}", hostname);
    return https_redirection(hostname, globals.https_port, path_and_query);
  }

  // Find reverse proxy for given path
  let destination_uri = if let Some(uri) = backend.reverse_proxy.destination_uris.get(path) {
    uri.to_owned()
  } else {
    backend.reverse_proxy.default_destination_uri.clone()
  };

  debug!("destination_uri: {}", destination_uri);
  // if req.version() == hyper::Version::HTTP_11 {
  //   Ok(Response::new(Body::from("Hello World")))
  // } else {
  // Note: it's usually better to return a Response
  // with an appropriate StatusCode instead of an Err.
  // Err("not HTTP/1.1, abort connection")
  // http_error(StatusCode::NOT_FOUND)
  https_redirection("www.google.com".to_string(), Some(443_u16), "/")
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

fn https_redirection(
  hostname: String,
  https_port: Option<u16>,
  path_and_query: &str,
) -> Result<Response<Body>> {
  let dest_uri: String = if let Some(https_port) = https_port {
    if https_port == 443 {
      format!("https://{}{}", hostname, path_and_query)
    } else {
      format!("https://{}:{}{}", hostname, https_port, path_and_query)
    }
  } else {
    return http_error(StatusCode::SERVICE_UNAVAILABLE);
  };
  let response = Response::builder()
    .status(StatusCode::MOVED_PERMANENTLY)
    .header("Location", dest_uri)
    .body(Body::empty())
    .unwrap();
  Ok(response)
}

fn parse_hostname_port(req: &Request<Body>, tls_enabled: bool) -> Result<(String, u16)> {
  let hostname_port_headers = req.headers().get("host");
  let hostname_uri = req.uri().host();
  let port_uri = req.uri().port_u16();

  if hostname_port_headers.is_none() && hostname_uri.is_none() {
    bail!("No host in request header");
  }

  let (hostname, port) = match (hostname_uri, hostname_port_headers) {
    (Some(x), _) => {
      let hostname = hostname_uri.unwrap();
      let port = if let Some(p) = port_uri {
        p
      } else if tls_enabled {
        443
      } else {
        80
      };
      (hostname.to_string(), port)
    }
    (None, Some(x)) => {
      let hp_as_uri = x.to_str().unwrap().parse::<Uri>().unwrap();
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
      (hostname.to_string(), port)
    }
    (None, None) => {
      bail!("Host unspecified in request")
    }
  };

  Ok((hostname, port))
}
