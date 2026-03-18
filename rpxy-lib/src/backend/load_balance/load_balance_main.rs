use super::Upstream;
#[allow(unused)]
#[cfg(feature = "sticky-cookie")]
pub use super::{
  load_balance_sticky::{LoadBalanceSticky, LoadBalanceStickyBuilder},
  sticky_cookie::StickyCookie,
};
use crate::log::*;
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

#[cfg(feature = "health-check")]
fn first_healthy_index(upstreams: &[Upstream]) -> Option<usize> {
  upstreams.iter().position(Upstream::is_healthy)
}

fn healthy_index_count(upstreams: &[Upstream]) -> usize {
  upstreams.iter().filter(|u| u.is_healthy()).count()
}

/// Pick the nth healthy upstream without allocating an intermediate index list.
/// Falls back to all upstreams if every upstream is unhealthy (best-effort).
pub(super) fn pick_nth_available_index(upstreams: &[Upstream], nth: usize) -> usize {
  let len = upstreams.len();
  if len == 0 {
    // Should never happen — config validation rejects empty upstream lists.
    // Return 0 as a defensive fallback instead of panicking.
    error!("Upstream list is empty when picking nth available index");
    return 0;
  }
  let healthy_count = healthy_index_count(upstreams);

  // Fast path: all upstreams are healthy (common case, including when health-check is disabled)
  if healthy_count == len {
    trace!("All upstreams are healthy when picking nth available index");
    return nth % len;
  }

  if healthy_count == 0 {
    // When all upstreams are unhealthy, fall back to round robin among all upstreams (best-effort).
    warn!("No healthy upstreams available when picking nth available index. Picking among all upstreams as a fallback.");
    nth % len
  } else {
    let target = nth % healthy_count;
    upstreams
      .iter()
      .enumerate()
      .filter(|(_, u)| u.is_healthy())
      .nth(target)
      .map(|(i, _)| i)
      .expect("healthy upstream index must exist")
  }
}

#[cfg(feature = "health-check")]
/// Get the index of the first healthy upstream, or 0 if all are unhealthy (best-effort).
pub(super) fn first_available_index(upstreams: &[Upstream]) -> usize {
  if upstreams.is_empty() {
    // No upstreams available: return a deterministic default index instead of panicking.
    // This should not happen in practice since config validation should reject empty upstream lists, but we handle it defensively just in case.
    error!("Upstream list is empty when picking first available index");
    return 0;
  }
  first_healthy_index(upstreams).unwrap_or(0)
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
    let count = self.fetch_and_advance();
    let ptr = pick_nth_available_index(upstreams, count);
    PointerToUpstream { ptr, context: None }
  }
}

#[derive(Debug, Clone, Builder)]
/// Random LB object to keep the object of random pools
pub struct LoadBalanceRandom {}

impl LoadBalanceWithPointer for LoadBalanceRandom {
  fn get_ptr(&self, _info: Option<&LoadBalanceContext>, upstreams: &[Upstream]) -> PointerToUpstream {
    let len = upstreams.len();
    let healthy_count = healthy_index_count(upstreams);
    let mut rng = rand::rng();
    // When all upstreams are healthy or all are unhealthy, pick randomly among all upstreams.
    // Otherwise, pick randomly among healthy upstreams only.
    let ptr = if healthy_count == len || healthy_count == 0 {
      rng.random_range(0..len)
    } else {
      pick_nth_available_index(upstreams, rng.random_range(0..healthy_count))
    };
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
  // Always pick the lowest-indexed healthy upstream (primary preference).
  // Falls back to index 0 if every upstream is currently unhealthy.
  fn get_ptr(&self, _info: Option<&LoadBalanceContext>, upstreams: &[Upstream]) -> PointerToUpstream {
    let ptr = first_available_index(upstreams);
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
      LoadBalance::FixToFirst => {
        #[cfg(feature = "health-check")]
        {
          PointerToUpstream {
            ptr: first_available_index(upstreams),
            context: None,
          }
        }
        #[cfg(not(feature = "health-check"))]
        {
          PointerToUpstream {
            ptr: 0usize,
            context: None,
          }
        }
      }
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
