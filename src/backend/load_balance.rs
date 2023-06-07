use derive_builder::Builder;
use rand::Rng;
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Arc,
};

/// Constants to specify a load balance option
pub(super) mod load_balance_options {
  pub const FIX_TO_FIRST: &str = "none";
  pub const ROUND_ROBIN: &str = "round_robin";
  pub const RANDOM: &str = "random";
  pub const STICKY_ROUND_ROBIN: &str = "sticky";
}

#[derive(Debug, Clone, Builder)]
/// Round Robin LB object as a pointer to the current serving upstream destination
pub struct LbRoundRobin {
  #[builder(default)]
  /// Pointer to the index of the last served upstream destination
  ptr: Arc<AtomicUsize>,
  #[builder(setter(custom), default)]
  /// Number of upstream destinations
  num_upstreams: usize,
}
impl LbRoundRobinBuilder {
  pub fn num_upstreams(&mut self, v: &usize) -> &mut Self {
    self.num_upstreams = Some(*v);
    self
  }
}
impl LbRoundRobin {
  /// Get a current count of upstream served
  fn current_ptr(&self) -> usize {
    self.ptr.load(Ordering::Relaxed)
  }

  /// Increment the count of upstream served up to the max value
  pub fn increment_ptr(&self) -> usize {
    if self.current_ptr() < self.num_upstreams - 1 {
      self.ptr.fetch_add(1, Ordering::Relaxed)
    } else {
      // Clear the counter
      self.ptr.fetch_and(0, Ordering::Relaxed)
    }
  }
}

#[derive(Debug, Clone, Builder)]
/// Random LB object to keep the object of random pools
pub struct LbRandom {
  #[builder(setter(custom), default)]
  /// Number of upstream destinations
  num_upstreams: usize,
}
impl LbRandomBuilder {
  pub fn num_upstreams(&mut self, v: &usize) -> &mut Self {
    self.num_upstreams = Some(*v);
    self
  }
}
impl LbRandom {
  /// Returns the random index within the range
  pub fn get_ptr(&self) -> usize {
    let mut rng = rand::thread_rng();
    rng.gen_range(0..self.num_upstreams)
  }
}

#[derive(Debug, Clone)]
/// Load Balancing Option
pub enum LoadBalance {
  /// Fix to the first upstream. Use if only one upstream destination is specified
  FixToFirst,
  /// Randomly chose one upstream server
  Random(LbRandom),
  /// Simple round robin without session persistance
  RoundRobin(LbRoundRobin),
  /// Round robin with session persistance using cookie
  StickyRoundRobin(LbRoundRobin),
}
impl Default for LoadBalance {
  fn default() -> Self {
    Self::FixToFirst
  }
}

impl LoadBalance {
  /// Get the index of the upstream serving the incoming request
  pub(super) fn get_idx(&self) -> usize {
    match self {
      LoadBalance::FixToFirst => 0usize,
      LoadBalance::RoundRobin(ptr) => ptr.increment_ptr(),
      LoadBalance::Random(v) => v.get_ptr(),
      LoadBalance::StickyRoundRobin(_ptr) => 0usize, // todo!(), // TODO: TODO: TODO: TODO: tentative value
    }
  }
}
