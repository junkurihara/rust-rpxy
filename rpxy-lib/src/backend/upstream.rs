#[cfg(feature = "sticky-cookie")]
use super::load_balance::LoadBalanceStickyBuilder;
use super::load_balance::{
  LoadBalance, LoadBalanceContext, LoadBalanceRandomBuilder, LoadBalanceRoundRobinBuilder, load_balance_options as lb_opts,
};
use super::upstream_opts::UpstreamOption;
#[cfg(feature = "health-check")]
use crate::globals::HealthCheckConfig;
use crate::{
  error::RpxyError,
  globals::{AppConfig, UpstreamUri},
  log::*,
  name_exp::{ByteName, PathName},
};
use ahash::{HashMap, HashSet};
#[cfg(feature = "sticky-cookie")]
use base64::{Engine as _, engine::general_purpose};
use derive_builder::Builder;
#[cfg(feature = "sticky-cookie")]
use sha2::{Digest, Sha256};
use std::borrow::Cow;
#[cfg(feature = "health-check")]
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone)]
/// Handler for given path to route incoming request to path's corresponding upstream server(s).
pub struct PathManager {
  /// HashMap of upstream candidate server info, key is path name
  /// TODO: HashMapでいいのかは疑問。max_by_keyでlongest prefix matchしてるのも無駄っぽいが。。。
  inner: HashMap<PathName, UpstreamCandidates>,
}

impl TryFrom<&AppConfig> for PathManager {
  type Error = RpxyError;
  fn try_from(app_config: &AppConfig) -> Result<Self, Self::Error> {
    let mut inner: HashMap<PathName, UpstreamCandidates> = HashMap::default();

    app_config.reverse_proxy.iter().for_each(|rpc| {
      #[cfg(not(feature = "health-check"))]
      let upstream_vec: Vec<Upstream> = rpc.upstream.iter().map(Upstream::from).collect();
      #[cfg(feature = "health-check")]
      let upstream_vec: Vec<Upstream> = rpc
        .upstream
        .iter()
        .map(Upstream::from)
        .map(|u| Upstream {
          health: rpc
            .health_check
            .as_ref()
            .map(|_| Arc::new(super::health_check::UpstreamHealth::new())),
          ..u
        })
        .collect();

      let mut builder = UpstreamCandidatesBuilder::default();
      builder
        .upstream(&upstream_vec)
        .path(&rpc.path)
        .replace_path(&rpc.replace_path)
        .load_balance(&rpc.load_balance, &upstream_vec, &app_config.server_name, &rpc.path)
        .options(&rpc.upstream_options)
        .concurrency_limiter(&rpc.max_upstream_concurrency)
        .min_request_interval(&rpc.min_request_interval);

      #[cfg(feature = "health-check")]
      builder.health_check_config(&rpc.health_check);

      let elem = builder.build().unwrap();
      inner.insert(elem.path.clone(), elem);
    });

    if app_config.reverse_proxy.iter().filter(|rpc| rpc.path.is_none()).count() >= 2 {
      error!("Multiple default reverse proxy setting");
      return Err(RpxyError::InvalidReverseProxyConfig);
    }

    if !(inner.iter().all(|(_, elem)| {
      !(elem.options.contains(&UpstreamOption::ForceHttp11Upstream) && elem.options.contains(&UpstreamOption::ForceHttp2Upstream))
    })) {
      error!("Either one of force_http11 or force_http2 can be enabled");
      return Err(RpxyError::InvalidUpstreamOptionSetting);
    }

    Ok(PathManager { inner })
  }
}

impl PathManager {
  #[cfg(feature = "health-check")]
  pub(crate) fn iter_candidates(&self) -> impl Iterator<Item = (&PathName, &UpstreamCandidates)> {
    self.inner.iter()
  }

  /// Get an appropriate upstream destinations for given path string.
  /// trie使ってlongest prefix match させてもいいけどルート記述は少ないと思われるので、
  /// コスト的にこの程度で十分では。
  pub fn get<'a>(&self, path_str: impl Into<Cow<'a, str>>) -> Option<&UpstreamCandidates> {
    let path_name = &path_str.to_path_name();

    let matched_upstream = self
      .inner
      .iter()
      .filter(|(route_bytes, _)| {
        path_name.starts_with(route_bytes) && {
          route_bytes.len() == 1 // route = '/', i.e., default
            || path_name.get(route_bytes.len()).map_or(
              true, // exact case
              |p| p == &b'/'
            ) // sub-path case
        }
      })
      .max_by_key(|(route_bytes, _)| route_bytes.len());
    matched_upstream.map(|(path, u)| {
      trace!(
        "Found upstream: {:?}",
        path.try_into().unwrap_or_else(|_| "<none>".to_string())
      );
      u
    })
  }
}

#[derive(Debug, Clone)]
/// Upstream struct just containing uri without path
pub struct Upstream {
  /// Base uri without specific path
  pub uri: hyper::Uri,
  /// Health state shared with the health checker task.
  /// None if health check is not configured or explicitly disabled for upstream group this upstream belongs to.
  #[cfg(feature = "health-check")]
  pub health: Option<Arc<super::health_check::UpstreamHealth>>,
}
impl From<&UpstreamUri> for Upstream {
  fn from(value: &UpstreamUri) -> Self {
    Self {
      uri: value.inner.clone(),
      #[cfg(feature = "health-check")]
      health: None,
    }
  }
}
impl Upstream {
  #[allow(unused)]
  /// Returns whether this upstream is considered healthy.
  /// Always returns true if health check is not configured.
  pub fn is_healthy(&self) -> bool {
    #[cfg(feature = "health-check")]
    {
      self.health.as_ref().map_or(true, |h| h.is_healthy())
    }
    #[cfg(not(feature = "health-check"))]
    {
      true
    }
  }

  /// Returns whether this upstream has health check state attached.
  #[cfg(feature = "health-check")]
  pub fn has_health_state(&self) -> bool {
    self.health.is_some()
  }
}
impl Upstream {
  #[cfg(feature = "sticky-cookie")]
  /// Hashing uri with index to avoid collision
  pub fn calculate_id_with_index(&self, index: usize) -> String {
    let mut hasher = Sha256::new();
    let uri_string = format!("{}&index={}", self.uri.clone(), index);
    hasher.update(uri_string.as_bytes());
    let digest = hasher.finalize();
    general_purpose::URL_SAFE_NO_PAD.encode(digest)
  }
}
/// Tracks the minimum interval between consecutive forwarded requests.
///
/// Uses a "reserve timestamp" pattern: when a task needs to wait, it immediately
/// reserves a future slot so the next task can compute its correct wait time,
/// preventing thundering-herd issues when multiple tasks arrive simultaneously.
///
/// If the task is cancelled while sleeping, the reserved timestamp is rolled back
/// to prevent unbounded drift that would block subsequent requests.
pub struct RequestIntervalTracker {
  /// The configured minimum interval duration
  duration: Duration,
  /// Timestamp of the last (or reserved) forwarded request start; `None` until the first request
  last_request_at: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}

impl std::fmt::Debug for RequestIntervalTracker {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("RequestIntervalTracker")
      .field("duration", &self.duration)
      .finish()
  }
}

impl RequestIntervalTracker {
  pub fn new(duration: Duration) -> Self {
    Self {
      duration,
      last_request_at: tokio::sync::Mutex::new(None),
    }
  }

  /// Enforces the minimum interval between consecutive forwarded requests.
  ///
  /// Must be called AFTER acquiring any concurrency semaphore and BEFORE forwarding.
  /// The interval is measured between the *start* of each forwarded request.
  ///
  /// Cancellation-safe: if the caller's future is dropped while sleeping, the reserved
  /// timestamp is rolled back so subsequent requests are not blocked by a phantom reservation.
  pub async fn enforce_interval(&self) {
    let wait_duration = {
      let mut last = self.last_request_at.lock().await;
      let now = tokio::time::Instant::now();
      match *last {
        Some(prev_start) => {
          let earliest_start = prev_start + self.duration;
          if now >= earliest_start {
            // Enough time has elapsed — proceed immediately
            *last = Some(now);
            None
          } else {
            // Need to wait. Reserve our slot so next caller sees the correct timestamp.
            let wait = earliest_start - now;
            *last = Some(earliest_start);
            Some((wait, earliest_start, prev_start))
          }
        }
        None => {
          // First request ever — no waiting needed
          *last = Some(now);
          None
        }
      }
    };

    if let Some((wait, reserved, _prev_start)) = wait_duration {
      tokio::time::sleep(wait).await;
      // After waking, update to actual start time (may differ slightly from reserved due to scheduling).
      // If the sleep was cancelled (future dropped), this line won't execute and we need to roll back.
      let mut last = self.last_request_at.lock().await;
      if *last == Some(reserved) {
        // Still our reservation — we weren't cancelled. Update to actual start time.
        *last = Some(tokio::time::Instant::now());
      }
      // If the reservation was already overwritten (e.g. by a timeout rollback), do nothing.
    }
  }
}

#[derive(Debug, Clone, Builder)]
/// Struct serving multiple upstream servers for, e.g., load balancing.
pub struct UpstreamCandidates {
  #[builder(setter(custom))]
  /// Upstream server(s)
  pub inner: Vec<Upstream>,

  #[builder(setter(custom), default)]
  /// Path like "/path" in [[PathName]] associated with the upstream server(s)
  pub path: PathName,

  #[builder(setter(custom), default)]
  /// Path in [[PathName]] that will be used to replace the "path" part of incoming url
  pub replace_path: Option<PathName>,

  #[builder(setter(custom), default)]
  /// Load balancing option
  pub load_balance: LoadBalance,

  #[builder(setter(custom), default)]
  /// Activated upstream options defined in [[UpstreamOption]]
  pub options: HashSet<UpstreamOption>,

  /// Limits concurrent in-flight requests to upstream.
  /// `None` = unlimited (default). `Some(Arc<Semaphore>)` = FIFO queue.
  #[builder(setter(custom), default)]
  pub concurrency_limiter: Option<Arc<tokio::sync::Semaphore>>,

  /// Minimum interval between consecutive forwarded requests.
  /// When `Some`, ensures at least this duration elapses between the START of each forwarded request.
  #[builder(setter(custom), default)]
  pub min_request_interval: Option<Arc<RequestIntervalTracker>>,

  #[cfg(feature = "health-check")]
  #[builder(setter(custom), default)]
  /// Health check configuration for this upstream group
  pub health_check_config: Option<HealthCheckConfig>,
}

impl UpstreamCandidatesBuilder {
  /// Set the upstream server(s)
  pub fn upstream(&mut self, upstream_vec: &[Upstream]) -> &mut Self {
    self.inner = Some(upstream_vec.to_vec());
    self
  }
  /// Set the path like "/path" in [[PathName]] associated with the upstream server(s), default is "/"
  pub fn path(&mut self, v: &Option<String>) -> &mut Self {
    let path = match v {
      Some(p) => p.to_path_name(),
      None => "/".to_path_name(),
    };
    self.path = Some(path);
    self
  }
  /// Set the path in [[PathName]] that will be used to replace the "path" part of incoming url
  pub fn replace_path(&mut self, v: &Option<String>) -> &mut Self {
    self.replace_path = Some(v.to_owned().as_ref().map_or_else(|| None, |v| Some(v.to_path_name())));
    self
  }
  /// Set the load balancing option
  pub fn load_balance(
    &mut self,
    v: &Option<String>,
    // upstream_num: &usize,
    #[cfg(feature = "sticky-cookie")] upstream_vec: &[Upstream],
    #[cfg(not(feature = "sticky-cookie"))] _upstream_vec: &[Upstream],
    #[cfg(feature = "sticky-cookie")] server_name: &str,
    #[cfg(not(feature = "sticky-cookie"))] _server_name: &str,
    #[cfg(feature = "sticky-cookie")] path_opt: &Option<String>,
    #[cfg(not(feature = "sticky-cookie"))] _path_opt: &Option<String>,
  ) -> &mut Self {
    let lb = if let Some(x) = v {
      match x.as_str() {
        lb_opts::FIX_TO_FIRST => LoadBalance::FixToFirst,
        lb_opts::RANDOM => LoadBalance::Random(LoadBalanceRandomBuilder::default().build().unwrap()),
        lb_opts::ROUND_ROBIN => LoadBalance::RoundRobin(LoadBalanceRoundRobinBuilder::default().build().unwrap()),
        #[cfg(feature = "sticky-cookie")]
        lb_opts::STICKY_ROUND_ROBIN => LoadBalance::StickyRoundRobin(
          LoadBalanceStickyBuilder::default()
            .sticky_config(server_name, path_opt)
            .upstream_maps(upstream_vec) // TODO:
            .build()
            .unwrap(),
        ),
        #[cfg(feature = "health-check")]
        lb_opts::PRIMARY_BACKUP => LoadBalance::PrimaryBackup(super::load_balance::LoadBalancePrimaryBackup),
        _ => {
          error!("Specified load balancing option is invalid.");
          LoadBalance::default()
        }
      }
    } else {
      LoadBalance::default()
    };
    self.load_balance = Some(lb);
    self
  }

  #[cfg(feature = "health-check")]
  /// Set the health check configuration
  pub fn health_check_config(&mut self, v: &Option<HealthCheckConfig>) -> &mut Self {
    self.health_check_config = Some(v.clone());
    self
  }

  /// Set the activated upstream options defined in [[UpstreamOption]]
  pub fn options(&mut self, v: &Option<Vec<String>>) -> &mut Self {
    let opts = v.as_ref().map_or_else(
      || Default::default(),
      |opts| {
        opts
          .iter()
          .filter_map(|str| UpstreamOption::try_from(str.as_str()).ok())
          .collect::<HashSet<UpstreamOption>>()
      },
    );
    self.options = Some(opts);
    self
  }

  /// Set the maximum upstream concurrency from optional permit count
  pub fn concurrency_limiter(&mut self, v: &Option<usize>) -> &mut Self {
    self.concurrency_limiter = Some(v.as_ref().map(|n| Arc::new(tokio::sync::Semaphore::new(*n))));
    self
  }

  /// Set the minimum request interval from optional duration
  pub fn min_request_interval(&mut self, v: &Option<Duration>) -> &mut Self {
    self.min_request_interval = Some(v.as_ref().map(|d| Arc::new(RequestIntervalTracker::new(*d))));
    self
  }
}

impl UpstreamCandidates {
  /// Get an enabled option of load balancing [[LoadBalance]]
  pub fn get(&self, context_to_lb: &Option<LoadBalanceContext>) -> (Option<&Upstream>, Option<LoadBalanceContext>) {
    let pointer_to_upstream = self.load_balance.get_context(context_to_lb, &self.inner);
    trace!("Upstream of index {} is chosen.", pointer_to_upstream.ptr);
    trace!("Context to LB (Cookie in Request): {:?}", context_to_lb);
    trace!("Context from LB (Set-Cookie in Response): {:?}", pointer_to_upstream.context);
    (self.inner.get(pointer_to_upstream.ptr), pointer_to_upstream.context)
  }
}

#[cfg(test)]
mod test {
  #[allow(unused)]
  use super::*;

  #[cfg(feature = "sticky-cookie")]
  #[test]
  fn calc_id_works() {
    let uri = "https://www.rust-lang.org".parse::<hyper::Uri>().unwrap();
    let upstream = Upstream {
      uri,
      #[cfg(feature = "health-check")]
      health: None,
    };
    assert_eq!(
      "eGsjoPbactQ1eUJjafYjPT3ekYZQkaqJnHdA_FMSkgM",
      upstream.calculate_id_with_index(0)
    );
    assert_eq!(
      "tNVXFJ9eNCT2mFgKbYq35XgH5q93QZtfU8piUiiDxVA",
      upstream.calculate_id_with_index(1)
    );
  }
}
