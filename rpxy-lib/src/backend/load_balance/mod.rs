mod load_balance_main;
#[cfg(feature = "sticky-cookie")]
mod load_balance_sticky;
#[cfg(feature = "sticky-cookie")]
mod sticky_cookie;
#[cfg(feature = "sticky-cookie")]
mod sticky_cookie_seal;

use super::upstream::Upstream;
use thiserror::Error;

#[cfg(feature = "health-check")]
pub use load_balance_main::LoadBalancePrimaryBackup;
pub use load_balance_main::{
  LoadBalance, LoadBalanceContext, LoadBalanceRandomBuilder, LoadBalanceRoundRobinBuilder, load_balance_options,
};
#[cfg(feature = "sticky-cookie")]
pub use load_balance_sticky::LoadBalanceStickyBuilder;
#[cfg(feature = "sticky-cookie")]
pub use sticky_cookie::{StickyCookie, StickyCookieConfig, StickyCookieValue};
#[cfg(feature = "sticky-cookie")]
pub use sticky_cookie_seal::{StickyCookieSecret, validate_sticky_cookie_aad_component};
#[cfg(feature = "sticky-cookie")]
pub(crate) use sticky_cookie_seal::{build_sticky_cookie_cipher, open_server_id, seal_server_id};

/// Result type for load balancing
#[cfg(feature = "sticky-cookie")]
type LoadBalanceResult<T> = std::result::Result<T, LoadBalanceError>;
/// Describes things that can go wrong in the Load Balance
#[allow(unused)]
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
