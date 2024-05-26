mod certs;
mod error;
mod service;
mod source;

#[allow(unused_imports)]
pub(crate) mod log {
  pub(crate) use tracing::{debug, error, info, warn};
}

pub use crate::{
  certs::SingleServerCrypto,
  source::{CryptoFileSource, CryptoFileSourceBuilder, CryptoFileSourceBuilderError, CryptoSource},
};

/* ------------------------------------------------ */
#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn read_server_crt_key_files() {
    let tls_cert_path = "../example-certs/server.crt";
    let tls_cert_key_path = "../example-certs/server.key";
    let crypto_file_source = CryptoFileSourceBuilder::default()
      .tls_cert_key_path(tls_cert_key_path)
      .tls_cert_path(tls_cert_path)
      .build();
    assert!(crypto_file_source.is_ok());

    let crypto_file_source = crypto_file_source.unwrap();
    let crypto_elem = crypto_file_source.read().await;
    assert!(crypto_elem.is_ok());

    let crypto_elem = crypto_elem.unwrap();
    let certificed_key = crypto_elem.rustls_certified_key();
    assert!(certificed_key.is_ok());
  }

  #[tokio::test]
  async fn read_server_crt_key_files_with_client_ca_crt() {
    let tls_cert_path = "../example-certs/server.crt";
    let tls_cert_key_path = "../example-certs/server.key";
    let client_ca_cert_path = Some("../example-certs/client.ca.crt");
    let crypto_file_source = CryptoFileSourceBuilder::default()
      .tls_cert_key_path(tls_cert_key_path)
      .tls_cert_path(tls_cert_path)
      .client_ca_cert_path(client_ca_cert_path)
      .build();
    assert!(crypto_file_source.is_ok());

    let crypto_file_source = crypto_file_source.unwrap();
    let crypto_elem = crypto_file_source.read().await;
    assert!(crypto_elem.is_ok());

    let crypto_elem = crypto_elem.unwrap();
    assert!(crypto_elem.is_mutual_tls());

    let certificed_key = crypto_elem.rustls_certified_key();
    assert!(certificed_key.is_ok());

    let trust_anchors = crypto_elem.rustls_trust_anchors();
    assert!(trust_anchors.is_ok());

    let trust_anchors = trust_anchors.unwrap();
    assert_eq!(trust_anchors.len(), 1);
  }
}
