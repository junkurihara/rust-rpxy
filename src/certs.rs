use async_trait::async_trait;
use rustls::{Certificate, PrivateKey};

/// Certificates and private keys in rustls loaded from files
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct CertsAndKeys {
  pub certs: Vec<Certificate>,
  pub cert_keys: Vec<PrivateKey>,
  pub client_ca_certs: Option<Vec<Certificate>>,
}

#[async_trait]
// Trait to read certs and keys anywhere from KVS, file, sqlite, etc.
pub trait CryptoSource {
  type Error;

  /// read crypto materials from source
  async fn read(&self) -> Result<CertsAndKeys, Self::Error>;

  /// Returns true when mutual tls is enabled
  fn is_mutual_tls(&self) -> bool;
}
