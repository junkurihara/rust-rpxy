use rustc_hash::FxHashMap as HashMap;
use std::path::PathBuf;
use url::Url;

use crate::{
  constants::{ACME_ACCOUNT_SUBDIR, ACME_CERTIFICATE_FILE_NAME, ACME_DIR_URL, ACME_PRIVATE_KEY_FILE_NAME, ACME_REGISTRY_PATH},
  error::RpxyAcmeError,
  log::*,
};

#[derive(Debug)]
/// ACME settings
pub struct AcmeTargets {
  /// ACME account email
  pub email: String,
  /// ACME directory url
  pub acme_dir_url: Url,
  /// ACME registry path that stores account key and certificate
  pub acme_registry_path: PathBuf,
  /// ACME accounts directory, subdirectory of ACME_REGISTRY_PATH
  pub acme_accounts_dir: PathBuf,
  /// ACME target info map
  pub acme_targets: HashMap<String, AcmeTargetInfo>,
}

#[derive(Debug)]
/// ACME settings for each server name
pub struct AcmeTargetInfo {
  /// Server name
  pub server_name: String,
  /// private key path
  pub private_key_path: PathBuf,
  /// certificate path
  pub certificate_path: PathBuf,
}

impl AcmeTargets {
  /// Create a new instance
  pub fn try_new(email: &str, acme_dir_url: Option<&str>, acme_registry_path: Option<&str>) -> Result<Self, RpxyAcmeError> {
    let acme_dir_url = Url::parse(acme_dir_url.unwrap_or(ACME_DIR_URL))?;
    let acme_registry_path = acme_registry_path.map_or_else(|| PathBuf::from(ACME_REGISTRY_PATH), PathBuf::from);
    if acme_registry_path.exists() && !acme_registry_path.is_dir() {
      return Err(RpxyAcmeError::InvalidAcmeRegistryPath);
    }
    let acme_account_dir = acme_registry_path.join(ACME_ACCOUNT_SUBDIR);
    if acme_account_dir.exists() && !acme_account_dir.is_dir() {
      return Err(RpxyAcmeError::InvalidAcmeRegistryPath);
    }
    std::fs::create_dir_all(&acme_account_dir)?;

    Ok(Self {
      email: email.to_owned(),
      acme_dir_url,
      acme_registry_path,
      acme_accounts_dir: acme_account_dir,
      acme_targets: HashMap::default(),
    })
  }

  /// Add a new target
  /// Write dummy cert and key files if not exists
  pub fn add_target(&mut self, server_name: &str) -> Result<(), RpxyAcmeError> {
    info!("Adding ACME target: {}", server_name);
    let parent_dir = self.acme_registry_path.join(server_name);
    let private_key_path = parent_dir.join(ACME_PRIVATE_KEY_FILE_NAME);
    let certificate_path = parent_dir.join(ACME_CERTIFICATE_FILE_NAME);

    if !parent_dir.exists() {
      warn!("Creating ACME target directory: {}", parent_dir.display());
      std::fs::create_dir_all(parent_dir)?;
    }

    self.acme_targets.insert(
      server_name.to_owned(),
      AcmeTargetInfo {
        server_name: server_name.to_owned(),
        private_key_path,
        certificate_path,
      },
    );
    Ok(())
  }
}
