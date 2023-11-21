use http::StatusCode;
use thiserror::Error;

pub type HttpResult<T> = std::result::Result<T, HttpError>;

/// Describes things that can go wrong in the handler
#[derive(Debug, Error)]
pub enum HttpError {}

impl From<HttpError> for StatusCode {
  fn from(e: HttpError) -> StatusCode {
    match e {
      _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
  }
}
