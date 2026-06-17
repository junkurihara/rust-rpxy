use super::http_result::{HttpError, HttpResult};
use crate::{
  error::*,
  hyper_ext::body::{ResponseBody, empty},
  name_exp::ServerName,
};
use http::{Request, Response, StatusCode, Uri, Version, header};

/// build http response with status code of 4xx and 5xx
pub(crate) fn synthetic_error_response(status_code: StatusCode) -> RpxyResult<Response<ResponseBody>> {
  let res = Response::builder()
    .status(status_code)
    .body(ResponseBody::Boxed(empty()))
    .unwrap();
  Ok(res)
}

/// Build a 4xx/5xx synthetic response that closes the connection on h1.
///
/// HTTP/1.x has no in-band stream-close mechanism, so when we reject a request before
/// consuming its body (e.g. a 413 Payload Too Large from the pre-flight Content-Length
/// check) the client could otherwise keep streaming the rest of the body into a
/// connection we want to recycle. Set `Connection: close` for HTTP/1.0 and HTTP/1.1; for
/// HTTP/2 and HTTP/3 the response naturally closes only the affected stream and no
/// header is needed. `request_version` is captured at the handler entry before the
/// request value is moved into the inner handler.
pub(crate) fn synthetic_error_response_with_close(
  status_code: StatusCode,
  request_version: Version,
) -> RpxyResult<Response<ResponseBody>> {
  let mut builder = Response::builder().status(status_code);
  if matches!(request_version, Version::HTTP_10 | Version::HTTP_11) {
    builder = builder.header(header::CONNECTION, "close");
  }
  let res = builder.body(ResponseBody::Boxed(empty())).unwrap();
  Ok(res)
}

#[cfg(test)]
mod tests {
  use super::*;

  /// HTTP/1.1: `Connection: close` is inserted so the unread body can't be fed into a
  /// recycled connection.
  #[test]
  fn close_aware_413_h1_inserts_connection_close() {
    let res = synthetic_error_response_with_close(StatusCode::PAYLOAD_TOO_LARGE, Version::HTTP_11).unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
      res.headers().get(header::CONNECTION).map(|v| v.as_bytes()),
      Some(b"close".as_ref())
    );
  }

  /// HTTP/1.0: same as HTTP/1.1 — the stream-close mechanism is connection-level.
  #[test]
  fn close_aware_413_h10_inserts_connection_close() {
    let res = synthetic_error_response_with_close(StatusCode::PAYLOAD_TOO_LARGE, Version::HTTP_10).unwrap();
    assert_eq!(
      res.headers().get(header::CONNECTION).map(|v| v.as_bytes()),
      Some(b"close".as_ref())
    );
  }

  /// HTTP/2: stream close happens in-band via `END_STREAM`; no `Connection: close`
  /// header (and the http crate would reject one as forbidden in h2 if it ever reached
  /// the writer).
  #[test]
  fn close_aware_413_h2_omits_connection_close() {
    let res = synthetic_error_response_with_close(StatusCode::PAYLOAD_TOO_LARGE, Version::HTTP_2).unwrap();
    assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(res.headers().get(header::CONNECTION).is_none());
  }

  /// HTTP/3: same as h2 — connection header is not used; stream close is protocol-native.
  #[test]
  fn close_aware_413_h3_omits_connection_close() {
    let res = synthetic_error_response_with_close(StatusCode::PAYLOAD_TOO_LARGE, Version::HTTP_3).unwrap();
    assert!(res.headers().get(header::CONNECTION).is_none());
  }
}

/// Generate synthetic response message of a redirection to https host with 301
pub(super) fn secure_redirection_response<B>(
  server_name: &ServerName,
  tls_port: Option<u16>,
  req: &Request<B>,
) -> HttpResult<Response<ResponseBody>> {
  let server_name: String = server_name.to_string();
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
    .body(ResponseBody::Boxed(empty()))
    .map_err(|e| HttpError::FailedToRedirect(e.to_string()))?;
  Ok(response)
}
