mod upstream;
mod upstream_opts;

use crate::{
  log::*,
  utils::{BytesName, PathNameBytesExp, ServerNameBytesExp},
};
use derive_builder::Builder;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use rustls::{OwnedTrustAnchor, RootCertStore};
use std::{
  borrow::Cow,
  fs::File,
  io::{self, BufReader, Cursor, Read},
  path::PathBuf,
  sync::Arc,
};
use tokio_rustls::rustls::{
  server::ResolvesServerCertUsingSni,
  sign::{any_supported_type, CertifiedKey},
  Certificate, PrivateKey, ServerConfig,
};
pub use upstream::{ReverseProxy, Upstream, UpstreamGroup, UpstreamGroupBuilder};
pub use upstream_opts::UpstreamOption;
use x509_parser::prelude::*;

/// Struct serving information to route incoming connections, like server name to be handled and tls certs/keys settings.
#[derive(Builder)]
pub struct Backend {
  #[builder(setter(into))]
  /// backend application name, e.g., app1
  pub app_name: String,
  #[builder(setter(custom))]
  /// server name, e.g., example.com, in String ascii lower case
  pub server_name: String,
  /// struct of reverse proxy serving incoming request
  pub reverse_proxy: ReverseProxy,

  /// tls settings
  #[builder(setter(custom), default)]
  pub tls_cert_path: Option<PathBuf>,
  #[builder(setter(custom), default)]
  pub tls_cert_key_path: Option<PathBuf>,
  #[builder(default)]
  pub https_redirection: Option<bool>,
  #[builder(setter(custom), default)]
  pub client_ca_cert_path: Option<PathBuf>,
}
impl<'a> BackendBuilder {
  pub fn server_name(&mut self, server_name: impl Into<Cow<'a, str>>) -> &mut Self {
    self.server_name = Some(server_name.into().to_ascii_lowercase());
    self
  }
  pub fn tls_cert_path(&mut self, v: &Option<String>) -> &mut Self {
    self.tls_cert_path = Some(opt_string_to_opt_pathbuf(v));
    self
  }
  pub fn tls_cert_key_path(&mut self, v: &Option<String>) -> &mut Self {
    self.tls_cert_key_path = Some(opt_string_to_opt_pathbuf(v));
    self
  }
  pub fn client_ca_cert_path(&mut self, v: &Option<String>) -> &mut Self {
    self.client_ca_cert_path = Some(opt_string_to_opt_pathbuf(v));
    self
  }
}

fn opt_string_to_opt_pathbuf(input: &Option<String>) -> Option<PathBuf> {
  input.to_owned().as_ref().map(PathBuf::from)
}

impl Backend {
  pub fn read_certs_and_key(&self) -> io::Result<CertifiedKey> {
    debug!("Read TLS server certificates and private key");
    let (Some(certs_path), Some(certs_keys_path)) = (self.tls_cert_path.as_ref(), self.tls_cert_key_path.as_ref()) else {
      return Err(io::Error::new(io::ErrorKind::Other, "Invalid certs and keys paths"));
    };
    let certs: Vec<_> = {
      let certs_path_str = certs_path.display().to_string();
      let mut reader = BufReader::new(File::open(certs_path).map_err(|e| {
        io::Error::new(
          e.kind(),
          format!("Unable to load the certificates [{certs_path_str}]: {e}"),
        )
      })?);
      rustls_pemfile::certs(&mut reader)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Unable to parse the certificates"))?
    }
    .drain(..)
    .map(Certificate)
    .collect();
    let certs_keys: Vec<_> = {
      let certs_keys_path_str = certs_keys_path.display().to_string();
      let encoded_keys = {
        let mut encoded_keys = vec![];
        File::open(certs_keys_path)
          .map_err(|e| {
            io::Error::new(
              e.kind(),
              format!("Unable to load the certificate keys [{certs_keys_path_str}]: {e}"),
            )
          })?
          .read_to_end(&mut encoded_keys)?;
        encoded_keys
      };
      let mut reader = Cursor::new(encoded_keys);
      let pkcs8_keys = rustls_pemfile::pkcs8_private_keys(&mut reader).map_err(|_| {
        io::Error::new(
          io::ErrorKind::InvalidInput,
          "Unable to parse the certificates private keys (PKCS8)",
        )
      })?;
      reader.set_position(0);
      let mut rsa_keys = rustls_pemfile::rsa_private_keys(&mut reader)?;
      let mut keys = pkcs8_keys;
      keys.append(&mut rsa_keys);
      if keys.is_empty() {
        return Err(io::Error::new(
          io::ErrorKind::InvalidInput,
          "No private keys found - Make sure that they are in PKCS#8/PEM format",
        ));
      }
      keys.drain(..).map(PrivateKey).collect()
    };
    let signing_key = certs_keys
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
    Ok(CertifiedKey::new(certs, signing_key))
  }

  fn read_client_ca_certs(&self) -> io::Result<(Vec<OwnedTrustAnchor>, HashSet<Vec<u8>>)> {
    debug!("Read CA certificates for client authentication");
    // Reads client certificate and returns client
    let client_ca_cert_path = {
      let Some(c) = self.client_ca_cert_path.as_ref() else {
        return Err(io::Error::new(io::ErrorKind::Other, "Invalid certs and keys paths"));
      };
      c
    };
    let certs: Vec<_> = {
      let certs_path_str = client_ca_cert_path.display().to_string();
      let mut reader = BufReader::new(File::open(client_ca_cert_path).map_err(|e| {
        io::Error::new(
          e.kind(),
          format!("Unable to load the client certificates [{certs_path_str}]: {e}"),
        )
      })?);
      rustls_pemfile::certs(&mut reader)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Unable to parse the client certificates"))?
    }
    .drain(..)
    .map(Certificate)
    .collect();

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

/// HashMap and some meta information for multiple Backend structs.
pub struct Backends {
  pub apps: HashMap<ServerNameBytesExp, Backend>, // hyper::uriで抜いたhostで引っ掛ける
  pub default_server_name_bytes: Option<ServerNameBytesExp>, // for plaintext http
}

pub type SniServerCryptoMap = HashMap<ServerNameBytesExp, Arc<ServerConfig>>;
pub struct ServerCrypto {
  // For Quic/HTTP3, only servers with no client authentication
  pub inner_global_no_client_auth: Arc<ServerConfig>,
  // For TLS over TCP/HTTP2 and 1.1, map of SNI to server_crypto for all given servers
  pub inner_local_map: Arc<SniServerCryptoMap>,
}

impl Backends {
  pub async fn generate_server_crypto(&self) -> Result<ServerCrypto, anyhow::Error> {
    let mut resolver_global = ResolvesServerCertUsingSni::new();
    let mut server_crypto_local_map: SniServerCryptoMap = HashMap::default();

    for (server_name_bytes_exp, backend) in self.apps.iter() {
      if backend.tls_cert_key_path.is_some() && backend.tls_cert_path.is_some() {
        match backend.read_certs_and_key() {
          Ok(certified_key) => {
            let mut resolver_local = ResolvesServerCertUsingSni::new();
            let mut client_ca_roots_local = RootCertStore::empty();

            // add server certificate and key
            if let Err(e) = resolver_local.add(backend.server_name.as_str(), certified_key.to_owned()) {
              error!(
                "{}: Failed to read some certificates and keys {}",
                backend.server_name.as_str(),
                e
              )
            }

            if backend.client_ca_cert_path.is_none() {
              // aggregated server config for no client auth server for http3
              if let Err(e) = resolver_global.add(backend.server_name.as_str(), certified_key) {
                error!(
                  "{}: Failed to read some certificates and keys {}",
                  backend.server_name.as_str(),
                  e
                )
              }
            } else {
              // add client certificate if specified
              match backend.read_client_ca_certs() {
                Ok((owned_trust_anchors, _subject_key_ids)) => {
                  client_ca_roots_local.add_server_trust_anchors(owned_trust_anchors.into_iter());
                }
                Err(e) => {
                  warn!(
                    "Failed to add client CA certificate for {}: {}",
                    backend.server_name.as_str(),
                    e
                  );
                }
              }
            }

            let mut server_config_local = if client_ca_roots_local.is_empty() {
              // with no client auth, enable http1.1 -- 3
              let mut sc = ServerConfig::builder()
                .with_safe_defaults()
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(resolver_local));
              #[cfg(feature = "http3")]
              {
                sc.alpn_protocols = vec![b"h3".to_vec(), b"hq-29".to_vec()]; // TODO: remove hq-29 later?
              }
              sc
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
          Err(e) => {
            warn!("Failed to add certificate for {}: {}", backend.server_name.as_str(), e);
          }
        }
      }
    }
    // debug!("Load certificate chain for {} server_name's", cnt);

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
      server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }

    Ok(ServerCrypto {
      inner_global_no_client_auth: Arc::new(server_crypto_global),
      inner_local_map: Arc::new(server_crypto_local_map),
    })
  }
}
