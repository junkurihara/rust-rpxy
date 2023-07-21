// Highly motivated by https://github.com/felipenoris/hyper-reverse-proxy
use crate::error::*;
use hyper::{Body, Request, Response, StatusCode, Uri};

////////////////////////////////////////////////////
// Functions to create response (error or redirect)

pub(super) fn http_error(status_code: StatusCode) -> Result<Response<Body>> {
  let response = Response::builder().status(status_code).body(Body::empty())?;
  Ok(response)
}

pub(super) fn secure_redirection<B>(
  server_name: &str,
  tls_port: Option<u16>,
  req: &Request<B>,
) -> Result<Response<Body>> {
  let pq = match req.uri().path_and_query() {
    Some(x) => x.as_str(),
    _ => "",
  };
  let new_uri = Uri::builder().scheme("https").path_and_query(pq);
  let dest_uri = match tls_port {
    Some(443) | None => new_uri.authority(server_name),
    Some(p) => new_uri.authority(format!("{server_name}:{p}")),
  }
  .build()?;
  let response = Response::builder()
    .status(StatusCode::MOVED_PERMANENTLY)
    .header("Location", dest_uri.to_string())
    .body(Body::empty())?;
  Ok(response)
}
