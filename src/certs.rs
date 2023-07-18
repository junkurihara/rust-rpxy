use async_trait::async_trait;
use rustc_hash::FxHashSet as HashSet;
use rustls::{
  sign::{any_supported_type, CertifiedKey},
  Certificate, OwnedTrustAnchor, PrivateKey,
};
use std::io;
use x509_parser::prelude::*;

#[async_trait]
// Trait to read certs and keys anywhere from KVS, file, sqlite, etc.
pub trait CryptoSource {
  type Error;

  /// read crypto materials from source
  async fn read(&self) -> Result<CertsAndKeys, Self::Error>;

  /// Returns true when mutual tls is enabled
  fn is_mutual_tls(&self) -> bool;
}

/// Certificates and private keys in rustls loaded from files
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct CertsAndKeys {
  pub certs: Vec<Certificate>,
  pub cert_keys: Vec<PrivateKey>,
  pub client_ca_certs: Option<Vec<Certificate>>,
}

impl CertsAndKeys {
  pub fn parse_server_certs_and_keys(&self) -> Result<CertifiedKey, anyhow::Error> {
    // for (server_name_bytes_exp, certs_and_keys) in self.inner.iter() {
    let signing_key = self
      .cert_keys
      .iter()
      .find_map(|k| {
        if let Ok(sk) = any_supported_type(k) {
          Some(sk)
        } else {
          None
        }
      })
      .ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::InvalidInput,
          "Unable to find a valid certificate and key",
        )
      })?;
    Ok(CertifiedKey::new(self.certs.clone(), signing_key))
  }

  pub fn parse_client_ca_certs(&self) -> Result<(Vec<OwnedTrustAnchor>, HashSet<Vec<u8>>), anyhow::Error> {
    let certs = self.client_ca_certs.as_ref().ok_or(anyhow::anyhow!("No client cert"))?;

    let owned_trust_anchors: Vec<_> = certs
      .iter()
      .map(|v| {
        // let trust_anchor = tokio_rustls::webpki::TrustAnchor::try_from_cert_der(&v.0).unwrap();
        let trust_anchor = webpki::TrustAnchor::try_from_cert_der(&v.0).unwrap();
        rustls::OwnedTrustAnchor::from_subject_spki_name_constraints(
          trust_anchor.subject,
          trust_anchor.spki,
          trust_anchor.name_constraints,
        )
      })
      .collect();

    // TODO: SKID is not used currently
    let subject_key_identifiers: HashSet<_> = certs
      .iter()
      .filter_map(|v| {
        // retrieve ca key id (subject key id)
        let cert = parse_x509_certificate(&v.0).unwrap().1;
        let subject_key_ids = cert
          .iter_extensions()
          .filter_map(|ext| match ext.parsed_extension() {
            ParsedExtension::SubjectKeyIdentifier(skid) => Some(skid),
            _ => None,
          })
          .collect::<Vec<_>>();
        if !subject_key_ids.is_empty() {
          Some(subject_key_ids[0].0.to_owned())
        } else {
          None
        }
      })
      .collect();

    Ok((owned_trust_anchors, subject_key_identifiers))
  }
}
