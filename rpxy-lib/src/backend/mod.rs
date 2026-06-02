mod backend_main;
mod load_balance;
mod upstream;
mod upstream_opts;

#[cfg(feature = "health-check")]
pub(crate) mod health_check;

#[cfg(feature = "sticky-cookie")]
pub(crate) use self::load_balance::{
  StickyCookie, StickyCookieConfig, StickyCookieValue, build_sticky_cookie_aad, build_sticky_cookie_cipher, open_server_id,
  seal_server_id,
};
#[cfg(feature = "sticky-cookie")]
pub use self::load_balance::{StickyCookieSecret, validate_sticky_cookie_aad_component};
#[allow(unused)]
pub(crate) use self::{
  load_balance::{LoadBalance, LoadBalanceContext},
  upstream::{PathManager, Upstream, UpstreamCandidates},
  upstream_opts::UpstreamOption,
};
pub(crate) use backend_main::{BackendApp, BackendAppBuilderError, BackendAppManager};

#[cfg(feature = "health-check")]
pub(crate) const LOAD_BALANCE_PRIMARY_BACKUP: &str = self::load_balance::load_balance_options::PRIMARY_BACKUP;
#[cfg(feature = "sticky-cookie")]
pub(crate) const LOAD_BALANCE_STICKY_ROUND_ROBIN: &str = self::load_balance::load_balance_options::STICKY_ROUND_ROBIN;
