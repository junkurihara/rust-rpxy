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
  #[error("No upstream candidates")]
  NoUpstreamCandidates,
  #[error("Failed to generate upstream request for backend application: {0}")]
  FailedToGenerateUpstreamRequest(String),
  #[error("Failed to get response from backend: {0}")]
  FailedToGetResponseFromBackend(String),

  #[error("Failed to add set-cookie header in response {0}")]
  FailedToAddSetCookeInResponse(String),
  #[error("Failed to generated downstream response for clients: {0}")]
  FailedToGenerateDownstreamResponse(String),

  #[error("Failed to upgrade connection: {0}")]
  FailedToUpgrade(String),
  #[error("Request does not have an upgrade extension")]
  NoUpgradeExtensionInRequest,
  #[error("Response does not have an upgrade extension")]
  NoUpgradeExtensionInResponse,

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
      HttpError::FailedToRedirect(_) => StatusCode::INTERNAL_SERVER_ERROR,
      HttpError::NoUpstreamCandidates => StatusCode::NOT_FOUND,
      HttpError::FailedToGenerateUpstreamRequest(_) => StatusCode::INTERNAL_SERVER_ERROR,
      HttpError::FailedToAddSetCookeInResponse(_) => StatusCode::INTERNAL_SERVER_ERROR,
      HttpError::FailedToGenerateDownstreamResponse(_) => StatusCode::INTERNAL_SERVER_ERROR,
      HttpError::FailedToUpgrade(_) => StatusCode::INTERNAL_SERVER_ERROR,
      HttpError::NoUpgradeExtensionInRequest => StatusCode::BAD_REQUEST,
      HttpError::NoUpgradeExtensionInResponse => StatusCode::BAD_GATEWAY,
      _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
  }
}
