use crate::dir_cache::DirCache;
use crate::{
  constants::{ACME_DIR_URL, ACME_REGISTRY_PATH},
  error::RpxyAcmeError,
  log::*,
};
use rustc_hash::FxHashMap as HashMap;
use rustls_acme::AcmeConfig;
use std::{fmt::Debug, path::PathBuf, sync::Arc};
use url::Url;

#[derive(Debug)]
/// ACME settings
pub struct AcmeContexts<EC, EA = EC>
where
  EC: Debug + 'static,
  EA: Debug + 'static,
{
  /// ACME directory url
  acme_dir_url: Url,
  /// ACME registry directory
  acme_registry_dir: PathBuf,
  /// ACME contacts
  contacts: Vec<String>,
  /// ACME config
  inner: HashMap<String, Box<AcmeConfig<EC, EA>>>,
}

impl AcmeContexts<std::io::Error> {
  /// Create a new instance. Note that for each domain, a new AcmeConfig is created.
  /// This means that for each domain, a distinct operation will be dispatched and separated certificates will be generated.
  pub fn try_new(
    acme_dir_url: Option<&str>,
    acme_registry_dir: Option<&str>,
    contacts: &[String],
    domains: &[String],
  ) -> Result<Self, RpxyAcmeError> {
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
    let rustls_client_config = rustls::ClientConfig::builder()
      .dangerous() // The `Verifier` we're using is actually safe
      .with_custom_certificate_verifier(std::sync::Arc::new(rustls_platform_verifier::Verifier::new()))
      .with_no_client_auth();
    let rustls_client_config = Arc::new(rustls_client_config);

    let inner = domains
      .iter()
      .map(|domain| {
        let dir_cache = DirCache::new(&acme_registry_dir, domain);
        let config = AcmeConfig::new([domain])
          .contact(&contacts)
          .cache(dir_cache)
          .directory(acme_dir_url.as_str())
          .client_tls_config(rustls_client_config.clone());
        let config = Box::new(config);
        (domain.to_ascii_lowercase(), config)
      })
      .collect::<HashMap<_, _>>();

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
  use super::*;

  #[test]
  fn test_try_new() {
    let acme_dir_url = "https://acme.example.com/directory";
    let acme_registry_dir = "/tmp/acme";
    let contacts = vec!["test@example.com".to_string()];
    let acme_contexts: AcmeContexts<std::io::Error> = AcmeContexts::try_new(
      Some(acme_dir_url),
      Some(acme_registry_dir),
      &contacts,
      &["example.com".to_string(), "example.org".to_string()],
    )
    .unwrap();
    println!("{:#?}", acme_contexts);
  }
}
