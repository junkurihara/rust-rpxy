use crate::{
  constants::{ACME_DIR_URL, ACME_REGISTRY_PATH},
  dir_cache::DirCache,
  error::RpxyAcmeError,
  log::*,
};
use rustc_hash::FxHashMap as HashMap;
// use rustls_acme::AcmeConfig;
use std::path::PathBuf;
use url::Url;

#[derive(Debug)]
/// ACME settings
pub struct AcmeContexts {
  /// ACME directory url
  acme_dir_url: Url,
  /// ACME registry directory
  acme_registry_dir: PathBuf,
  /// ACME contacts
  contacts: Vec<String>,
  /// ACME directly cache information
  inner: HashMap<String, DirCache>,
}

impl AcmeContexts {
  /// Create a new instance. Note that for each domain, a new AcmeConfig is created.
  /// This means that for each domain, a distinct operation will be dispatched and separated certificates will be generated.
  pub fn try_new(
    acme_dir_url: Option<&str>,
    acme_registry_dir: Option<&str>,
    contacts: &[String],
    domains: &[String],
  ) -> Result<Self, RpxyAcmeError> {
    // Install aws_lc_rs as default crypto provider for rustls
    let _ = rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());

    let acme_registry_dir = acme_registry_dir
      .map(|v| v.to_ascii_lowercase())
      .map_or_else(|| PathBuf::from(ACME_REGISTRY_PATH), PathBuf::from);
    if acme_registry_dir.exists() && !acme_registry_dir.is_dir() {
      return Err(RpxyAcmeError::InvalidAcmeRegistryPath);
    }
    let acme_dir_url = acme_dir_url
      .map(|v| v.to_ascii_lowercase())
      .as_deref()
      .map_or_else(|| Url::parse(ACME_DIR_URL), Url::parse)?;
    let contacts = contacts.iter().map(|email| format!("mailto:{email}")).collect::<Vec<_>>();
    // let rustls_client_config = rustls::ClientConfig::builder()
    //   .dangerous() // The `Verifier` we're using is actually safe
    //   .with_custom_certificate_verifier(std::sync::Arc::new(rustls_platform_verifier::Verifier::new()))
    //   .with_no_client_auth();
    // let rustls_client_config = Arc::new(rustls_client_config);

    let inner = domains
      .iter()
      .map(|domain| {
        let domain = domain.to_ascii_lowercase();
        let dir_cache = DirCache::new(&acme_registry_dir, &domain);
        (domain, dir_cache)
      })
      .collect::<HashMap<_, _>>();
    // let inner = domains
    //   .iter()
    //   .map(|domain| {
    //     let dir_cache = DirCache::new(&acme_registry_dir, domain);
    //     let config = AcmeConfig::new([domain])
    //       .contact(&contacts)
    //       .cache(dir_cache)
    //       .directory(acme_dir_url.as_str())
    //       .client_tls_config(rustls_client_config.clone());
    //     let config = Box::new(config);
    //     (domain.to_ascii_lowercase(), config)
    //   })
    //   .collect::<HashMap<_, _>>();

    Ok(Self {
      acme_dir_url,
      acme_registry_dir,
      contacts,
      inner,
    })
  }
}

#[cfg(test)]
mod tests {
  use crate::constants::ACME_ACCOUNT_SUBDIR;

  use super::*;

  #[test]
  fn test_try_new() {
    let acme_dir_url = "https://acme.example.com/directory";
    let acme_registry_dir = "/tmp/acme";
    let contacts = vec!["test@example.com".to_string()];
    let acme_contexts: AcmeContexts = AcmeContexts::try_new(
      Some(acme_dir_url),
      Some(acme_registry_dir),
      &contacts,
      &["example.com".to_string(), "example.org".to_string()],
    )
    .unwrap();
    assert_eq!(acme_contexts.inner.len(), 2);
    assert_eq!(acme_contexts.contacts, vec!["mailto:test@example.com".to_string()]);
    assert_eq!(acme_contexts.acme_dir_url.as_str(), acme_dir_url);
    assert_eq!(acme_contexts.acme_registry_dir, PathBuf::from(acme_registry_dir));
    assert_eq!(
      acme_contexts.inner["example.com"],
      DirCache {
        account_dir: PathBuf::from(acme_registry_dir).join(ACME_ACCOUNT_SUBDIR),
        cert_dir: PathBuf::from(acme_registry_dir).join("example.com"),
      }
    );
    assert_eq!(
      acme_contexts.inner["example.org"],
      DirCache {
        account_dir: PathBuf::from(acme_registry_dir).join(ACME_ACCOUNT_SUBDIR),
        cert_dir: PathBuf::from(acme_registry_dir).join("example.org"),
      }
    );
  }
}
