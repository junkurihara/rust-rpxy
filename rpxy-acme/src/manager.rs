use crate::{
  constants::{ACME_DIR_URL, ACME_REGISTRY_PATH},
  dir_cache::DirCache,
  error::RpxyAcmeError,
  log::*,
};
use ahash::HashMap;
use rustls::ServerConfig;
use rustls_acme::AcmeConfig;
use std::{path::PathBuf, sync::Arc};
use tokio::runtime::Handle;
use tokio_stream::StreamExt;
use url::Url;

#[derive(Debug, Clone)]
/// ACME settings
pub struct AcmeManager {
  /// ACME directory url
  acme_dir_url: Url,
  // /// ACME registry directory
  // acme_registry_dir: PathBuf,
  /// ACME contacts
  contacts: Vec<String>,
  /// ACME directly cache information
  inner: HashMap<String, DirCache>,
  /// Tokio runtime handle
  runtime_handle: Handle,
}

impl AcmeManager {
  /// Create a new instance. Note that for each domain, a new AcmeConfig is created.
  /// This means that for each domain, a distinct operation will be dispatched and separated certificates will be generated.
  pub fn try_new(
    acme_dir_url: Option<&str>,
    acme_registry_dir: Option<&str>,
    contacts: &[String],
    domains: &[String],
    runtime_handle: Handle,
  ) -> Result<Self, RpxyAcmeError> {
    #[cfg(not(feature = "post-quantum"))]
    // Install aws_lc_rs as default crypto provider for rustls
    let _ = rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());
    #[cfg(feature = "post-quantum")]
    let _ = rustls::crypto::CryptoProvider::install_default(rustls_post_quantum::provider());

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

    let inner = domains
      .iter()
      .map(|domain| {
        let domain = domain.to_ascii_lowercase();
        let dir_cache = DirCache::new(&acme_registry_dir, &domain);
        (domain, dir_cache)
      })
      .collect::<HashMap<_, _>>();

    Ok(Self {
      acme_dir_url,
      // acme_registry_dir,
      contacts,
      inner,
      runtime_handle,
    })
  }

  /// Start ACME manager to manage certificates for each domain.
  /// Returns a Vec<JoinHandle<()>> as a tasks handles and a map of domain to ServerConfig for challenge.
  pub fn spawn_manager_tasks(
    &self,
    cancel_token: tokio_util::sync::CancellationToken,
  ) -> (Vec<tokio::task::JoinHandle<()>>, HashMap<String, Arc<ServerConfig>>) {
    let rustls_client_config = Self::create_tls_client_config().expect("Failed to create TLS client configuration for ACME");

    let mut server_configs_for_challenge: HashMap<String, Arc<ServerConfig>> = HashMap::default();
    let join_handles = self
      .inner
      .clone()
      .into_iter()
      .map(|(domain, dir_cache)| {
        let config = AcmeConfig::new([&domain])
          .contact(&self.contacts)
          .cache(dir_cache.to_owned())
          .directory(self.acme_dir_url.as_str())
          .client_tls_config(rustls_client_config.clone());
        let mut state = config.state();
        server_configs_for_challenge.insert(domain.to_ascii_lowercase(), state.challenge_rustls_config());
        self.runtime_handle.spawn({
          let cancel_token = cancel_token.clone();
          async move {
            info!("rpxy ACME manager task for {domain} started");
            // infinite loop unless the return value is None
            let task = async {
              loop {
                let Some(res) = state.next().await else {
                  error!("rpxy ACME manager task for {domain} exited");
                  break;
                };
                match res {
                  Ok(ok) => info!("rpxy ACME event: {ok:?}"),
                  Err(err) => error!("rpxy ACME error: {err:?}"),
                }
              }
            };

            tokio::select! {
              _ = task => {},
              _ = cancel_token.cancelled() => { debug!("rpxy ACME manager task for {domain} terminated") }
            }
          }
        })
      })
      .collect::<Vec<_>>();

    (join_handles, server_configs_for_challenge)
  }

  /// Creates a TLS client configuration with platform certificate verification.
  ///
  /// This configuration uses the system's certificate store for verification,
  /// which is appropriate for ACME certificate validation.
  fn create_tls_client_config() -> Result<Arc<rustls::ClientConfig>, RpxyAcmeError> {
    let crypto_provider = rustls::crypto::CryptoProvider::get_default().ok_or(RpxyAcmeError::TlsClientConfig(
      "No default crypto provider available".to_string(),
    ))?;

    let verifier = rustls_platform_verifier::Verifier::new(crypto_provider.clone())
      .map_err(|e| RpxyAcmeError::TlsClientConfig(format!("Failed to create certificate verifier: {}", e)))?;

    let client_config = rustls::ClientConfig::builder()
      .dangerous() // Safe: using platform certificate verifier
      .with_custom_certificate_verifier(Arc::new(verifier))
      .with_no_client_auth();

    Ok(Arc::new(client_config))
  }
}

#[cfg(test)]
mod tests {
  use crate::constants::ACME_ACCOUNT_SUBDIR;

  use super::*;

  #[tokio::test]
  async fn test_try_new() {
    let acme_dir_url = "https://acme.example.com/directory";
    let acme_registry_dir = "/tmp/acme";
    let contacts = vec!["test@example.com".to_string()];
    let handle = Handle::current();
    let acme_contexts: AcmeManager = AcmeManager::try_new(
      Some(acme_dir_url),
      Some(acme_registry_dir),
      &contacts,
      &["example.com".to_string(), "example.org".to_string()],
      handle,
    )
    .unwrap();
    assert_eq!(acme_contexts.inner.len(), 2);
    assert_eq!(acme_contexts.contacts, vec!["mailto:test@example.com".to_string()]);
    assert_eq!(acme_contexts.acme_dir_url.as_str(), acme_dir_url);
    // assert_eq!(acme_contexts.acme_registry_dir, PathBuf::from(acme_registry_dir));
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
