use thiserror::Error;

pub type RpxyResult<T> = std::result::Result<T, RpxyError>;

/// Describes things that can go wrong in the Rpxy
#[derive(Debug, Error)]
pub enum RpxyError {
  // general errors
  #[error("IO error: {0}")]
  Io(#[from] std::io::Error),

  // TLS errors
  #[error("Failed to build TLS acceptor: {0}")]
  FailedToTlsHandshake(String),
  #[error("No server name in ClientHello")]
  NoServerNameInClientHello,
  #[error("No TLS serving app: {0}")]
  NoTlsServingApp(String),
  #[error("Failed to update server crypto: {0}")]
  FailedToUpdateServerCrypto(String),
  #[error("No server crypto: {0}")]
  NoServerCrypto(String),

  // hyper errors
  #[error("hyper body manipulation error: {0}")]
  HyperBodyManipulationError(String),
  #[error("New closed in incoming-like")]
  HyperIncomingLikeNewClosed,
  #[error("New body write aborted")]
  HyperNewBodyWriteAborted,

  // http/3 errors
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  #[error("H3 error: {0}")]
  H3Error(#[from] h3::Error),
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  #[error("Exceeds max request body size for HTTP/3")]
  H3TooLargeBody,

  #[cfg(feature = "http3-quinn")]
  #[error("Invalid rustls TLS version: {0}")]
  QuinnInvalidTlsProtocolVersion(String),
  #[cfg(feature = "http3-quinn")]
  #[error("Quinn connection error: {0}")]
  QuinnConnectionFailed(#[from] quinn::ConnectionError),

  #[cfg(feature = "http3-s2n")]
  #[error("s2n-quic validation error: {0}")]
  S2nQuicValidationError(#[from] s2n_quic_core::transport::parameters::ValidationError),
  #[cfg(feature = "http3-s2n")]
  #[error("s2n-quic connection error: {0}")]
  S2nQuicConnectionError(#[from] s2n_quic_core::connection::Error),
  #[cfg(feature = "http3-s2n")]
  #[error("s2n-quic start error: {0}")]
  S2nQuicStartError(#[from] s2n_quic::provider::StartError),

  // certificate reloader errors
  #[error("No certificate reloader when building a proxy for TLS")]
  NoCertificateReloader,
  #[error("Certificate reload error: {0}")]
  CertificateReloadError(#[from] hot_reload::ReloaderError<crate::crypto::ServerCryptoBase>),

  // backend errors
  #[error("Invalid reverse proxy setting")]
  InvalidReverseProxyConfig,
  #[error("Invalid upstream option setting")]
  InvalidUpstreamOptionSetting,
  #[error("Failed to build backend app: {0}")]
  FailedToBuildBackendApp(#[from] crate::backend::BackendAppBuilderError),

  // Handler errors
  #[error("Failed to build message handler: {0}")]
  FailedToBuildMessageHandler(#[from] crate::message_handle::HttpMessageHandlerBuilderError),
  #[error("Failed to upgrade request: {0}")]
  FailedToUpgradeRequest(String),
  #[error("Failed to upgrade response: {0}")]
  FailedToUpgradeResponse(String),
  #[error("Failed to copy bidirectional for upgraded connections: {0}")]
  FailedToCopyBidirectional(String),

  // Upstream connection setting errors
  #[error("Unsupported upstream option")]
  UnsupportedUpstreamOption,

  // Others
  #[error("Infallible")]
  Infallible(#[from] std::convert::Infallible),
}
