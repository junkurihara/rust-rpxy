use crate::{error::*, utils::*};
use hyper::{header, Request, Uri};
use std::net::SocketAddr;

////////////////////////////////////////////////////
// Functions of utils for request messages

pub(super) fn log_request_msg<B>(req: &Request<B>, client_addr: &SocketAddr) -> String {
  let server_name = req.headers().get(header::HOST).map_or_else(
    || {
      req
        .uri()
        .authority()
        .map_or_else(|| "<none>", |au| au.as_str())
    },
    |h| h.to_str().unwrap_or("<none>"),
  );

  return format!(
    "{} <- {} -- {} {:?} {:?} ({:?})",
    server_name,
    client_addr.to_canonical(),
    req.method(),
    req.version(),
    req
      .uri()
      .path_and_query()
      .map_or_else(|| "", |v| v.as_str()),
    req.headers()
  );
}

pub(super) fn parse_host_port<B: core::fmt::Debug>(
  req: &Request<B>,
) -> Result<(String, Option<u16>)> {
  let headers_host = req.headers().get("host");
  let uri_host = req.uri().host();
  let uri_port = req.uri().port_u16();

  ensure!(
    !(headers_host.is_none() && uri_host.is_none()),
    "No host in request header"
  );

  // prioritize server_name in uri
  if let Some(v) = uri_host {
    Ok((v.to_string(), uri_port))
  } else {
    let uri_from_host = headers_host.unwrap().to_str()?.parse::<Uri>()?;
    Ok((
      uri_from_host
        .host()
        .ok_or_else(|| anyhow!("Failed to parse host"))?
        .to_string(),
      uri_from_host.port_u16(),
    ))
  }
}
