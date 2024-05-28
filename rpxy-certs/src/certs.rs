use crate::error::*;
use rustc_hash::FxHashMap as HashMap;
use rustls::{crypto::aws_lc_rs::sign::any_supported_type, pki_types, sign::CertifiedKey};
use std::sync::Arc;
use x509_parser::prelude::*;

/* ------------------------------------------------ */
/// Raw certificates in rustls format
type Certificate = rustls::pki_types::CertificateDer<'static>;
/// Raw private key in rustls format
type PrivateKey = pki_types::PrivateKeyDer<'static>;
/// Subject Key ID in bytes
type SubjectKeyIdentifier = Vec<u8>;
/// Client CA trust anchors subject to the subject key identifier
type TrustAnchors = HashMap<SubjectKeyIdentifier, pki_types::TrustAnchor<'static>>;

/* ------------------------------------------------ */
/// Raw certificates and private keys loaded from files for a single server name
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct SingleServerCertsKeys {
  certs: Vec<Certificate>,
  cert_keys: Arc<Vec<PrivateKey>>,
  client_ca_certs: Option<Vec<Certificate>>,
}

impl SingleServerCertsKeys {
  /// Create a new instance of SingleServerCrypto
  pub fn new(certs: &[Certificate], cert_keys: &Arc<Vec<PrivateKey>>, client_ca_certs: &Option<Vec<Certificate>>) -> Self {
    Self {
      certs: certs.to_owned(),
      cert_keys: cert_keys.clone(),
      client_ca_certs: client_ca_certs.clone(),
    }
  }
  /// Check if mutual tls is enabled
  pub fn is_mutual_tls(&self) -> bool {
    self.client_ca_certs.is_some()
  }
  /* ------------------------------------------------ */
  /// Convert the certificates to bytes in der
  pub fn certs_bytes(&self) -> Vec<Vec<u8>> {
    self.certs.iter().map(|c| c.to_vec()).collect()
  }
  /// Convert the private keys to bytes in der
  pub fn cert_keys_bytes(&self) -> Vec<Vec<u8>> {
    self
      .cert_keys
      .iter()
      .map(|k| match k {
        pki_types::PrivateKeyDer::Pkcs1(pkcs1) => pkcs1.secret_pkcs1_der().to_owned(),
        pki_types::PrivateKeyDer::Sec1(sec1) => sec1.secret_sec1_der().to_owned(),
        pki_types::PrivateKeyDer::Pkcs8(pkcs8) => pkcs8.secret_pkcs8_der().to_owned(),
        _ => unreachable!(),
      })
      .collect()
  }
  /// Convert the client CA certificates to bytes in der
  pub fn client_ca_certs_bytes(&self) -> Option<Vec<Vec<u8>>> {
    self.client_ca_certs.as_ref().map(|v| v.iter().map(|c| c.to_vec()).collect())
  }
  /* ------------------------------------------------ */
  /// Parse the certificates and private keys for a single server and return a rustls CertifiedKey
  pub fn rustls_certified_key(&self) -> Result<CertifiedKey, RpxyCertError> {
    let signing_key = self
      .cert_keys
      .clone()
      .iter()
      .find_map(|k| if let Ok(sk) = any_supported_type(k) { Some(sk) } else { None })
      .ok_or_else(|| RpxyCertError::InvalidCertificateAndKey)?;

    let cert = self.certs.iter().map(|c| Certificate::from(c.to_vec())).collect::<Vec<_>>();
    Ok(CertifiedKey::new(cert, signing_key))
  }

  /* ------------------------------------------------ */
  /// Parse the client CA certificates and return a hashmap of pairs of a subject key identifier (key) and a trust anchor (value)
  pub fn rustls_client_certs_trust_anchors(&self) -> Result<TrustAnchors, RpxyCertError> {
    let Some(certs) = self.client_ca_certs.as_ref() else {
      return Err(RpxyCertError::NoClientCert);
    };
    let certs = certs.iter().map(|c| Certificate::from(c.to_vec())).collect::<Vec<_>>();

    let trust_anchors = certs
      .iter()
      .filter_map(|v| {
        // retrieve trust anchor
        let trust_anchor = webpki::anchor_from_trusted_cert(v).ok()?;

        // retrieve ca key id (subject key id)
        let x509_cert = parse_x509_certificate(v).map(|v| v.1).ok()?;
        let mut subject_key_ids = x509_cert.iter_extensions().filter_map(|ext| match ext.parsed_extension() {
          ParsedExtension::SubjectKeyIdentifier(skid) => Some(skid),
          _ => None,
        });
        let skid = subject_key_ids.next()?;

        Some((skid.0.to_owned(), trust_anchor.to_owned()))
      })
      .collect::<HashMap<_, _>>();

    Ok(trust_anchors)
  }
}

/* ------------------------------------------------ */
#[cfg(test)]
mod tests {
  use super::super::*;

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

    let trust_anchors = crypto_elem.rustls_client_certs_trust_anchors();
    assert!(trust_anchors.is_ok());

    let trust_anchors = trust_anchors.unwrap();
    assert_eq!(trust_anchors.len(), 1);
  }
}
