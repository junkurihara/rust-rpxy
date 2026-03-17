use super::Upstream;
#[allow(unused)]
#[cfg(feature = "sticky-cookie")]
pub use super::{
  load_balance_sticky::{LoadBalanceSticky, LoadBalanceStickyBuilder},
  sticky_cookie::StickyCookie,
};
use derive_builder::Builder;
use rand::RngExt;
use std::sync::{
  Arc,
  atomic::{AtomicUsize, Ordering},
};

/// Constants to specify a load balance option
pub mod load_balance_options {
  pub const FIX_TO_FIRST: &str = "none";
  pub const ROUND_ROBIN: &str = "round_robin";
  pub const RANDOM: &str = "random";
  #[cfg(feature = "sticky-cookie")]
  pub const STICKY_ROUND_ROBIN: &str = "sticky";
  #[cfg(feature = "health-check")]
  pub const PRIMARY_BACKUP: &str = "primary_backup";
}

#[derive(Debug, Clone)]
/// Pointer to upstream serving the incoming request.
/// If 'sticky cookie'-based LB is enabled and cookie must be updated/created, the new cookie is also given.
pub struct PointerToUpstream {
  pub ptr: usize,
  pub context: Option<LoadBalanceContext>,
}
/// Trait for LB
pub(super) trait LoadBalanceWithPointer {
  /// Get the index of the upstream serving the incoming request, and optionally a context to update LB state (e.g. sticky cookie value).
  fn get_ptr(&self, req_info: Option<&LoadBalanceContext>, upstreams: &[Upstream]) -> PointerToUpstream;
}

/// Collect indices of healthy upstreams. Returns all indices if none are healthy (best-effort).
pub(super) fn healthy_indices(upstreams: &[Upstream]) -> Vec<usize> {
  let healthy: Vec<usize> = upstreams
    .iter()
    .enumerate()
    .filter(|(_, u)| u.is_healthy())
    .map(|(i, _)| i)
    .collect();
  if healthy.is_empty() {
    // All unhealthy — best-effort: return all indices
    (0..upstreams.len()).collect()
  } else {
    healthy
  }
}

#[derive(Debug, Clone, Builder)]
/// Round Robin LB object as a pointer to the current serving upstream destination
pub struct LoadBalanceRoundRobin {
  #[builder(default)]
  /// Pointer to the index of the last served upstream destination
  ptr: Arc<AtomicUsize>,
}
impl LoadBalanceRoundRobin {
  /// Atomically increment ptr, reset near overflow to avoid wrapping issues.
  fn fetch_and_advance(&self) -> usize {
    let prev = self.ptr.fetch_add(1, Ordering::Relaxed);
    if prev >= usize::MAX - 1 {
      self.ptr.store(0, Ordering::Relaxed);
    }
    prev
  }
}
impl LoadBalanceWithPointer for LoadBalanceRoundRobin {
  /// Get the index of the upstream serving the incoming request using round robin among healthy upstreams.
  fn get_ptr(&self, _info: Option<&LoadBalanceContext>, upstreams: &[Upstream]) -> PointerToUpstream {
    let healthy = healthy_indices(upstreams);
    let count = self.fetch_and_advance();
    let ptr = healthy[count % healthy.len()];
    PointerToUpstream { ptr, context: None }
  }
}

#[derive(Debug, Clone, Builder)]
/// Random LB object to keep the object of random pools
pub struct LoadBalanceRandom {}

impl LoadBalanceWithPointer for LoadBalanceRandom {
  fn get_ptr(&self, _info: Option<&LoadBalanceContext>, upstreams: &[Upstream]) -> PointerToUpstream {
    let healthy = healthy_indices(upstreams);
    let mut rng = rand::rng();
    let ptr = healthy[rng.random_range(0..healthy.len())];
    PointerToUpstream { ptr, context: None }
  }
}

#[cfg(feature = "health-check")]
#[derive(Debug, Clone)]
/// Primary/Backup LB: use the first healthy upstream (lowest index).
/// Falls back to the next healthy upstream when primary is down.
/// Requires health_check to be enabled (validated at config time).
pub struct LoadBalancePrimaryBackup;

#[cfg(feature = "health-check")]
impl LoadBalanceWithPointer for LoadBalancePrimaryBackup {
  // Always pick the lowest-indexed healthy upstream (primary preference)
  // Fallback to 0 if None (this does not happen since healthy_indices returns all indices if none are healthy. Just in case.)
  fn get_ptr(&self, _info: Option<&LoadBalanceContext>, upstreams: &[Upstream]) -> PointerToUpstream {
    let healthy = healthy_indices(upstreams);
    let ptr = healthy.get(0).cloned().unwrap_or(0);
    PointerToUpstream { ptr, context: None }
  }
}

#[derive(Debug, Clone)]
/// Load Balancing Option
pub enum LoadBalance {
  /// Fix to the first upstream. Use if only one upstream destination is specified
  FixToFirst,
  /// Randomly chose one upstream server
  Random(LoadBalanceRandom),
  /// Simple round robin without session persistance
  RoundRobin(LoadBalanceRoundRobin),
  #[cfg(feature = "sticky-cookie")]
  /// Round robin with session persistance using cookie
  StickyRoundRobin(LoadBalanceSticky),
  #[cfg(feature = "health-check")]
  /// Primary/Backup: always prefer the lowest-indexed healthy upstream
  PrimaryBackup(LoadBalancePrimaryBackup),
}
impl Default for LoadBalance {
  fn default() -> Self {
    Self::FixToFirst
  }
}

impl LoadBalance {
  /// Get the index of the upstream serving the incoming request
  pub fn get_context(&self, _context_to_lb: &Option<LoadBalanceContext>, upstreams: &[Upstream]) -> PointerToUpstream {
    match self {
      LoadBalance::FixToFirst => PointerToUpstream {
        ptr: 0usize,
        context: None,
      },
      LoadBalance::RoundRobin(ptr) => ptr.get_ptr(None, upstreams),
      LoadBalance::Random(ptr) => ptr.get_ptr(None, upstreams),
      #[cfg(feature = "sticky-cookie")]
      LoadBalance::StickyRoundRobin(ptr) => ptr.get_ptr(_context_to_lb.as_ref(), upstreams),
      #[cfg(feature = "health-check")]
      LoadBalance::PrimaryBackup(ptr) => ptr.get_ptr(None, upstreams),
    }
  }
}

#[derive(Debug, Clone)]
/// Struct to handle the sticky cookie string,
/// - passed from Rp module (http handler) to LB module, manipulated from req, only StickyCookieValue exists.
/// - passed from LB module to Rp module (http handler), will be inserted into res, StickyCookieValue and Info exist.
pub struct LoadBalanceContext {
  #[cfg(feature = "sticky-cookie")]
  pub sticky_cookie: StickyCookie,
}
