mod backend_main;
mod load_balance;
mod upstream;
mod upstream_opts;

#[cfg(feature = "health-check")]
pub(crate) mod health_check;

#[cfg(feature = "sticky-cookie")]
pub(crate) use self::load_balance::{
  StickyCookie, StickyCookieConfig, StickyCookieValue, build_sticky_cookie_cipher, open_server_id, seal_server_id,
};
#[cfg(feature = "sticky-cookie")]
pub use self::load_balance::{StickyCookieSecret, validate_sticky_cookie_aad_component};
// `LoadBalanceContext` crosses the backend boundary only for the sticky-cookie response path.
#[cfg(feature = "sticky-cookie")]
pub(crate) use self::load_balance::LoadBalanceContext;
// `LoadBalance` is referenced outside `backend` only by the sticky-cookie dispatch and by the
// header_ops upstream unit tests; its module is private so consumers cannot take a direct path.
#[cfg(any(feature = "sticky-cookie", test))]
pub(crate) use self::load_balance::LoadBalance;
pub(crate) use self::{
  upstream::{Upstream, UpstreamCandidates},
  upstream_opts::UpstreamOption,
};
pub(crate) use backend_main::{BackendApp, BackendAppBuilderError, BackendAppManager};

#[cfg(feature = "health-check")]
pub(crate) const LOAD_BALANCE_PRIMARY_BACKUP: &str = self::load_balance::load_balance_options::PRIMARY_BACKUP;
#[cfg(feature = "sticky-cookie")]
pub(crate) const LOAD_BALANCE_STICKY_ROUND_ROBIN: &str = self::load_balance::load_balance_options::STICKY_ROUND_ROBIN;
