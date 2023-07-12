use crate::{
  cert_file_reader::read_certs_and_keys, // TODO: Trait defining read_certs_and_keys and add struct implementing the trait to backend when build backend
  certs::{CertsAndKeys, CryptoSource},
  globals::Globals,
  log::*,
  utils::ServerNameBytesExp,
};
use async_trait::async_trait;
use hot_reload::*;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use rustls::{
  server::ResolvesServerCertUsingSni,
  sign::{any_supported_type, CertifiedKey},
  OwnedTrustAnchor, RootCertStore, ServerConfig,
};
use std::{io, sync::Arc};
use x509_parser::prelude::*;

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
      if backend.tls_cert_key_path.is_some() && backend.tls_cert_path.is_some() {
        let tls_cert_key_path = backend.tls_cert_key_path.as_ref().unwrap();
        let tls_cert_path = backend.tls_cert_path.as_ref().unwrap();
        let tls_client_ca_cert_path = backend.client_ca_cert_path.as_ref();
        let certs_and_keys = read_certs_and_keys(tls_cert_path, tls_cert_key_path, tls_client_ca_cert_path)
          .map_err(|_e| ReloaderError::<ServerCryptoBase>::Reload("Failed to reload cert, key or ca cert"))?;

        certs_and_keys_map
          .inner
          .insert(server_name_bytes_exp.to_owned(), certs_and_keys);
      }
    }

    Ok(Some(certs_and_keys_map))
  }
}

impl CertsAndKeys {
  fn parse_server_certs_and_keys(&self) -> Result<CertifiedKey, anyhow::Error> {
    // for (server_name_bytes_exp, certs_and_keys) in self.inner.iter() {
    let signing_key = self
      .cert_keys
      .iter()
      .find_map(|k| {
        if let Ok(sk) = any_supported_type(k) {
          Some(sk)
        } else {
          None
        }
      })
      .ok_or_else(|| {
        io::Error::new(
          io::ErrorKind::InvalidInput,
          "Unable to find a valid certificate and key",
        )
      })?;
    Ok(CertifiedKey::new(self.certs.clone(), signing_key))
  }

  pub fn parse_client_ca_certs(&self) -> Result<(Vec<OwnedTrustAnchor>, HashSet<Vec<u8>>), anyhow::Error> {
    let certs = self.client_ca_certs.as_ref().ok_or(anyhow::anyhow!("No client cert"))?;

    let owned_trust_anchors: Vec<_> = certs
      .iter()
      .map(|v| {
        // let trust_anchor = tokio_rustls::webpki::TrustAnchor::try_from_cert_der(&v.0).unwrap();
        let trust_anchor = webpki::TrustAnchor::try_from_cert_der(&v.0).unwrap();
        rustls::OwnedTrustAnchor::from_subject_spki_name_constraints(
          trust_anchor.subject,
          trust_anchor.spki,
          trust_anchor.name_constraints,
        )
      })
      .collect();

    // TODO: SKID is not used currently
    let subject_key_identifiers: HashSet<_> = certs
      .iter()
      .filter_map(|v| {
        // retrieve ca key id (subject key id)
        let cert = parse_x509_certificate(&v.0).unwrap().1;
        let subject_key_ids = cert
          .iter_extensions()
          .filter_map(|ext| match ext.parsed_extension() {
            ParsedExtension::SubjectKeyIdentifier(skid) => Some(skid),
            _ => None,
          })
          .collect::<Vec<_>>();
        if !subject_key_ids.is_empty() {
          Some(subject_key_ids[0].0.to_owned())
        } else {
          None
        }
      })
      .collect();

    Ok((owned_trust_anchors, subject_key_identifiers))
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
