#[allow(unused)]
#[cfg(feature = "sticky-cookie")]
pub use super::{
  load_balance_sticky::{LoadBalanceSticky, LoadBalanceStickyBuilder},
  sticky_cookie::StickyCookie,
};
use derive_builder::Builder;
use rand::Rng;
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Arc,
};

/// Constants to specify a load balance option
pub mod load_balance_options {
  pub const FIX_TO_FIRST: &str = "none";
  pub const ROUND_ROBIN: &str = "round_robin";
  pub const RANDOM: &str = "random";
  #[cfg(feature = "sticky-cookie")]
  pub const STICKY_ROUND_ROBIN: &str = "sticky";
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
  fn get_ptr(&self, req_info: Option<&LoadBalanceContext>) -> PointerToUpstream;
}

#[derive(Debug, Clone, Builder)]
/// Round Robin LB object as a pointer to the current serving upstream destination
pub struct LoadBalanceRoundRobin {
  #[builder(default)]
  /// Pointer to the index of the last served upstream destination
  ptr: Arc<AtomicUsize>,
  #[builder(setter(custom), default)]
  /// Number of upstream destinations
  num_upstreams: usize,
}
impl LoadBalanceRoundRobinBuilder {
  pub fn num_upstreams(&mut self, v: &usize) -> &mut Self {
    self.num_upstreams = Some(*v);
    self
  }
}
impl LoadBalanceWithPointer for LoadBalanceRoundRobin {
  /// Increment the count of upstream served up to the max value
  fn get_ptr(&self, _info: Option<&LoadBalanceContext>) -> PointerToUpstream {
    // Get a current count of upstream served
    let current_ptr = self.ptr.load(Ordering::Relaxed);

    let ptr = if current_ptr < self.num_upstreams - 1 {
      self.ptr.fetch_add(1, Ordering::Relaxed)
    } else {
      // Clear the counter
      self.ptr.fetch_and(0, Ordering::Relaxed)
    };
    PointerToUpstream { ptr, context: None }
  }
}

#[derive(Debug, Clone, Builder)]
/// Random LB object to keep the object of random pools
pub struct LoadBalanceRandom {
  #[builder(setter(custom), default)]
  /// Number of upstream destinations
  num_upstreams: usize,
}
impl LoadBalanceRandomBuilder {
  pub fn num_upstreams(&mut self, v: &usize) -> &mut Self {
    self.num_upstreams = Some(*v);
    self
  }
}
impl LoadBalanceWithPointer for LoadBalanceRandom {
  /// Returns the random index within the range
  fn get_ptr(&self, _info: Option<&LoadBalanceContext>) -> PointerToUpstream {
    let mut rng = rand::thread_rng();
    let ptr = rng.gen_range(0..self.num_upstreams);
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
}
impl Default for LoadBalance {
  fn default() -> Self {
    Self::FixToFirst
  }
}

impl LoadBalance {
  /// Get the index of the upstream serving the incoming request
  pub fn get_context(&self, _context_to_lb: &Option<LoadBalanceContext>) -> PointerToUpstream {
    match self {
      LoadBalance::FixToFirst => PointerToUpstream {
        ptr: 0usize,
        context: None,
      },
      LoadBalance::RoundRobin(ptr) => ptr.get_ptr(None),
      LoadBalance::Random(ptr) => ptr.get_ptr(None),
      #[cfg(feature = "sticky-cookie")]
      LoadBalance::StickyRoundRobin(ptr) => {
        // Generate new context if sticky round robin is enabled.
        ptr.get_ptr(_context_to_lb.as_ref())
      }
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
  #[cfg(not(feature = "sticky-cookie"))]
  pub sticky_cookie: (),
}
