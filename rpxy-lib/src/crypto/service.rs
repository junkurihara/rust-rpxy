use super::certs::{CertsAndKeys, CryptoSource};
use crate::{backend::BackendAppManager, log::*, name_exp::ServerName};
use async_trait::async_trait;
use hot_reload::*;
use rustc_hash::FxHashMap as HashMap;
use rustls::{server::ResolvesServerCertUsingSni, sign::CertifiedKey, RootCertStore, ServerConfig};
use std::sync::Arc;

#[derive(Clone)]
/// Reloader service for certificates and keys for TLS
pub struct CryptoReloader<T>
where
  T: CryptoSource,
{
  inner: Arc<BackendAppManager<T>>,
}

/// SNI to ServerConfig map type
pub type SniServerCryptoMap = HashMap<ServerName, Arc<ServerConfig>>;
/// SNI to ServerConfig map
pub struct ServerCrypto {
  // For Quic/HTTP3, only servers with no client authentication
  #[cfg(feature = "http3-quinn")]
  pub inner_global_no_client_auth: Arc<ServerConfig>,
  #[cfg(all(feature = "http3-s2n", not(feature = "http3-quinn")))]
  pub inner_global_no_client_auth: s2n_quic_rustls::Server,
  // For TLS over TCP/HTTP2 and 1.1, map of SNI to server_crypto for all given servers
  pub inner_local_map: Arc<SniServerCryptoMap>,
}

/// Reloader target for the certificate reloader service
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct ServerCryptoBase {
  inner: HashMap<ServerName, CertsAndKeys>,
}

#[async_trait]
impl<T> Reload<ServerCryptoBase> for CryptoReloader<T>
where
  T: CryptoSource + Sync + Send,
{
  type Source = Arc<BackendAppManager<T>>;
  async fn new(source: &Self::Source) -> Result<Self, ReloaderError<ServerCryptoBase>> {
    Ok(Self { inner: source.clone() })
  }

  async fn reload(&self) -> Result<Option<ServerCryptoBase>, ReloaderError<ServerCryptoBase>> {
    let mut certs_and_keys_map = ServerCryptoBase::default();

    for (server_name_bytes_exp, backend) in self.inner.apps.iter() {
      if let Some(crypto_source) = &backend.crypto_source {
        let certs_and_keys = crypto_source
          .read()
          .await
          .map_err(|_e| ReloaderError::<ServerCryptoBase>::Reload("Failed to reload cert, key or ca cert"))?;
        certs_and_keys_map
          .inner
          .insert(server_name_bytes_exp.to_owned(), certs_and_keys);
      }
    }

    Ok(Some(certs_and_keys_map))
  }
}

impl TryInto<Arc<ServerCrypto>> for &ServerCryptoBase {
  type Error = anyhow::Error;

  fn try_into(self) -> Result<Arc<ServerCrypto>, Self::Error> {
    #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
    let server_crypto_global = self.build_server_crypto_global()?;
    let server_crypto_local_map: SniServerCryptoMap = self.build_server_crypto_local_map()?;

    Ok(Arc::new(ServerCrypto {
      #[cfg(feature = "http3-quinn")]
      inner_global_no_client_auth: Arc::new(server_crypto_global),
      #[cfg(all(feature = "http3-s2n", not(feature = "http3-quinn")))]
      inner_global_no_client_auth: server_crypto_global,
      inner_local_map: Arc::new(server_crypto_local_map),
    }))
  }
}

impl ServerCryptoBase {
  fn build_server_crypto_local_map(&self) -> Result<SniServerCryptoMap, ReloaderError<ServerCryptoBase>> {
    let mut server_crypto_local_map: SniServerCryptoMap = HashMap::default();

    for (server_name_bytes_exp, certs_and_keys) in self.inner.iter() {
      let server_name: String = server_name_bytes_exp.try_into()?;

      // Parse server certificates and private keys
      let Ok(certified_key): Result<CertifiedKey, _> = certs_and_keys.parse_server_certs_and_keys() else {
        warn!("Failed to add certificate for {}", server_name);
        continue;
      };

      let mut resolver_local = ResolvesServerCertUsingSni::new();
      let mut client_ca_roots_local = RootCertStore::empty();

      // add server certificate and key
      if let Err(e) = resolver_local.add(server_name.as_str(), certified_key.to_owned()) {
        error!("{}: Failed to read some certificates and keys {}", server_name.as_str(), e)
      }

      // add client certificate if specified
      if certs_and_keys.client_ca_certs.is_some() {
        // add client certificate if specified
        match certs_and_keys.parse_client_ca_certs() {
          Ok((owned_trust_anchors, _subject_key_ids)) => {
            client_ca_roots_local.extend(owned_trust_anchors);
            // client_ca_roots_local.add_trust_anchors(owned_trust_anchors.into_iter());
          }
          Err(e) => {
            warn!("Failed to add client CA certificate for {}: {}", server_name.as_str(), e);
          }
        }
      }

      let mut server_config_local = if client_ca_roots_local.is_empty() {
        // with no client auth, enable http1.1 -- 3
        #[cfg(not(any(feature = "http3-quinn", feature = "http3-s2n")))]
        {
          ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver_local))
        }
        #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
        {
          let mut sc = ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver_local));
          sc.alpn_protocols = vec![b"h3".to_vec(), b"hq-29".to_vec()]; // TODO: remove hq-29 later?
          sc
        }
      } else {
        // with client auth, enable only http1.1 and 2
        // let client_certs_verifier = rustls::server::AllowAnyAnonymousOrAuthenticatedClient::new(client_ca_roots);
        let client_certs_verifier =
          match rustls::server::WebPkiClientVerifier::builder(Arc::new(client_ca_roots_local)).build() {
            Ok(v) => v,
            Err(e) => {
              warn!(
                "Failed to build client CA certificate verifier for {}: {}",
                server_name.as_str(),
                e
              );
              continue;
            }
          };
        ServerConfig::builder()
          .with_client_cert_verifier(client_certs_verifier)
          .with_cert_resolver(Arc::new(resolver_local))
      };
      server_config_local.alpn_protocols.push(b"h2".to_vec());
      server_config_local.alpn_protocols.push(b"http/1.1".to_vec());

      server_crypto_local_map.insert(server_name_bytes_exp.to_owned(), Arc::new(server_config_local));
    }
    Ok(server_crypto_local_map)
  }

  #[cfg(feature = "http3-quinn")]
  fn build_server_crypto_global(&self) -> Result<ServerConfig, ReloaderError<ServerCryptoBase>> {
    let mut resolver_global = ResolvesServerCertUsingSni::new();

    for (server_name_bytes_exp, certs_and_keys) in self.inner.iter() {
      let server_name: String = server_name_bytes_exp.try_into()?;

      // Parse server certificates and private keys
      let Ok(certified_key): Result<CertifiedKey, _> = certs_and_keys.parse_server_certs_and_keys() else {
        warn!("Failed to add certificate for {}", server_name);
        continue;
      };

      if certs_and_keys.client_ca_certs.is_none() {
        // aggregated server config for no client auth server for http3
        if let Err(e) = resolver_global.add(server_name.as_str(), certified_key) {
          error!("{}: Failed to read some certificates and keys {}", server_name.as_str(), e)
        }
      }
    }

    //////////////
    let mut server_crypto_global = ServerConfig::builder()
      .with_no_client_auth()
      .with_cert_resolver(Arc::new(resolver_global));

    //////////////////////////////

    server_crypto_global.alpn_protocols = vec![
      b"h3".to_vec(),
      b"hq-29".to_vec(), // TODO: remove later?
      b"h2".to_vec(),
      b"http/1.1".to_vec(),
    ];
    Ok(server_crypto_global)
  }

  #[cfg(all(feature = "http3-s2n", not(feature = "http3-quinn")))]
  fn build_server_crypto_global(&self) -> Result<s2n_quic_rustls::Server, ReloaderError<ServerCryptoBase>> {
    let mut resolver_global = s2n_quic_rustls::rustls::server::ResolvesServerCertUsingSni::new();

    for (server_name_bytes_exp, certs_and_keys) in self.inner.iter() {
      let server_name: String = server_name_bytes_exp.try_into()?;

      // Parse server certificates and private keys
      let Ok(certified_key) = parse_server_certs_and_keys_s2n(certs_and_keys) else {
        warn!("Failed to add certificate for {}", server_name);
        continue;
      };

      if certs_and_keys.client_ca_certs.is_none() {
        // aggregated server config for no client auth server for http3
        if let Err(e) = resolver_global.add(server_name.as_str(), certified_key) {
          error!("{}: Failed to read some certificates and keys {}", server_name.as_str(), e)
        }
      }
    }
    let alpn = [
      b"h3".to_vec(),
      b"hq-29".to_vec(), // TODO: remove later?
      b"h2".to_vec(),
      b"http/1.1".to_vec(),
    ];
    let server_crypto_global = s2n_quic::provider::tls::rustls::Server::builder()
      .with_cert_resolver(Arc::new(resolver_global))
      .map_err(|e| anyhow::anyhow!(e))?
      .with_application_protocols(alpn.iter())
      .map_err(|e| anyhow::anyhow!(e))?
      .build()
      .map_err(|e| anyhow::anyhow!(e))?;
    Ok(server_crypto_global)
  }
}

#[cfg(all(feature = "http3-s2n", not(feature = "http3-quinn")))]
/// This is workaround for the version difference between rustls and s2n-quic-rustls
fn parse_server_certs_and_keys_s2n(
  certs_and_keys: &CertsAndKeys,
) -> Result<s2n_quic_rustls::rustls::sign::CertifiedKey, anyhow::Error> {
  let signing_key = certs_and_keys
    .cert_keys
    .iter()
    .find_map(|k| {
      let s2n_private_key = s2n_quic_rustls::PrivateKey(k.0.clone());
      if let Ok(sk) = s2n_quic_rustls::rustls::sign::any_supported_type(&s2n_private_key) {
        Some(sk)
      } else {
        None
      }
    })
    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "Unable to find a valid certificate and key"))?;
  let certs: Vec<_> = certs_and_keys
    .certs
    .iter()
    .map(|c| s2n_quic_rustls::rustls::Certificate(c.0.clone()))
    .collect();
  Ok(s2n_quic_rustls::rustls::sign::CertifiedKey::new(certs, signing_key))
}
