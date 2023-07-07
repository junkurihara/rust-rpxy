use crate::backend::Backends;
use std::net::SocketAddr;
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Arc,
};
use tokio::time::Duration;

/// Global object containing proxy configurations and shared object like counters.
/// But note that in Globals, we do not have Mutex and RwLock. It is indeed, the context shared among async tasks.
pub struct Globals {
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

  // Shared context
  // Backend application objects to which http request handler forward incoming requests
  pub backends: Backends,
  // Counter for serving requests
  pub request_count: RequestCount,
  // Async task runtime handler
  pub runtime_handle: tokio::runtime::Handle,
}

// // TODO: Implement default for default values
// #[derive(Debug, Clone)]
// pub struct ProxyConfig {}

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
