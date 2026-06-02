use crate::constants::ACME_ACCOUNT_SUBDIR;
use async_trait::async_trait;
use aws_lc_rs as crypto;
use base64::prelude::*;
use blocking::unblock;
use crypto::digest::{Context, SHA256};
use rustls_acme::{AccountCache, CertCache};
use std::{
  io::{ErrorKind, Write},
  path::{Path, PathBuf},
};

/// Mode applied to newly-created cache directories on Unix. Directories that
/// already exist are not modified; this only takes effect when the directory
/// is actually created by `DirBuilder::create`.
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// Mode applied to newly-created cache files (including private keys) on Unix.
/// Existing files retain their current mode; `OpenOptions::mode` only applies
/// when `O_CREAT` actually allocates a new inode.
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;

/// Create `dir` (and intermediate components) if missing. On Unix, newly
/// created components get mode `DIR_MODE`; existing components are untouched.
fn create_dir_secure(dir: &Path) -> std::io::Result<()> {
  let mut builder = std::fs::DirBuilder::new();
  builder.recursive(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::DirBuilderExt;
    builder.mode(DIR_MODE);
  }
  builder.create(dir)
}

/// Write `contents` to `path`, creating it with mode `FILE_MODE` on Unix if it
/// did not already exist. If the file existed, its mode is preserved (the
/// content is truncated and overwritten in place).
fn write_file_secure(path: &Path, contents: &[u8]) -> std::io::Result<()> {
  let mut opts = std::fs::OpenOptions::new();
  opts.write(true).create(true).truncate(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    opts.mode(FILE_MODE);
  }
  let mut f = opts.open(path)?;
  f.write_all(contents)?;
  Ok(())
}

enum FileType {
  Account,
  Cert,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct DirCache {
  pub(super) account_dir: PathBuf,
  pub(super) cert_dir: PathBuf,
}

impl DirCache {
  pub fn new<P>(dir: P, server_name: &str) -> Self
  where
    P: AsRef<Path>,
  {
    Self {
      account_dir: dir.as_ref().join(ACME_ACCOUNT_SUBDIR),
      cert_dir: dir.as_ref().join(server_name),
    }
  }
  async fn read_if_exist(&self, file: impl AsRef<Path>, file_type: FileType) -> Result<Option<Vec<u8>>, std::io::Error> {
    let subdir = match file_type {
      FileType::Account => &self.account_dir,
      FileType::Cert => &self.cert_dir,
    };
    let file_path = subdir.join(file);
    match unblock(move || std::fs::read(file_path)).await {
      Ok(content) => Ok(Some(content)),
      Err(err) => match err.kind() {
        ErrorKind::NotFound => Ok(None),
        _ => Err(err),
      },
    }
  }
  async fn write(&self, file: impl AsRef<Path>, contents: impl AsRef<[u8]>, file_type: FileType) -> Result<(), std::io::Error> {
    let subdir = match file_type {
      FileType::Account => &self.account_dir,
      FileType::Cert => &self.cert_dir,
    }
    .clone();
    let subdir_clone = subdir.clone();
    unblock(move || create_dir_secure(&subdir_clone)).await?;
    let file_path = subdir.join(file);
    let contents = contents.as_ref().to_owned();
    unblock(move || write_file_secure(&file_path, &contents)).await
  }
  pub fn cached_account_file_name(contact: &[String], directory_url: impl AsRef<str>) -> String {
    let mut ctx = Context::new(&SHA256);
    for el in contact {
      ctx.update(el.as_ref());
      ctx.update(&[0])
    }
    ctx.update(directory_url.as_ref().as_bytes());
    let hash = BASE64_URL_SAFE_NO_PAD.encode(ctx.finish());
    format!("cached_account_{}", hash)
  }
  pub fn cached_cert_file_name(domains: &[String], directory_url: impl AsRef<str>) -> String {
    let mut ctx = Context::new(&SHA256);
    for domain in domains {
      ctx.update(domain.as_ref());
      ctx.update(&[0])
    }
    ctx.update(directory_url.as_ref().as_bytes());
    let hash = BASE64_URL_SAFE_NO_PAD.encode(ctx.finish());
    format!("cached_cert_{}", hash)
  }

  /// Verify that we have write permissions to both account and cert directories.
  /// This should be called at startup to fail fast if permissions are incorrect.
  pub async fn verify_write_permissions(&self) -> Result<(), std::io::Error> {
    // Test write to account directory
    Self::verify_dir_writable(&self.account_dir).await?;
    // Test write to cert directory
    Self::verify_dir_writable(&self.cert_dir).await?;
    Ok(())
  }

  /// Verify that a directory is writable by creating it and writing a test file.
  /// Uses unique filename (PID + timestamp) to avoid race conditions when multiple
  /// instances start simultaneously.
  async fn verify_dir_writable(dir: &Path) -> Result<(), std::io::Error> {
    let dir = dir.to_owned();
    unblock(move || {
      create_dir_secure(&dir)?;
      let test_file = dir.join(format!(
        ".write_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
          .duration_since(std::time::UNIX_EPOCH)
          .map(|d| d.as_nanos())
          .unwrap_or(0)
      ));
      write_file_secure(&test_file, b"test")?;
      std::fs::remove_file(&test_file)?;
      Ok(())
    })
    .await
  }
}

#[async_trait]
impl CertCache for DirCache {
  type EC = std::io::Error;
  async fn load_cert(&self, domains: &[String], directory_url: &str) -> Result<Option<Vec<u8>>, Self::EC> {
    let file_name = Self::cached_cert_file_name(domains, directory_url);
    self.read_if_exist(file_name, FileType::Cert).await
  }
  async fn store_cert(&self, domains: &[String], directory_url: &str, cert: &[u8]) -> Result<(), Self::EC> {
    let file_name = Self::cached_cert_file_name(domains, directory_url);
    self.write(file_name, cert, FileType::Cert).await
  }
}

#[async_trait]
impl AccountCache for DirCache {
  type EA = std::io::Error;
  async fn load_account(&self, contact: &[String], directory_url: &str) -> Result<Option<Vec<u8>>, Self::EA> {
    let file_name = Self::cached_account_file_name(contact, directory_url);
    self.read_if_exist(file_name, FileType::Account).await
  }

  async fn store_account(&self, contact: &[String], directory_url: &str, account: &[u8]) -> Result<(), Self::EA> {
    let file_name = Self::cached_account_file_name(contact, directory_url);
    self.write(file_name, account, FileType::Account).await
  }
}

#[cfg(all(test, unix))]
mod tests {
  use super::*;
  use std::os::unix::fs::PermissionsExt;
  use tempfile::tempdir;

  fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).expect("metadata").permissions().mode() & 0o7777
  }

  #[tokio::test]
  async fn write_creates_new_file_with_0600_and_new_dir_with_0700() {
    let tmp = tempdir().expect("tempdir");
    let cache = DirCache::new(tmp.path(), "example.com");

    cache.write("cached_cert_x", b"payload", FileType::Cert).await.expect("write");

    let dir_mode = mode_of(&cache.cert_dir);
    assert_eq!(dir_mode, 0o700, "new cert dir should be 0700, got {:o}", dir_mode);

    let file_mode = mode_of(&cache.cert_dir.join("cached_cert_x"));
    assert_eq!(file_mode, 0o600, "new cert file should be 0600, got {:o}", file_mode);
  }

  #[tokio::test]
  async fn write_preserves_existing_dir_mode() {
    let tmp = tempdir().expect("tempdir");
    let cache = DirCache::new(tmp.path(), "example.com");

    std::fs::create_dir_all(&cache.cert_dir).expect("pre-create");
    std::fs::set_permissions(&cache.cert_dir, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    cache.write("cached_cert_x", b"payload", FileType::Cert).await.expect("write");

    let dir_mode = mode_of(&cache.cert_dir);
    assert_eq!(dir_mode, 0o755, "pre-existing dir mode must be preserved, got {:o}", dir_mode);
  }

  #[tokio::test]
  async fn write_preserves_existing_file_mode_on_overwrite() {
    let tmp = tempdir().expect("tempdir");
    let cache = DirCache::new(tmp.path(), "example.com");

    std::fs::create_dir_all(&cache.cert_dir).expect("pre-create");
    let target = cache.cert_dir.join("cached_cert_x");
    std::fs::write(&target, b"old").expect("seed");
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).expect("chmod");

    cache
      .write("cached_cert_x", b"new payload", FileType::Cert)
      .await
      .expect("write");

    let file_mode = mode_of(&target);
    assert_eq!(
      file_mode, 0o644,
      "pre-existing file mode must be preserved, got {:o}",
      file_mode
    );
    let body = std::fs::read(&target).expect("read");
    assert_eq!(body, b"new payload", "file contents should be replaced");
  }

  #[tokio::test]
  async fn verify_dir_writable_does_not_leak_test_file_and_uses_secure_mode() {
    let tmp = tempdir().expect("tempdir");
    let cache = DirCache::new(tmp.path(), "example.com");

    cache.verify_write_permissions().await.expect("verify");

    let entries: Vec<_> = std::fs::read_dir(&cache.cert_dir)
      .expect("read_dir")
      .filter_map(|e| e.ok())
      .collect();
    assert!(entries.is_empty(), "verify_write_permissions must clean up its probe");

    let dir_mode = mode_of(&cache.cert_dir);
    assert_eq!(
      dir_mode, 0o700,
      "cert dir created by verify should be 0700, got {:o}",
      dir_mode
    );
  }
}
