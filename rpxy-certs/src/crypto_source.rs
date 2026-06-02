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
/// Emit a `warn!` if the private key file at `path` has any group or other
/// permission bits set. This is a Unix-only observability helper: it does not
/// modify the file and does not gate loading. A metadata error is silently
/// ignored, since the subsequent `File::open` will surface the real error.
fn warn_if_key_perm_loose(path: &Path) {
  #[cfg(unix)]
  {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
      return;
    };
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 == 0 {
      return;
    }
    if mode & 0o004 != 0 {
      warn!(
        "TLS private key file is world-readable (mode {:o}): {}. Recommended mode is 0600.",
        mode,
        path.display()
      );
    } else {
      warn!(
        "TLS private key file has loose permissions (mode {:o}): {}. Recommended mode is 0600.",
        mode,
        path.display()
      );
    }
  }
  #[cfg(not(unix))]
  {
    let _ = path;
  }
}

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
  warn_if_key_perm_loose(cert_key_path);
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

#[cfg(all(test, unix))]
mod tests {
  use super::warn_if_key_perm_loose;
  use std::io::Write;
  use std::os::unix::fs::PermissionsExt;
  use std::path::Path;
  use std::sync::{Arc, Mutex};
  use tempfile::tempdir;

  /// Custom `MakeWriter` that captures every line written by the `fmt`
  /// subscriber, so the test can assert on emitted warning text.
  #[derive(Clone, Default)]
  struct LogCapture(Arc<Mutex<Vec<u8>>>);

  impl LogCapture {
    fn snapshot(&self) -> String {
      String::from_utf8_lossy(&self.0.lock().unwrap()).to_string()
    }
  }

  impl Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
      self.0.lock().unwrap().extend_from_slice(buf);
      Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
      Ok(())
    }
  }

  impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
    type Writer = LogCapture;
    fn make_writer(&'a self) -> Self::Writer {
      self.clone()
    }
  }

  /// Run `f` with a fresh tracing subscriber that captures `warn!` events into
  /// the returned string. Uses `with_default` so concurrent tests do not
  /// clobber each other's subscriber.
  fn capture_warnings<F: FnOnce()>(f: F) -> String {
    let cap = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
      .with_writer(cap.clone())
      .with_max_level(tracing::Level::WARN)
      .with_ansi(false)
      .without_time()
      .finish();
    tracing::subscriber::with_default(subscriber, f);
    cap.snapshot()
  }

  fn write_key_with_mode(dir: &Path, mode: u32) -> std::path::PathBuf {
    let path = dir.join(format!("key_{:o}.pem", mode));
    std::fs::write(&path, b"dummy").expect("seed key");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    path
  }

  #[test]
  fn no_warning_for_0600() {
    let tmp = tempdir().expect("tempdir");
    let path = write_key_with_mode(tmp.path(), 0o600);
    let log = capture_warnings(|| warn_if_key_perm_loose(&path));
    assert!(log.is_empty(), "0600 must not produce a warning, got: {}", log);
  }

  #[test]
  fn no_warning_for_0400() {
    let tmp = tempdir().expect("tempdir");
    let path = write_key_with_mode(tmp.path(), 0o400);
    let log = capture_warnings(|| warn_if_key_perm_loose(&path));
    assert!(log.is_empty(), "0400 must not produce a warning, got: {}", log);
  }

  #[test]
  fn warns_for_group_readable_0640() {
    let tmp = tempdir().expect("tempdir");
    let path = write_key_with_mode(tmp.path(), 0o640);
    let log = capture_warnings(|| warn_if_key_perm_loose(&path));
    assert!(
      log.contains("loose permissions"),
      "0640 should warn about loose perms, got: {}",
      log
    );
    assert!(
      !log.contains("world-readable"),
      "0640 must not be flagged as world-readable, got: {}",
      log
    );
    assert!(log.contains("640"), "warning must mention the offending mode, got: {}", log);
  }

  #[test]
  fn warns_world_readable_for_0644() {
    let tmp = tempdir().expect("tempdir");
    let path = write_key_with_mode(tmp.path(), 0o644);
    let log = capture_warnings(|| warn_if_key_perm_loose(&path));
    assert!(
      log.contains("world-readable"),
      "0644 should be flagged world-readable, got: {}",
      log
    );
    assert!(log.contains("644"), "warning must mention the offending mode, got: {}", log);
  }

  #[test]
  fn nonexistent_path_is_silent() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("does_not_exist.pem");
    let log = capture_warnings(|| warn_if_key_perm_loose(&path));
    assert!(log.is_empty(), "missing key path must not warn, got: {}", log);
  }
}
