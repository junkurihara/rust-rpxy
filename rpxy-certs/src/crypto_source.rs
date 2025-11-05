use crate::{certs::SingleServerCertsKeys, error::*, log::*};
use async_trait::async_trait;
use derive_builder::Builder;
use rustls::pki_types::{self, pem::PemObject};
use std::{
  fs::File,
  io::{self, BufReader, Cursor, Read},
  path::{Path, PathBuf},
  sync::Arc,
};

/* ------------------------------------------------ */
#[async_trait]
// Trait to read certs and keys anywhere from KVS, file, sqlite, etc.
pub trait CryptoSource {
  type Error;

  /// read crypto materials from source
  async fn read(&self) -> Result<SingleServerCertsKeys, Self::Error>;

  /// Returns true when mutual tls is enabled
  fn is_mutual_tls(&self) -> bool;
}

/* ------------------------------------------------ */
#[derive(Builder, Debug, Clone)]
/// Crypto-related file reader implementing `CryptoSource` trait
pub struct CryptoFileSource {
  #[builder(setter(custom))]
  /// Always exist
  pub tls_cert_path: PathBuf,

  #[builder(setter(custom))]
  /// Always exist
  pub tls_cert_key_path: PathBuf,

  #[builder(setter(custom), default)]
  /// This may not exist
  pub client_ca_cert_path: Option<PathBuf>,
}

impl CryptoFileSourceBuilder {
  pub fn tls_cert_path<T: AsRef<Path>>(&mut self, v: T) -> &mut Self {
    self.tls_cert_path = Some(v.as_ref().to_path_buf());
    self
  }
  pub fn tls_cert_key_path<T: AsRef<Path>>(&mut self, v: T) -> &mut Self {
    self.tls_cert_key_path = Some(v.as_ref().to_path_buf());
    self
  }
  pub fn client_ca_cert_path<T: AsRef<Path>>(&mut self, v: Option<T>) -> &mut Self {
    self.client_ca_cert_path = Some(v.map(|p| p.as_ref().to_path_buf()));
    self
  }
}

/* ------------------------------------------------ */
#[async_trait]
impl CryptoSource for CryptoFileSource {
  type Error = RpxyCertError;
  /// read crypto materials from source
  async fn read(&self) -> Result<SingleServerCertsKeys, Self::Error> {
    read_certs_and_keys(
      &self.tls_cert_path,
      &self.tls_cert_key_path,
      self.client_ca_cert_path.as_ref(),
    )
  }
  /// Returns true when mutual tls is enabled
  fn is_mutual_tls(&self) -> bool {
    self.client_ca_cert_path.is_some()
  }
}

/* ------------------------------------------------ */
/// Read certificates and private keys from file
fn read_certs_and_keys(
  cert_path: &PathBuf,
  cert_key_path: &PathBuf,
  client_ca_cert_path: Option<&PathBuf>,
) -> Result<SingleServerCertsKeys, RpxyCertError> {
  debug!("Read TLS server certificates and private key");

  // ------------------------
  // certificates
  let mut reader = BufReader::new(File::open(cert_path).map_err(|e| {
    io::Error::new(
      e.kind(),
      format!("Unable to load the certificates [{}]: {e}", cert_path.display()),
    )
  })?);
  let raw_certs = pki_types::CertificateDer::pem_reader_iter(&mut reader)
    .collect::<Result<Vec<_>, _>>()
    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Unable to parse the certificates"))?;

  // ------------------------
  // private keys
  let mut encoded_keys = vec![];
  File::open(cert_key_path)
    .map_err(|e| {
      io::Error::new(
        e.kind(),
        format!("Unable to load the certificate keys [{}]: {e}", cert_key_path.display()),
      )
    })?
    .read_to_end(&mut encoded_keys)?;
  let mut reader = Cursor::new(encoded_keys);
  let pkcs8_keys = pki_types::PrivatePkcs8KeyDer::pem_reader_iter(&mut reader)
    .map(|v| v.map(pki_types::PrivateKeyDer::Pkcs8))
    .collect::<Result<Vec<_>, _>>()
    .map_err(|_| {
      io::Error::new(
        io::ErrorKind::InvalidInput,
        "Unable to parse the certificates private keys (PKCS8)",
      )
    })?;
  reader.set_position(0);
  let mut rsa_keys = pki_types::PrivatePkcs1KeyDer::pem_reader_iter(&mut reader)
    .map(|v| v.map(pki_types::PrivateKeyDer::Pkcs1))
    .collect::<Result<Vec<_>, _>>()
    .map_err(|_| {
      io::Error::new(
        io::ErrorKind::InvalidInput,
        "Unable to parse the certificates private keys (RSA)",
      )
    })?;
  let mut raw_cert_keys = pkcs8_keys;
  raw_cert_keys.append(&mut rsa_keys);
  if raw_cert_keys.is_empty() {
    return Err(RpxyCertError::IoError(io::Error::new(
      io::ErrorKind::InvalidInput,
      "No private keys found - Make sure that they are in PKCS#8/PEM format",
    )));
  }

  // ------------------------
  // client ca certificates
  let client_ca_certs = client_ca_cert_path
    .map(|path| {
      debug!("Read CA certificates for client authentication");
      // Reads client certificate and returns client
      let inner = File::open(path).map_err(|e| {
        io::Error::new(
          e.kind(),
          format!("Unable to load the client certificates [{}]: {e}", path.display()),
        )
      })?;
      let mut reader = BufReader::new(inner);
      pki_types::CertificateDer::pem_reader_iter(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Unable to parse the client certificates"))
    })
    .transpose()?;

  Ok(SingleServerCertsKeys::new(
    &raw_certs,
    &Arc::new(raw_cert_keys),
    &client_ca_certs,
  ))
}
