mod upstream;
mod upstream_opts;

use crate::{
  log::*,
  utils::{BytesName, PathNameBytesExp, ServerNameBytesExp},
};
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use rustls::OwnedTrustAnchor;
use std::{
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
pub use upstream::{ReverseProxy, Upstream, UpstreamGroup};
pub use upstream_opts::UpstreamOption;
use x509_parser::prelude::*;

/// Struct serving information to route incoming connections, like server name to be handled and tls certs/keys settings.
pub struct Backend {
  pub app_name: String,
  pub server_name: String,
  pub reverse_proxy: ReverseProxy,

  // tls settings
  pub tls_cert_path: Option<PathBuf>,
  pub tls_cert_key_path: Option<PathBuf>,
  pub https_redirection: Option<bool>,
  pub client_ca_cert_path: Option<PathBuf>,
}

impl Backend {
  pub fn read_certs_and_key(&self) -> io::Result<CertifiedKey> {
    debug!("Read TLS server certificates and private key");
    let (certs_path, certs_keys_path) =
      if let (Some(c), Some(k)) = (self.tls_cert_path.as_ref(), self.tls_cert_key_path.as_ref()) {
        (c, k)
      } else {
        return Err(io::Error::new(io::ErrorKind::Other, "Invalid certs and keys paths"));
      };
    let certs: Vec<_> = {
      let certs_path_str = certs_path.display().to_string();
      let mut reader = BufReader::new(File::open(certs_path).map_err(|e| {
        io::Error::new(
          e.kind(),
          format!("Unable to load the certificates [{}]: {}", certs_path_str, e),
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
              format!("Unable to load the certificate keys [{}]: {}", certs_keys_path_str, e),
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
      if let Some(c) = self.client_ca_cert_path.as_ref() {
        c
      } else {
        return Err(io::Error::new(io::ErrorKind::Other, "Invalid certs and keys paths"));
      }
    };
    let certs: Vec<_> = {
      let certs_path_str = client_ca_cert_path.display().to_string();
      let mut reader = BufReader::new(File::open(client_ca_cert_path).map_err(|e| {
        io::Error::new(
          e.kind(),
          format!("Unable to load the client certificates [{}]: {}", certs_path_str, e),
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
        let trust_anchor = tokio_rustls::webpki::TrustAnchor::try_from_cert_der(&v.0).unwrap();
        rustls::OwnedTrustAnchor::from_subject_spki_name_constraints(
          trust_anchor.subject,
          trust_anchor.spki,
          trust_anchor.name_constraints,
        )
      })
      .collect();

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

pub type SniKeyIdsMap = HashMap<ServerNameBytesExp, HashSet<Vec<u8>>>;
pub struct ServerCrypto {
  pub inner: Arc<ServerConfig>,
  pub server_name_client_ca_keyids_map: Arc<SniKeyIdsMap>,
}

impl Backends {
  pub async fn generate_server_crypto_with_cert_resolver(&self) -> Result<ServerCrypto, anyhow::Error> {
    let mut resolver = ResolvesServerCertUsingSni::new();
    let mut client_ca_roots = rustls::RootCertStore::empty();
    let mut client_ca_key_ids: SniKeyIdsMap = HashMap::default();

    // let mut cnt = 0;
    for (server_name_bytes_exp, backend) in self.apps.iter() {
      if backend.tls_cert_key_path.is_some() && backend.tls_cert_path.is_some() {
        match backend.read_certs_and_key() {
          Ok(certified_key) => {
            if let Err(e) = resolver.add(backend.server_name.as_str(), certified_key) {
              error!(
                "{}: Failed to read some certificates and keys {}",
                backend.server_name.as_str(),
                e
              )
            } else {
              // debug!("Add certificate for server_name: {}", backend.server_name.as_str());
              // cnt += 1;
            }
          }
          Err(e) => {
            warn!("Failed to add certificate for {}: {}", backend.server_name.as_str(), e);
          }
        }
        // add client certificate if specified
        if backend.client_ca_cert_path.is_some() {
          match backend.read_client_ca_certs() {
            Ok((owned_trust_anchors, subject_key_ids)) => {
              // TODO: ここでSubject Key ID (CA Key ID)を記録しておく。認証後にpeer certificateのauthority key idとの一貫性をチェック。
              // v3 x509前提で特定のkey id extが入ってなければ使えない前提
              client_ca_roots.add_server_trust_anchors(owned_trust_anchors.into_iter());
              client_ca_key_ids.insert(server_name_bytes_exp.to_owned(), subject_key_ids);
            }
            Err(e) => {
              warn!(
                "Failed to add client ca certificate for {}: {}",
                backend.server_name.as_str(),
                e
              );
            }
          }
        }
      }
    }
    // debug!("Load certificate chain for {} server_name's", cnt);

    //////////////
    let mut server_config = if client_ca_key_ids.is_empty() {
      ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(resolver))
    } else {
      // TODO: Client Certs
      // No ClientCert or WithClientCert
      // let client_certs_verifier = rustls::server::AllowAnyAuthenticatedClient::new(client_ca_roots);
      let client_certs_verifier = rustls::server::AllowAnyAnonymousOrAuthenticatedClient::new(client_ca_roots);
      ServerConfig::builder()
        .with_safe_defaults()
        .with_client_cert_verifier(client_certs_verifier)
        .with_cert_resolver(Arc::new(resolver))
    };

    //////////////////////////////

    #[cfg(feature = "http3")]
    {
      server_config.alpn_protocols = vec![
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
      inner: Arc::new(server_config),
      server_name_client_ca_keyids_map: Arc::new(client_ca_key_ids),
    })
  }
}
