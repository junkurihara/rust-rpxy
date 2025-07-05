use thiserror::Error;

#[derive(Error, Debug)]
/// Error type for rpxy-acme
pub enum RpxyAcmeError {
  /// Invalid acme registry path
  #[error("Invalid acme registry path")]
  InvalidAcmeRegistryPath,
  /// Invalid url
  #[error("Invalid url: {0}")]
  InvalidUrl(#[from] url::ParseError),
  /// IO error
  #[error("IO error: {0}")]
  Io(#[from] std::io::Error),
  /// TLS client configuration error
  #[error("TLS client configuration error: {0}")]
  TlsClientConfig(String),
}
