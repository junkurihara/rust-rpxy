use std::sync::atomic::{AtomicBool, Ordering};

/// Shared health state for a single upstream, accessed by both the health checker task
/// and the request handler (via Arc).
#[derive(Debug)]
pub struct UpstreamHealth {
  healthy: AtomicBool,
}

impl UpstreamHealth {
  /// Create a new health state, initialized as healthy (optimistic boot).
  pub fn new() -> Self {
    Self {
      healthy: AtomicBool::new(true),
    }
  }

  /// Returns current health status.
  pub fn is_healthy(&self) -> bool {
    self.healthy.load(Ordering::Relaxed)
  }

  /// Set health status.
  pub fn set(&self, healthy: bool) {
    self.healthy.store(healthy, Ordering::Relaxed);
  }
}

impl Default for UpstreamHealth {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn initial_state_is_healthy() {
    let h = UpstreamHealth::new();
    assert!(h.is_healthy());
  }

  #[test]
  fn set_unhealthy_and_recover() {
    let h = UpstreamHealth::new();
    h.set(false);
    assert!(!h.is_healthy());
    h.set(true);
    assert!(h.is_healthy());
  }
}
