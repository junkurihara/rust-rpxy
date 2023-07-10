use crate::{backend::Backends, constants::*};
use std::net::SocketAddr;
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Arc,
};
use tokio::time::Duration;

/// Global object containing proxy configurations and shared object like counters.
/// But note that in Globals, we do not have Mutex and RwLock. It is indeed, the context shared among async tasks.
pub struct Globals {
  /// Configuration parameters for proxy transport and request handlers
  pub proxy_config: ProxyConfig, // TODO: proxy configはarcに包んでこいつだけ使いまわせばいいように変えていく。backendsも？

  /// Shared context - Backend application objects to which http request handler forward incoming requests
  pub backends: Backends,

  /// Shared context - Counter for serving requests
  pub request_count: RequestCount,

  /// Shared context - Async task runtime handler
  pub runtime_handle: tokio::runtime::Handle,
}

/// Configuration parameters for proxy transport and request handlers
pub struct ProxyConfig {
  pub listen_sockets: Vec<SocketAddr>, // when instantiate server
  pub http_port: Option<u16>,          // when instantiate server
  pub https_port: Option<u16>,         // when instantiate server

  pub proxy_timeout: Duration,    // when serving requests at Proxy
  pub upstream_timeout: Duration, // when serving requests at Handler

  pub max_clients: usize,          // when serving requests
  pub max_concurrent_streams: u32, // when instantiate server
  pub keepalive: bool,             // when instantiate server

  // experimentals
  pub sni_consistency: bool, // Handler
  // All need to make packet acceptor
  #[cfg(feature = "http3")]
  pub http3: bool,
  #[cfg(feature = "http3")]
  pub h3_alt_svc_max_age: u32,
  #[cfg(feature = "http3")]
  pub h3_request_max_body_size: usize,
  #[cfg(feature = "http3")]
  pub h3_max_concurrent_bidistream: quinn::VarInt,
  #[cfg(feature = "http3")]
  pub h3_max_concurrent_unistream: quinn::VarInt,
  #[cfg(feature = "http3")]
  pub h3_max_concurrent_connections: u32,
  #[cfg(feature = "http3")]
  pub h3_max_idle_timeout: Option<quinn::IdleTimeout>,
}

impl Default for ProxyConfig {
  fn default() -> Self {
    Self {
      listen_sockets: Vec::new(),
      http_port: None,
      https_port: None,

      // TODO: Reconsider each timeout values
      proxy_timeout: Duration::from_secs(PROXY_TIMEOUT_SEC),
      upstream_timeout: Duration::from_secs(UPSTREAM_TIMEOUT_SEC),

      max_clients: MAX_CLIENTS,
      max_concurrent_streams: MAX_CONCURRENT_STREAMS,
      keepalive: true,

      sni_consistency: true,

      #[cfg(feature = "http3")]
      http3: false,
      #[cfg(feature = "http3")]
      h3_alt_svc_max_age: H3::ALT_SVC_MAX_AGE,
      #[cfg(feature = "http3")]
      h3_request_max_body_size: H3::REQUEST_MAX_BODY_SIZE,
      #[cfg(feature = "http3")]
      h3_max_concurrent_connections: H3::MAX_CONCURRENT_CONNECTIONS,
      #[cfg(feature = "http3")]
      h3_max_concurrent_bidistream: H3::MAX_CONCURRENT_BIDISTREAM.into(),
      #[cfg(feature = "http3")]
      h3_max_concurrent_unistream: H3::MAX_CONCURRENT_UNISTREAM.into(),
      #[cfg(feature = "http3")]
      h3_max_idle_timeout: Some(quinn::IdleTimeout::try_from(Duration::from_secs(H3::MAX_IDLE_TIMEOUT)).unwrap()),
    }
  }
}

#[derive(Debug, Clone, Default)]
/// Counter for serving requests
pub struct RequestCount(Arc<AtomicUsize>);

impl RequestCount {
  pub fn current(&self) -> usize {
    self.0.load(Ordering::Relaxed)
  }

  pub fn increment(&self) -> usize {
    self.0.fetch_add(1, Ordering::Relaxed)
  }

  pub fn decrement(&self) -> usize {
    let mut count;
    while {
      count = self.0.load(Ordering::Relaxed);
      count > 0
        && self
          .0
          .compare_exchange(count, count - 1, Ordering::Relaxed, Ordering::Relaxed)
          != Ok(count)
    } {}
    count
  }
}
