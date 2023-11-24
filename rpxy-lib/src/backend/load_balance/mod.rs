mod load_balance_main;
#[cfg(feature = "sticky-cookie")]
mod load_balance_sticky;
#[cfg(feature = "sticky-cookie")]
mod sticky_cookie;

use super::upstream::Upstream;
use thiserror::Error;

pub use load_balance_main::{
  load_balance_options, LoadBalance, LoadBalanceContext, LoadBalanceRandomBuilder, LoadBalanceRoundRobinBuilder,
};
#[cfg(feature = "sticky-cookie")]
pub use load_balance_sticky::LoadBalanceStickyBuilder;

/// Result type for load balancing
type LoadBalanceResult<T> = std::result::Result<T, LoadBalanceError>;
/// Describes things that can go wrong in the Load Balance
#[derive(Debug, Error)]
pub enum LoadBalanceError {
  // backend load balance errors
  #[cfg(feature = "sticky-cookie")]
  #[error("Failed to cookie conversion to/from string")]
  FailedToConversionStickyCookie,

  #[cfg(feature = "sticky-cookie")]
  #[error("Invalid cookie structure")]
  InvalidStickyCookieStructure,

  #[cfg(feature = "sticky-cookie")]
  #[error("No sticky cookie value")]
  NoStickyCookieValue,

  #[cfg(feature = "sticky-cookie")]
  #[error("Failed to cookie conversion into string: no meta information")]
  NoStickyCookieNoMetaInfo,

  #[cfg(feature = "sticky-cookie")]
  #[error("Failed to build sticky cookie from config")]
  FailedToBuildStickyCookie,
}
