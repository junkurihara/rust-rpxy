use crate::{certs::SingleServerCertsKeys, error::*, log::*};
use ahash::HashMap;
use rustls::{
  RootCertStore, ServerConfig,
  crypto::CryptoProvider,
  server::{NoServerSessionStorage, ProducesTickets, ResolvesServerCertUsingSni, WebPkiClientVerifier},
};
use std::sync::{Arc, OnceLock};

/* ------------------------------------------------ */
/// ServerName in bytes type.
///
/// TODO: Move server-name byte/string types into a shared module if another crate needs the abstraction.
pub type ServerNameBytes = Vec<u8>;
/// Convert ServerName in bytes to string
fn server_name_bytes_to_string(server_name_bytes: &ServerNameBytes) -> Result<String, RpxyCertError> {
  let server_name = String::from_utf8(server_name_bytes.to_ascii_lowercase())?;
  Ok(server_name)
}

/* ------------------------------------------------ */
/// Process-wide stateless session ticketer shared by every non-mTLS server config and across
/// certificate hot-reloads. A single instance is required: `Ticketer::new()` generates fresh
/// keys on every call, so per-build or per-reload instances would make outstanding tickets
/// mutually undecryptable and resumption would not survive reloads.
static SHARED_TICKETER: OnceLock<Arc<dyn ProducesTickets>> = OnceLock::new();

/// Get (or lazily create) the process-wide stateless ticketer.
fn shared_ticketer() -> Result<Arc<dyn ProducesTickets>, RpxyCertError> {
  if let Some(ticketer) = SHARED_TICKETER.get() {
    return Ok(ticketer.clone());
  }
  // `OnceLock::get_or_try_init` is still unstable (`once_cell_try`, E0658 as of Rust 1.96), so
  // build first and let `get_or_init` pick a single winner; a racing builder only creates a
  // transient extra ticketer that is dropped unused.
  let ticketer = rustls::crypto::aws_lc_rs::Ticketer::new()?;
  Ok(SHARED_TICKETER.get_or_init(|| ticketer).clone())
}

/* ------------------------------------------------ */
/// Per-SNI server config together with whether it enforces mutual TLS (client certificate auth)
#[derive(Clone)]
pub struct ServerCryptoForSni {
  pub server_config: Arc<ServerConfig>,
  pub is_mutual_tls: bool,
}

/// ServerName (SNI) to ServerConfig map type
pub type ServerNameCryptoMap = HashMap<ServerNameBytes, ServerCryptoForSni>;

/// ServerName (SNI) to ServerConfig map
pub struct ServerCrypto {
  // For Quic/HTTP3, only servers with no client authentication, aggregated server config
  pub aggregated_config_no_client_auth: Arc<ServerConfig>,
  // For TLS over TCP/HTTP2 and 1.1, map of SNI to server_crypto for all given servers
  pub individual_config_map: Arc<ServerNameCryptoMap>,
}

/* ------------------------------------------------ */
/// Reloader target for the certificate reloader service
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct ServerCryptoBase {
  /// Map of server name to certs and keys
  pub(super) inner: HashMap<ServerNameBytes, SingleServerCertsKeys>,
}

impl TryInto<Arc<ServerCrypto>> for &ServerCryptoBase {
  type Error = RpxyCertError;

  fn try_into(self) -> Result<Arc<ServerCrypto>, Self::Error> {
    let aggregated = self.build_aggregated_server_crypto()?;
    let individual = self.build_individual_server_crypto_map()?;
    Ok(Arc::new(ServerCrypto {
      aggregated_config_no_client_auth: Arc::new(aggregated),
      individual_config_map: Arc::new(individual),
    }))
  }
}

impl ServerCryptoBase {
  /// Build individual server crypto inner object
  fn build_individual_server_crypto_map(&self) -> Result<ServerNameCryptoMap, RpxyCertError> {
    let mut server_crypto_map: ServerNameCryptoMap = HashMap::default();

    // AWS LC provider by default
    let provider = CryptoProvider::get_default().ok_or(RpxyCertError::NoDefaultCryptoProvider)?;

    for (server_name_bytes, certs_keys) in self.inner.iter() {
      let server_name = server_name_bytes_to_string(server_name_bytes)?;

      // Parse server certificates and private keys
      let Ok(certified_key) = certs_keys.rustls_certified_key() else {
        warn!("Failed to add certificate for {server_name}");
        continue;
      };

      let mut resolver_local = ResolvesServerCertUsingSni::new();
      if let Err(e) = resolver_local.add(&server_name, certified_key) {
        error!("{server_name}: Failed to read some certificates and keys {e}");
      };

      // With no client authentication case
      if !certs_keys.is_mutual_tls() {
        let mut server_crypto_local = ServerConfig::builder_with_provider(provider.clone())
          .with_safe_default_protocol_versions()?
          .with_no_client_auth()
          .with_cert_resolver(Arc::new(resolver_local));

        #[cfg(feature = "http3")]
        {
          server_crypto_local.alpn_protocols = vec![b"h3".to_vec(), b"h2".to_vec(), b"http/1.1".to_vec()];
        }
        #[cfg(not(feature = "http3"))]
        {
          server_crypto_local.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        }
        // Resumption is via stateless session tickets only: the ticket keys are process-wide and
        // shared across reloads (see `shared_ticketer`), and the default mutex-guarded in-memory
        // session cache is disabled so the server keeps no per-session state (TLS 1.2 clients
        // resume via tickets as well).
        server_crypto_local.ticketer = shared_ticketer()?;
        server_crypto_local.session_storage = Arc::new(NoServerSessionStorage {});
        server_crypto_map.insert(
          server_name_bytes.clone(),
          ServerCryptoForSni {
            server_config: Arc::new(server_crypto_local),
            is_mutual_tls: false,
          },
        );
        continue;
      }

      // With client authentication case, enable only http2 and http1.1
      let mut client_ca_roots_local = RootCertStore::empty();
      let Ok(trust_anchors) = certs_keys.rustls_client_certs_trust_anchors() else {
        warn!("Failed to add client CA certificate for {server_name}");
        continue;
      };
      let trust_anchors_without_skid = trust_anchors.values().map(|ta| ta.to_owned());
      client_ca_roots_local.extend(trust_anchors_without_skid);

      let Ok(client_cert_verifier) =
        WebPkiClientVerifier::builder_with_provider(Arc::new(client_ca_roots_local), provider.clone()).build()
      else {
        warn!("Failed to build client CA certificate verifier for {server_name}");
        continue;
      };
      let mut server_crypto_local = ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(client_cert_verifier)
        .with_cert_resolver(Arc::new(resolver_local));
      server_crypto_local.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
      // Mutual-TLS apps must never resume a TLS session: a resumed handshake restores the stored
      // client identity without re-running the client cert verifier, which would delay revocation
      // by the session lifetime and escape the handshake-failure audit. No ticketer is configured,
      // and the default stateful session cache is disabled too so this holds for TLS 1.2 and 1.3.
      server_crypto_local.session_storage = Arc::new(NoServerSessionStorage {});
      server_crypto_map.insert(
        server_name_bytes.clone(),
        ServerCryptoForSni {
          server_config: Arc::new(server_crypto_local),
          is_mutual_tls: true,
        },
      );
    }

    Ok(server_crypto_map)
  }

  /* ------------------------------------------------ */
  /// Build aggregated server crypto inner object for no client auth server especially for http3
  fn build_aggregated_server_crypto(&self) -> Result<ServerConfig, RpxyCertError> {
    let mut resolver_global = ResolvesServerCertUsingSni::new();

    // AWS LC provider by default
    let provider = CryptoProvider::get_default().ok_or(RpxyCertError::NoDefaultCryptoProvider)?;

    for (server_name_bytes, certs_keys) in self.inner.iter() {
      let server_name = server_name_bytes_to_string(server_name_bytes)?;

      // Parse server certificates and private keys
      let Ok(certified_key) = certs_keys.rustls_certified_key() else {
        warn!("Failed to add certificate for {server_name}");
        continue;
      };
      // Add server certificates and private keys to resolver only if client CA certs are not present
      if !certs_keys.is_mutual_tls() {
        // aggregated server config for no client auth server for http3
        if let Err(e) = resolver_global.add(&server_name, certified_key) {
          error!("{server_name}: Failed to read some certificates and keys {e}");
        };
      }
    }

    let mut server_crypto_global = ServerConfig::builder_with_provider(provider.clone())
      .with_safe_default_protocol_versions()?
      .with_no_client_auth()
      .with_cert_resolver(Arc::new(resolver_global));

    #[cfg(feature = "http3")]
    {
      server_crypto_global.alpn_protocols = vec![b"h3".to_vec(), b"h2".to_vec(), b"http/1.1".to_vec()];
    }
    #[cfg(not(feature = "http3"))]
    {
      server_crypto_global.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    }

    // Same stateless-tickets-only resumption policy as the per-SNI non-mTLS configs above; this
    // aggregated config backs the QUIC/HTTP3 listener.
    server_crypto_global.ticketer = shared_ticketer()?;
    server_crypto_global.session_storage = Arc::new(NoServerSessionStorage {});

    Ok(server_crypto_global)
  }
}

/* ------------------------------------------------ */
#[cfg(test)]
mod tests {
  use super::*;
  use crate::{CryptoFileSourceBuilder, CryptoSource};
  use std::convert::TryInto;

  async fn read_file_source() -> SingleServerCertsKeys {
    let tls_cert_path = "../example-certs/server.crt";
    let tls_cert_key_path = "../example-certs/server.key";
    let client_ca_cert_path = Some("../example-certs/client.ca.crt");
    let crypto_file_source = CryptoFileSourceBuilder::default()
      .tls_cert_key_path(tls_cert_key_path)
      .tls_cert_path(tls_cert_path)
      .client_ca_cert_path(client_ca_cert_path)
      .build();
    crypto_file_source.unwrap().read().await.unwrap()
  }

  async fn read_file_source_without_client_ca() -> SingleServerCertsKeys {
    let crypto_file_source = CryptoFileSourceBuilder::default()
      .tls_cert_key_path("../example-certs/server.key")
      .tls_cert_path("../example-certs/server.crt")
      .build();
    crypto_file_source.unwrap().read().await.unwrap()
  }

  #[tokio::test]
  async fn test_server_crypto_base_try_into() {
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());

    let mut server_crypto_base = ServerCryptoBase::default();

    let single_certs_keys = read_file_source().await;
    server_crypto_base.inner.insert(b"localhost".to_vec(), single_certs_keys);
    let single_certs_keys_no_mtls = read_file_source_without_client_ca().await;
    server_crypto_base
      .inner
      .insert(b"example.com".to_vec(), single_certs_keys_no_mtls);
    let server_crypto: Arc<ServerCrypto> = (&server_crypto_base).try_into().unwrap();
    assert_eq!(server_crypto.individual_config_map.len(), 2);

    // The per-SNI mTLS flag drives the `mtls` field of handshake-failure audit logs.
    assert!(
      server_crypto
        .individual_config_map
        .get(b"localhost".as_slice())
        .unwrap()
        .is_mutual_tls,
      "localhost has a client CA configured, so it must be marked as mutual TLS"
    );
    assert!(
      !server_crypto
        .individual_config_map
        .get(b"example.com".as_slice())
        .unwrap()
        .is_mutual_tls,
      "example.com has no client CA, so it must not be marked as mutual TLS"
    );

    #[cfg(feature = "http3")]
    {
      assert_eq!(
        server_crypto.aggregated_config_no_client_auth.alpn_protocols,
        vec![b"h3".to_vec(), b"h2".to_vec(), b"http/1.1".to_vec()]
      );
    }
    #[cfg(not(feature = "http3"))]
    {
      assert_eq!(
        server_crypto.aggregated_config_no_client_auth.alpn_protocols,
        vec![b"h2".to_vec(), b"http/1.1".to_vec()]
      );
    }
  }

  #[tokio::test]
  async fn test_non_mtls_configs_use_stateless_tickets_without_session_cache() {
    #[cfg(not(feature = "post-quantum"))]
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());
    #[cfg(feature = "post-quantum")]
    let _ = CryptoProvider::install_default(rustls_post_quantum::provider());

    let mut server_crypto_base = ServerCryptoBase::default();
    server_crypto_base
      .inner
      .insert(b"example.com".to_vec(), read_file_source_without_client_ca().await);
    let server_crypto: Arc<ServerCrypto> = (&server_crypto_base).try_into().unwrap();

    let non_mtls = &server_crypto
      .individual_config_map
      .get(b"example.com".as_slice())
      .unwrap()
      .server_config;
    assert!(
      non_mtls.ticketer.enabled(),
      "non-mTLS config must issue stateless session tickets"
    );
    assert!(
      !non_mtls.session_storage.can_cache(),
      "non-mTLS config must keep no server-side session state (stateless tickets only)"
    );

    let aggregated = &server_crypto.aggregated_config_no_client_auth;
    assert!(
      aggregated.ticketer.enabled(),
      "aggregated (QUIC/HTTP3) config must issue stateless session tickets"
    );
    assert!(
      !aggregated.session_storage.can_cache(),
      "aggregated (QUIC/HTTP3) config must keep no server-side session state"
    );
  }

  #[tokio::test]
  async fn test_mtls_config_disables_session_resumption_entirely() {
    #[cfg(not(feature = "post-quantum"))]
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());
    #[cfg(feature = "post-quantum")]
    let _ = CryptoProvider::install_default(rustls_post_quantum::provider());

    let mut server_crypto_base = ServerCryptoBase::default();
    server_crypto_base
      .inner
      .insert(b"localhost".to_vec(), read_file_source().await);
    let server_crypto: Arc<ServerCrypto> = (&server_crypto_base).try_into().unwrap();

    let mtls = &server_crypto
      .individual_config_map
      .get(b"localhost".as_slice())
      .unwrap()
      .server_config;
    // `ticketer.enabled() == false` alone would also pass with the rustls default config, which
    // still resumes via the stateful fallback cache; `can_cache() == false` is what guarantees
    // that the client certificate is verified on every mTLS connection.
    assert!(!mtls.ticketer.enabled(), "mTLS config must not issue session tickets");
    assert!(
      !mtls.session_storage.can_cache(),
      "mTLS config must not cache sessions: client certs are verified on every connection"
    );
  }

  #[tokio::test]
  async fn test_ticketer_is_shared_across_rebuilds() {
    #[cfg(not(feature = "post-quantum"))]
    let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());
    #[cfg(feature = "post-quantum")]
    let _ = CryptoProvider::install_default(rustls_post_quantum::provider());

    let mut server_crypto_base = ServerCryptoBase::default();
    server_crypto_base
      .inner
      .insert(b"example.com".to_vec(), read_file_source_without_client_ca().await);

    // Building twice simulates a certificate hot-reload, which rebuilds every ServerConfig.
    let first: Arc<ServerCrypto> = (&server_crypto_base).try_into().unwrap();
    let second: Arc<ServerCrypto> = (&server_crypto_base).try_into().unwrap();

    let first_ticketer = &first
      .individual_config_map
      .get(b"example.com".as_slice())
      .unwrap()
      .server_config
      .ticketer;
    let second_ticketer = &second
      .individual_config_map
      .get(b"example.com".as_slice())
      .unwrap()
      .server_config
      .ticketer;
    assert!(
      Arc::ptr_eq(first_ticketer, second_ticketer),
      "ticketer must be one process-wide instance so outstanding tickets stay decryptable across cert hot-reloads"
    );
    assert!(
      Arc::ptr_eq(&first.aggregated_config_no_client_auth.ticketer, first_ticketer),
      "aggregated (QUIC/HTTP3) config must share the same ticketer instance"
    );
  }
}
