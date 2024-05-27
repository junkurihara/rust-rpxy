use crate::SingleServerCertsKeys;
use rustc_hash::FxHashMap as HashMap;
use rustls::ServerConfig;
use std::sync::Arc;

/* ------------------------------------------------ */
/// ServerName in bytes type (TODO: this may be changed to define `common` layer defining types of names. or should be independent?)
pub type ServerNameBytes = Vec<u8>;
/// ServerName (SNI) to ServerConfig map type
pub type ServerNameCryptoMap = HashMap<ServerNameBytes, Arc<ServerConfig>>;
/// ServerName (SNI) to ServerConfig map
pub struct ServerCrypto {
  // For Quic/HTTP3, only servers with no client authentication
  pub inner_global_no_client_auth: Arc<ServerConfig>,
  //   // For TLS over TCP/HTTP2 and 1.1, map of SNI to server_crypto for all given servers
  pub inner_local_map: Arc<ServerNameCryptoMap>,
}

/* ------------------------------------------------ */
/// Reloader target for the certificate reloader service
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct ServerCryptoBase {
  /// Map of server name to certs and keys
  pub(super) inner: HashMap<ServerNameBytes, SingleServerCertsKeys>,
}
