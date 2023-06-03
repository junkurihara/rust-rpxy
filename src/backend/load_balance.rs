use derive_builder::Builder;
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

//
//   /// Counter for load balancing
//   pub cnt: UpstreamCount,

// TODO: カウンタの移動
#[derive(Debug, Clone, Builder)]
pub struct LbRoundRobinCount {
  #[builder(default)]
  cnt: Arc<AtomicUsize>,
  #[builder(setter(custom), default)]
  max_val: usize,
}
impl LbRoundRobinCountBuilder {
  pub fn max_val(&mut self, v: &usize) -> &mut Self {
    self.max_val = Some(*v);
    self
  }
}
impl LbRoundRobinCount {
  /// Get a current count of upstream served
  fn current_cnt(&self) -> usize {
    self.cnt.load(Ordering::Relaxed)
  }

  /// Increment the count of upstream served up to the max value
  pub fn increment_cnt(&self) -> usize {
    if self.current_cnt() < self.max_val - 1 {
      self.cnt.fetch_add(1, Ordering::Relaxed)
    } else {
      // Clear the counter
      self.cnt.fetch_and(0, Ordering::Relaxed)
    }
  }
}

#[derive(Debug, Clone)]
/// Load Balancing Option
pub enum LoadBalance {
  /// Fix to the first upstream. Use if only one upstream destination is specified
  FixToFirst,
  /// Simple round robin without session persistance
  RoundRobin(LbRoundRobinCount), // TODO: カウンタはここにいれる。randomとかには不要なので
  /// Randomly chose one upstream server
  Random,
  /// Round robin with session persistance using cookie
  StickyRoundRobin,
}
impl Default for LoadBalance {
  fn default() -> Self {
    Self::FixToFirst
  }
}
