use crate::{constants::*, count::RequestCount};
use hot_reload::ReloaderReceiver;
use rpxy_certs::ServerCryptoBase;
use std::{net::SocketAddr, time::Duration};
use tokio_util::sync::CancellationToken;

/// Global object containing proxy configurations and shared object like counters.
/// But note that in Globals, we do not have Mutex and RwLock. It is indeed, the context shared among async tasks.
pub struct Globals {
  /// Configuration parameters for proxy transport and request handlers
  pub proxy_config: ProxyConfig,
  /// Shared context - Counter for serving requests
  pub request_count: RequestCount,
  /// Shared context - Async task runtime handler
  pub runtime_handle: tokio::runtime::Handle,
  /// Shared context - Notify object to stop async tasks
  pub cancel_token: CancellationToken,
  /// Shared context - Certificate reloader service receiver // TODO: newer one
  pub cert_reloader_rx: Option<ReloaderReceiver<ServerCryptoBase>>,

  #[cfg(feature = "acme")]
  /// ServerConfig used for only ACME challenge for ACME domains
  pub server_configs_acme_challenge: std::sync::Arc<rustc_hash::FxHashMap<String, std::sync::Arc<rustls::ServerConfig>>>,
}

/// Configuration parameters for proxy transport and request handlers
#[derive(PartialEq, Eq, Clone)]
pub struct ProxyConfig {
  /// listen socket addresses
  pub listen_sockets: Vec<SocketAddr>,
  /// http port
  pub http_port: Option<u16>,
  /// https port
  pub https_port: Option<u16>,
  /// tcp listen backlog
  pub tcp_listen_backlog: u32,

  /// Idle timeout as an HTTP server, used as the keep alive interval and timeout for reading request header
  pub proxy_idle_timeout: Duration,
  /// Idle timeout as an HTTP client, used as the keep alive interval for upstream connections
  pub upstream_idle_timeout: Duration,

  pub max_clients: usize,          // when serving requests
  pub max_concurrent_streams: u32, // when instantiate server
  pub keepalive: bool,             // when instantiate server

  // experimentals
  /// SNI consistency check
  pub sni_consistency: bool, // Handler
  /// Connection handling timeout
  /// timeout to handle a connection, total time of receive request, serve, and send response. this might limits the max length of response.
  pub connection_handling_timeout: Option<Duration>,

  #[cfg(feature = "cache")]
  pub cache_enabled: bool,
  #[cfg(feature = "cache")]
  pub cache_dir: Option<std::path::PathBuf>,
  #[cfg(feature = "cache")]
  pub cache_max_entry: usize,
  #[cfg(feature = "cache")]
  pub cache_max_each_size: usize,
  #[cfg(feature = "cache")]
  pub cache_max_each_size_on_memory: usize,

  // All need to make packet acceptor
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub http3: bool,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_alt_svc_max_age: u32,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_request_max_body_size: usize,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_max_concurrent_bidistream: u32,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_max_concurrent_unistream: u32,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_max_concurrent_connections: u32,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_max_idle_timeout: Option<Duration>,
}

impl Default for ProxyConfig {
  fn default() -> Self {
    Self {
      listen_sockets: Vec::new(),
      http_port: None,
      https_port: None,
      tcp_listen_backlog: TCP_LISTEN_BACKLOG,

      // TODO: Reconsider each timeout values
      proxy_idle_timeout: Duration::from_secs(PROXY_IDLE_TIMEOUT_SEC),
      upstream_idle_timeout: Duration::from_secs(UPSTREAM_IDLE_TIMEOUT_SEC),

      max_clients: MAX_CLIENTS,
      max_concurrent_streams: MAX_CONCURRENT_STREAMS,
      keepalive: true,

      sni_consistency: true,
      connection_handling_timeout: None,

      #[cfg(feature = "cache")]
      cache_enabled: false,
      #[cfg(feature = "cache")]
      cache_dir: None,
      #[cfg(feature = "cache")]
      cache_max_entry: MAX_CACHE_ENTRY,
      #[cfg(feature = "cache")]
      cache_max_each_size: MAX_CACHE_EACH_SIZE,
      #[cfg(feature = "cache")]
      cache_max_each_size_on_memory: MAX_CACHE_EACH_SIZE_ON_MEMORY,

      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      http3: false,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_alt_svc_max_age: H3::ALT_SVC_MAX_AGE,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_request_max_body_size: H3::REQUEST_MAX_BODY_SIZE,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_max_concurrent_connections: H3::MAX_CONCURRENT_CONNECTIONS,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_max_concurrent_bidistream: H3::MAX_CONCURRENT_BIDISTREAM,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_max_concurrent_unistream: H3::MAX_CONCURRENT_UNISTREAM,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_max_idle_timeout: Some(Duration::from_secs(H3::MAX_IDLE_TIMEOUT)),
    }
  }
}

/// Configuration parameters for backend applications
#[derive(PartialEq, Eq, Clone)]
pub struct AppConfigList {
  pub inner: Vec<AppConfig>,
  pub default_app: Option<String>,
}

/// Configuration parameters for single backend application
#[derive(PartialEq, Eq, Clone)]
pub struct AppConfig {
  pub app_name: String,
  pub server_name: String,
  pub reverse_proxy: Vec<ReverseProxyConfig>,
  pub tls: Option<TlsConfig>,
}

/// Configuration parameters for single reverse proxy corresponding to the path
#[derive(PartialEq, Eq, Clone)]
pub struct ReverseProxyConfig {
  pub path: Option<String>,
  pub replace_path: Option<String>,
  pub upstream: Vec<UpstreamUri>,
  pub upstream_options: Option<Vec<String>>,
  pub load_balance: Option<String>,
}

/// Configuration parameters for single upstream destination from a reverse proxy
#[derive(PartialEq, Eq, Clone)]
pub struct UpstreamUri {
  pub inner: http::Uri,
}

/// Configuration parameters on TLS for a single backend application
#[derive(PartialEq, Eq, Clone)]
pub struct TlsConfig {
  pub mutual_tls: bool,
  pub https_redirection: bool,
  #[cfg(feature = "acme")]
  pub acme: bool,
}
