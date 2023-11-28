use super::http_result::{HttpError, HttpResult};
use crate::{
  error::*,
  hyper_ext::body::{empty, BoxBody, IncomingOr},
  name_exp::ServerName,
};
use http::{Request, Response, StatusCode, Uri};

/// build http response with status code of 4xx and 5xx
pub(crate) fn synthetic_error_response(status_code: StatusCode) -> RpxyResult<Response<IncomingOr<BoxBody>>> {
  let res = Response::builder()
    .status(status_code)
    .body(IncomingOr::Right(empty()))
    .unwrap();
  Ok(res)
}

/// Generate synthetic response message of a redirection to https host with 301
pub(super) fn secure_redirection_response<B>(
  server_name: &ServerName,
  tls_port: Option<u16>,
  req: &Request<B>,
) -> HttpResult<Response<IncomingOr<BoxBody>>> {
  let server_name: String = server_name.try_into().unwrap_or_default();
  let pq = match req.uri().path_and_query() {
    Some(x) => x.as_str(),
    _ => "",
  };
  let new_uri = Uri::builder().scheme("https").path_and_query(pq);
  let dest_uri = match tls_port {
    Some(443) | None => new_uri.authority(server_name),
    Some(p) => new_uri.authority(format!("{server_name}:{p}")),
  }
  .build()
  .map_err(|e| HttpError::FailedToRedirect(e.to_string()))?;
  let response = Response::builder()
    .status(StatusCode::MOVED_PERMANENTLY)
    .header("Location", dest_uri.to_string())
    .body(IncomingOr::Right(empty()))
    .map_err(|e| HttpError::FailedToRedirect(e.to_string()))?;
  Ok(response)
}
