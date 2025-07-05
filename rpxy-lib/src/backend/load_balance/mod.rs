mod load_balance_main;
#[cfg(feature = "sticky-cookie")]
mod load_balance_sticky;
#[cfg(feature = "sticky-cookie")]
mod sticky_cookie;

#[cfg(feature = "sticky-cookie")]
use super::upstream::Upstream;
use thiserror::Error;

pub use load_balance_main::{
  LoadBalance, LoadBalanceContext, LoadBalanceRandomBuilder, LoadBalanceRoundRobinBuilder, load_balance_options,
};
#[cfg(feature = "sticky-cookie")]
pub use load_balance_sticky::LoadBalanceStickyBuilder;
#[cfg(feature = "sticky-cookie")]
pub use sticky_cookie::{StickyCookie, StickyCookieValue};

/// Result type for load balancing
#[cfg(feature = "sticky-cookie")]
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
