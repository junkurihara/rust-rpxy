mod backend_main;
mod failover;
mod load_balance;
mod upstream;
mod upstream_opts;

#[cfg(feature = "health-check")]
pub(crate) mod health_check;

#[cfg(feature = "sticky-cookie")]
pub(crate) use self::load_balance::{StickyCookie, StickyCookieValue};
#[allow(unused)]
pub(crate) use self::{
  failover::{FailoverConfig, FailoverContext},
  load_balance::{LoadBalance, LoadBalanceContext},
  upstream::{PathManager, Upstream, UpstreamCandidates},
  upstream_opts::UpstreamOption,
};
pub(crate) use backend_main::{BackendApp, BackendAppBuilderError, BackendAppManager};

#[cfg(feature = "health-check")]
pub(crate) const LOAD_BALANCE_PRIMARY_BACKUP: &str = self::load_balance::load_balance_options::PRIMARY_BACKUP;
