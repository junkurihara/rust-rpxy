use rustc_hash::FxHashMap as HashMap;
use rustls::ServerConfig;
use std::sync::Arc;

/// ServerName in bytes type
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
