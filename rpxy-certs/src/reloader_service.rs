use crate::{
  certs::SingleServerCertsKeys,
  crypto_source::CryptoSource,
  error::*,
  log::*,
  server_crypto::{ServerCryptoBase, ServerNameBytes},
};
use ahash::HashMap;
use async_trait::async_trait;
use hot_reload::{Reload, ReloaderError};
use std::sync::{Arc, Mutex};

/* ------------------------------------------------ */
/// Boxed CryptoSource trait object with Send and Sync
/// TODO: support for not only `CryptoFileSource` but also other type of sources
pub(super) type DynCryptoSource = dyn CryptoSource<Error = RpxyCertError> + Send + Sync + 'static;

#[derive(Clone)]
/// Reloader service for certificates and keys for TLS
pub struct CryptoReloader {
  inner: HashMap<ServerNameBytes, Arc<Box<DynCryptoSource>>>,
  /// Last successfully read certs and keys per server name. Retained so a transient read
  /// failure during reload keeps serving the previously loaded certificate instead of dropping
  /// the domain from the active SNI map until the next successful reload.
  last_good: Arc<Mutex<HashMap<ServerNameBytes, SingleServerCertsKeys>>>,
}

/// Locks the last-good cache, recovering the guard if the mutex was poisoned. The cache is a
/// best-effort retention store, so a poisoned lock is preferable to a panic on the reload path.
fn lock_last_good(
  cache: &Mutex<HashMap<ServerNameBytes, SingleServerCertsKeys>>,
) -> std::sync::MutexGuard<'_, HashMap<ServerNameBytes, SingleServerCertsKeys>> {
  cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl<T> Extend<(ServerNameBytes, T)> for CryptoReloader
where
  T: CryptoSource<Error = RpxyCertError> + Send + Sync + 'static,
{
  fn extend<I: IntoIterator<Item = (ServerNameBytes, T)>>(&mut self, iter: I) {
    let iter = iter
      .into_iter()
      .map(|(k, v)| (k, Arc::new(Box::new(v) as Box<DynCryptoSource>)));
    self.inner.extend(iter);
  }
}

#[async_trait]
impl Reload<ServerCryptoBase> for CryptoReloader {
  type Source = HashMap<ServerNameBytes, Arc<Box<DynCryptoSource>>>;

  async fn new(source: &Self::Source) -> Result<Self, ReloaderError<ServerCryptoBase>> {
    let mut inner = HashMap::default();
    inner.extend(source.clone());
    Ok(Self {
      inner,
      last_good: Arc::new(Mutex::new(HashMap::default())),
    })
  }

  async fn reload(&self) -> Result<Option<ServerCryptoBase>, ReloaderError<ServerCryptoBase>> {
    let mut server_crypto_base = ServerCryptoBase::default();

    for (server_name_bytes, crypto_source) in self.inner.iter() {
      let server_name = String::from_utf8_lossy(server_name_bytes);
      let certs_keys = match crypto_source.read().await {
        Ok(certs_keys) => {
          lock_last_good(&self.last_good).insert(server_name_bytes.clone(), certs_keys.clone());
          certs_keys
        }
        Err(e) => {
          let retained = lock_last_good(&self.last_good).get(server_name_bytes).cloned();
          match retained {
            Some(retained) => {
              warn!(server_name = %server_name, "Failed to read certs and keys, keeping the previously loaded certificate: {}", e);
              retained
            }
            None => {
              error!(server_name = %server_name, "Failed to read certs and keys, skip at this time: {}", e);
              continue;
            }
          }
        }
      };
      server_crypto_base.inner.insert(server_name_bytes.clone(), certs_keys);
    }

    Ok(Some(server_crypto_base))
  }
}
/* ------------------------------------------------ */

#[cfg(test)]
mod tests {
  use super::*;
  use crate::crypto_source::CryptoFileSourceBuilder;
  use std::path::Path;

  #[tokio::test]
  async fn test_crypto_reloader() {
    let tls_cert_path = "../example-certs/server.crt";
    let tls_cert_key_path = "../example-certs/server.key";
    let client_ca_cert_path = Some("../example-certs/client.ca.crt");

    let mut crypto_reloader = CryptoReloader::new(&HashMap::default()).await.unwrap();
    let crypto_source = CryptoFileSourceBuilder::default()
      .tls_cert_path(tls_cert_path)
      .tls_cert_key_path(tls_cert_key_path)
      .client_ca_cert_path(client_ca_cert_path)
      .build()
      .unwrap();
    crypto_reloader.extend(vec![(b"localhost".to_vec(), crypto_source)]);

    let server_crypto_base = crypto_reloader.reload().await.unwrap().unwrap();
    assert_eq!(server_crypto_base.inner.len(), 1);
  }

  /// Build a reloader watching cert/key/client-CA copies inside `dir`, so the test can mutate
  /// the files to drive read success and failure across reload cycles.
  async fn reloader_from_dir(dir: &Path) -> CryptoReloader {
    let mut crypto_reloader = CryptoReloader::new(&HashMap::default()).await.unwrap();
    let crypto_source = CryptoFileSourceBuilder::default()
      .tls_cert_path(dir.join("server.crt"))
      .tls_cert_key_path(dir.join("server.key"))
      .client_ca_cert_path(Some(dir.join("client.ca.crt")))
      .build()
      .unwrap();
    crypto_reloader.extend(vec![(b"localhost".to_vec(), crypto_source)]);
    crypto_reloader
  }

  /// A transient read failure must not drop the domain: the reloader keeps serving the
  /// previously loaded certificate until a later read succeeds again.
  #[tokio::test]
  async fn reload_retains_last_good_on_transient_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::copy("../example-certs/server.crt", dir.join("server.crt")).unwrap();
    std::fs::copy("../example-certs/server.key", dir.join("server.key")).unwrap();
    std::fs::copy("../example-certs/client.ca.crt", dir.join("client.ca.crt")).unwrap();

    let crypto_reloader = reloader_from_dir(dir).await;

    // First reload succeeds and populates the last-good cache.
    let first = crypto_reloader.reload().await.unwrap().unwrap();
    assert_eq!(first.inner.len(), 1);
    let loaded = first.inner.get(b"localhost".as_slice()).unwrap().clone();

    // Remove the certificate so the next read fails.
    std::fs::remove_file(dir.join("server.crt")).unwrap();
    let second = crypto_reloader.reload().await.unwrap().unwrap();

    // The domain is retained with the previously loaded certificate, not dropped.
    assert_eq!(second.inner.len(), 1);
    assert_eq!(second.inner.get(b"localhost".as_slice()).unwrap(), &loaded);
  }

  /// When a server name has never loaded successfully, a read failure has nothing to retain and
  /// the domain is skipped, preserving the original startup behavior.
  #[tokio::test]
  async fn reload_skips_when_never_loaded() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    // Intentionally do not create any of the cert files, so the first read fails.

    let crypto_reloader = reloader_from_dir(dir).await;

    let base = crypto_reloader.reload().await.unwrap().unwrap();
    assert!(base.inner.is_empty());
  }

  /// Retention must never pin a domain to a stale certificate: once a read succeeds again after
  /// a failure, the reloader propagates the new certificate and refreshes the cache.
  #[tokio::test]
  async fn reload_propagates_new_value_after_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::copy("../example-certs/server.crt", dir.join("server.crt")).unwrap();
    std::fs::copy("../example-certs/server.key", dir.join("server.key")).unwrap();
    std::fs::copy("../example-certs/client.ca.crt", dir.join("client.ca.crt")).unwrap();

    let crypto_reloader = reloader_from_dir(dir).await;

    // First successful load (mutual TLS with the original client CA).
    let first = crypto_reloader.reload().await.unwrap().unwrap();
    let original = first.inner.get(b"localhost".as_slice()).unwrap().clone();

    // Force a transient failure so the cache is exercised on the next cycle.
    std::fs::remove_file(dir.join("server.crt")).unwrap();
    let retained = crypto_reloader.reload().await.unwrap().unwrap();
    assert_eq!(retained.inner.get(b"localhost".as_slice()).unwrap(), &original);

    // Restore the server cert and swap the client CA file for a different valid certificate, so
    // the freshly read materials genuinely differ from the retained ones.
    std::fs::copy("../example-certs/server.crt", dir.join("server.crt")).unwrap();
    std::fs::copy("../example-certs/server.crt", dir.join("client.ca.crt")).unwrap();
    let updated = crypto_reloader.reload().await.unwrap().unwrap();

    let new_value = updated.inner.get(b"localhost".as_slice()).unwrap();
    assert_ne!(new_value, &original, "a successful read must replace the retained value");
  }
}
