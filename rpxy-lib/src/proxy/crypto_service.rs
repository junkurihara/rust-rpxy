use crate::{
  certs::{CertsAndKeys, CryptoSource},
  globals::Globals,
  log::*,
  utils::ServerNameBytesExp,
};
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
  globals: Arc<Globals<T>>,
}

pub type SniServerCryptoMap = HashMap<ServerNameBytesExp, Arc<ServerConfig>>;
pub struct ServerCrypto {
  // For Quic/HTTP3, only servers with no client authentication
  pub inner_global_no_client_auth: Arc<ServerConfig>,
  // For TLS over TCP/HTTP2 and 1.1, map of SNI to server_crypto for all given servers
  pub inner_local_map: Arc<SniServerCryptoMap>,
}

/// Reloader target for the certificate reloader service
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct ServerCryptoBase {
  inner: HashMap<ServerNameBytesExp, CertsAndKeys>,
}

#[async_trait]
impl<T> Reload<ServerCryptoBase> for CryptoReloader<T>
where
  T: CryptoSource + Sync + Send,
{
  type Source = Arc<Globals<T>>;
  async fn new(source: &Self::Source) -> Result<Self, ReloaderError<ServerCryptoBase>> {
    Ok(Self {
      globals: source.clone(),
    })
  }

  async fn reload(&self) -> Result<Option<ServerCryptoBase>, ReloaderError<ServerCryptoBase>> {
    let mut certs_and_keys_map = ServerCryptoBase::default();

    for (server_name_bytes_exp, backend) in self.globals.backends.apps.iter() {
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
    let mut resolver_global = ResolvesServerCertUsingSni::new();
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
        error!(
          "{}: Failed to read some certificates and keys {}",
          server_name.as_str(),
          e
        )
      }

      // add client certificate if specified
      if certs_and_keys.client_ca_certs.is_none() {
        // aggregated server config for no client auth server for http3
        if let Err(e) = resolver_global.add(server_name.as_str(), certified_key) {
          error!(
            "{}: Failed to read some certificates and keys {}",
            server_name.as_str(),
            e
          )
        }
      } else {
        // add client certificate if specified
        match certs_and_keys.parse_client_ca_certs() {
          Ok((owned_trust_anchors, _subject_key_ids)) => {
            client_ca_roots_local.add_server_trust_anchors(owned_trust_anchors.into_iter());
          }
          Err(e) => {
            warn!(
              "Failed to add client CA certificate for {}: {}",
              server_name.as_str(),
              e
            );
          }
        }
      }

      let mut server_config_local = if client_ca_roots_local.is_empty() {
        // with no client auth, enable http1.1 -- 3
        #[cfg(not(feature = "http3"))]
        {
          ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver_local))
        }
        #[cfg(feature = "http3")]
        {
          let mut sc = ServerConfig::builder()
            .with_safe_defaults()
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(resolver_local));
          sc.alpn_protocols = vec![b"h3".to_vec(), b"hq-29".to_vec()]; // TODO: remove hq-29 later?
          sc
        }
      } else {
        // with client auth, enable only http1.1 and 2
        // let client_certs_verifier = rustls::server::AllowAnyAnonymousOrAuthenticatedClient::new(client_ca_roots);
        let client_certs_verifier = rustls::server::AllowAnyAuthenticatedClient::new(client_ca_roots_local);
        ServerConfig::builder()
          .with_safe_defaults()
          .with_client_cert_verifier(Arc::new(client_certs_verifier))
          .with_cert_resolver(Arc::new(resolver_local))
      };
      server_config_local.alpn_protocols.push(b"h2".to_vec());
      server_config_local.alpn_protocols.push(b"http/1.1".to_vec());

      server_crypto_local_map.insert(server_name_bytes_exp.to_owned(), Arc::new(server_config_local));
    }

    //////////////
    let mut server_crypto_global = ServerConfig::builder()
      .with_safe_defaults()
      .with_no_client_auth()
      .with_cert_resolver(Arc::new(resolver_global));

    //////////////////////////////

    #[cfg(feature = "http3")]
    {
      server_crypto_global.alpn_protocols = vec![
        b"h3".to_vec(),
        b"hq-29".to_vec(), // TODO: remove later?
        b"h2".to_vec(),
        b"http/1.1".to_vec(),
      ];
    }
    #[cfg(not(feature = "http3"))]
    {
      server_crypto_global.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }

    Ok(Arc::new(ServerCrypto {
      inner_global_no_client_auth: Arc::new(server_crypto_global),
      inner_local_map: Arc::new(server_crypto_local_map),
    }))
  }
}
