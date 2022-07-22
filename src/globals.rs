use crate::backend::Backends;
use std::net::SocketAddr;
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Arc,
};
use tokio::time::Duration;

pub struct Globals {
  pub listen_sockets: Vec<SocketAddr>,
  pub http_port: Option<u16>,
  pub https_port: Option<u16>,

  pub proxy_timeout: Duration,
  pub upstream_timeout: Duration,

  pub max_clients: usize,
  pub request_count: RequestCount,
  pub max_concurrent_streams: u32,
  pub keepalive: bool,

  pub runtime_handle: tokio::runtime::Handle,
  pub backends: Backends,

  // experimentals
  pub sni_consistency: bool,
  pub http3: bool,
  pub h3_alt_svc_max_age: u32,
  pub h3_request_max_body_size: usize,
  pub h3_max_concurrent_bidistream: quinn::VarInt,
  pub h3_max_concurrent_unistream: quinn::VarInt,
  pub h3_max_concurrent_connections: u32,
}

#[derive(Debug, Clone, Default)]
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
