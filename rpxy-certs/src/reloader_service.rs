use crate::{
  crypto_source::CryptoSource,
  error::*,
  log::*,
  server_crypto::{ServerCryptoBase, ServerNameBytes},
};
use ahash::HashMap;
use async_trait::async_trait;
use hot_reload::{Reload, ReloaderError};
use std::sync::Arc;

/* ------------------------------------------------ */
/// Boxed CryptoSource trait object with Send and Sync
/// TODO: support for not only `CryptoFileSource` but also other type of sources
pub(super) type DynCryptoSource = dyn CryptoSource<Error = RpxyCertError> + Send + Sync + 'static;

#[derive(Clone)]
/// Reloader service for certificates and keys for TLS
pub struct CryptoReloader {
  inner: HashMap<ServerNameBytes, Arc<Box<DynCryptoSource>>>,
  tls_0rtt: bool,
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

pub struct ServerCryptoSource {
  pub(super) inner: HashMap<ServerNameBytes, Arc<Box<DynCryptoSource>>>,
  pub(super) tls_0rtt: bool,
}

#[async_trait]
impl Reload<ServerCryptoBase> for CryptoReloader {
  type Source = ServerCryptoSource;

  async fn new(source: &Self::Source) -> Result<Self, ReloaderError<ServerCryptoBase>> {
    let mut inner = HashMap::default();
    inner.extend(source.inner.clone());
    Ok(Self { inner, tls_0rtt: source.tls_0rtt, })
  }

  async fn reload(&self) -> Result<Option<ServerCryptoBase>, ReloaderError<ServerCryptoBase>> {
    let mut server_crypto_base = ServerCryptoBase::default();
    server_crypto_base.tls_0rtt = self.tls_0rtt;

    for (server_name_bytes, crypto_source) in self.inner.iter() {
      let certs_keys = match crypto_source.read().await {
        Ok(certs_keys) => certs_keys,
        Err(e) => {
          error!("Failed to read certs and keys, skip at this time: {}", e);
          continue;
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

  #[tokio::test]
  async fn test_crypto_reloader() {
    let tls_cert_path = "../example-certs/server.crt";
    let tls_cert_key_path = "../example-certs/server.key";
    let client_ca_cert_path = Some("../example-certs/client.ca.crt");

    let server_crypto_source = ServerCryptoSource { inner: HashMap::default(), tls_0rtt: false };
    let mut crypto_reloader = CryptoReloader::new(&server_crypto_source).await.unwrap();
    let crypto_source = CryptoFileSourceBuilder::default()
      .tls_cert_path(tls_cert_path)
      .tls_cert_key_path(tls_cert_key_path)
      .client_ca_cert_path(client_ca_cert_path)
      .build()
      .unwrap();
    crypto_reloader.extend(vec![(b"localhost".to_vec(), crypto_source)]);

    let server_crypto_base = crypto_reloader.reload().await.unwrap().unwrap();
    assert_eq!(server_crypto_base.inner.len(), 1);
    assert_eq!(server_crypto_base.tls_0rtt, false);
  }
}
