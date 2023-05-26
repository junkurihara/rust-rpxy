/// Constants to specify a load balance option
pub(super) mod load_balance_options {
  pub const FIX_TO_FIRST: &str = "none";
  pub const ROUND_ROBIN: &str = "round_robin";
  pub const RANDOM: &str = "random";
  pub const STICKY_ROUND_ROBIN: &str = "sticky";
}

#[derive(Debug, Clone)]
/// Load Balancing Option
pub enum LoadBalance {
  /// Fix to the first upstream. Use if only one upstream destination is specified
  FixToFirst,
  /// Simple round robin without session persistance
  RoundRobin, // TODO: カウンタはここにいれる。randomとかには不要なので
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
