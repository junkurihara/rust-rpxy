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
  pub clients_count: ClientsCount,
  pub max_concurrent_streams: u32,
  pub keepalive: bool,
  pub http3: bool,
  pub sni_consistency: bool,

  pub runtime_handle: tokio::runtime::Handle,

  pub backends: Backends,
}

#[derive(Debug, Clone, Default)]
pub struct ClientsCount(Arc<AtomicUsize>);

impl ClientsCount {
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
