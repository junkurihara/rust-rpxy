use thiserror::Error;

/// Describes things that can go wrong in the Rpxy certificate
#[derive(Debug, Error)]
pub enum RpxyCertError {
  /// Error when reading certificates and keys
  #[error("Failed to read certificates from file: {0}")]
  IoError(#[from] std::io::Error),
  /// Error when parsing certificates and keys to generate a rustls CertifiedKey
  #[error("Unable to find a valid certificate and key")]
  InvalidCertificateAndKey,
  /// Error when parsing client CA certificates: No client certificate found
  #[error("No client certificate found")]
  NoClientCert,
  /// Error for hot reload certificate reloader
  #[error("Certificate reload error: {0}")]
  CertificateReloadError(#[from] hot_reload::ReloaderError<crate::server_crypto::ServerCryptoBase>),
}
