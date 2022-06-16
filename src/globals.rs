use std::net::SocketAddr;
#[cfg(feature = "tls")]
use std::path::PathBuf;
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Arc,
};
use tokio::time::Duration;

#[derive(Debug, Clone)]
pub struct Globals {
  pub listen_addresses: Vec<SocketAddr>,

  pub timeout: Duration,
  pub max_clients: usize,
  pub clients_count: ClientsCount,
  pub max_concurrent_streams: u32,
  pub keepalive: bool,

  pub runtime_handle: tokio::runtime::Handle,

  #[cfg(feature = "tls")]
  pub tls_cert_path: Option<PathBuf>,

  #[cfg(feature = "tls")]
  pub tls_cert_key_path: Option<PathBuf>,
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
