use http::StatusCode;
use thiserror::Error;

/// HTTP result type, T is typically a hyper::Response
/// HttpError is used to generate a synthetic error response
pub(crate) type HttpResult<T> = std::result::Result<T, HttpError>;

/// Describes things that can go wrong in the forwarder
#[derive(Debug, Error)]
pub enum HttpError {
  #[error("No host is give nin request header")]
  NoHostInRequestHeader,
  #[error("Invalid host in request header")]
  InvalidHostInRequestHeader,
  #[error("SNI and Host header mismatch")]
  SniHostInconsistency,
  #[error("No matching backend app")]
  NoMatchingBackendApp,
  #[error("Failed to redirect: {0}")]
  FailedToRedirect(String),

  #[error(transparent)]
  Other(#[from] anyhow::Error),
}

impl From<HttpError> for StatusCode {
  fn from(e: HttpError) -> StatusCode {
    match e {
      HttpError::NoHostInRequestHeader => StatusCode::BAD_REQUEST,
      HttpError::InvalidHostInRequestHeader => StatusCode::BAD_REQUEST,
      HttpError::SniHostInconsistency => StatusCode::MISDIRECTED_REQUEST,
      HttpError::NoMatchingBackendApp => StatusCode::SERVICE_UNAVAILABLE,
      _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
  }
}
