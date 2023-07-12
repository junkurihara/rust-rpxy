use crate::{
  certs::{CertsAndKeys, CryptoSource},
  log::*,
};
use async_trait::async_trait;
use rustls::{Certificate, PrivateKey};
use std::{
  fs::File,
  io::{self, BufReader, Cursor, Read},
  path::PathBuf,
};

/// Crypto-related file reader implementing certs::CryptoRead trait
pub struct CryptoFileSource {
  /// tls settings in file
  pub tls_cert_path: PathBuf,
  pub tls_cert_key_path: PathBuf,
  pub client_ca_cert_path: Option<PathBuf>,
}

#[async_trait]
impl CryptoSource for CryptoFileSource {
  type Error = io::Error;
  async fn read(&self) -> Result<CertsAndKeys, Self::Error> {
    read_certs_and_keys(
      &self.tls_cert_path,
      &self.tls_cert_key_path,
      self.client_ca_cert_path.as_ref(),
    )
  }
}

/// Read certificates and private keys from file
pub(crate) fn read_certs_and_keys(
  cert_path: &PathBuf,
  cert_key_path: &PathBuf,
  client_ca_cert_path: Option<&PathBuf>,
) -> Result<CertsAndKeys, io::Error> {
  debug!("Read TLS server certificates and private key");

  let certs: Vec<_> = {
    let certs_path_str = cert_path.display().to_string();
    let mut reader = BufReader::new(File::open(cert_path).map_err(|e| {
      io::Error::new(
        e.kind(),
        format!("Unable to load the certificates [{certs_path_str}]: {e}"),
      )
    })?);
    rustls_pemfile::certs(&mut reader)
      .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Unable to parse the certificates"))?
  }
  .drain(..)
  .map(Certificate)
  .collect();

  let cert_keys: Vec<_> = {
    let cert_key_path_str = cert_key_path.display().to_string();
    let encoded_keys = {
      let mut encoded_keys = vec![];
      File::open(cert_key_path)
        .map_err(|e| {
          io::Error::new(
            e.kind(),
            format!("Unable to load the certificate keys [{cert_key_path_str}]: {e}"),
          )
        })?
        .read_to_end(&mut encoded_keys)?;
      encoded_keys
    };
    let mut reader = Cursor::new(encoded_keys);
    let pkcs8_keys = rustls_pemfile::pkcs8_private_keys(&mut reader).map_err(|_| {
      io::Error::new(
        io::ErrorKind::InvalidInput,
        "Unable to parse the certificates private keys (PKCS8)",
      )
    })?;
    reader.set_position(0);
    let mut rsa_keys = rustls_pemfile::rsa_private_keys(&mut reader)?;
    let mut keys = pkcs8_keys;
    keys.append(&mut rsa_keys);
    if keys.is_empty() {
      return Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "No private keys found - Make sure that they are in PKCS#8/PEM format",
      ));
    }
    keys.drain(..).map(PrivateKey).collect()
  };

  let client_ca_certs = if let Some(path) = client_ca_cert_path {
    debug!("Read CA certificates for client authentication");
    // Reads client certificate and returns client
    let certs: Vec<_> = {
      let certs_path_str = path.display().to_string();
      let mut reader = BufReader::new(File::open(path).map_err(|e| {
        io::Error::new(
          e.kind(),
          format!("Unable to load the client certificates [{certs_path_str}]: {e}"),
        )
      })?);
      rustls_pemfile::certs(&mut reader)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Unable to parse the client certificates"))?
    }
    .drain(..)
    .map(Certificate)
    .collect();
    Some(certs)
  } else {
    None
  };

  Ok(CertsAndKeys {
    certs,
    cert_keys,
    client_ca_certs,
  })
}
