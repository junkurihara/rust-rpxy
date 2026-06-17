use super::load_balance::{
  LoadBalance, LoadBalanceContext, LoadBalanceRandomBuilder, LoadBalanceRoundRobinBuilder, load_balance_options as lb_opts,
};
#[cfg(feature = "sticky-cookie")]
use super::load_balance::{LoadBalanceStickyBuilder, StickyCookieConfig};
use super::upstream_opts::UpstreamOption;
#[cfg(feature = "sticky-cookie")]
use crate::constants::STICKY_COOKIE_NAME;
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
use http::HeaderValue;
#[cfg(feature = "sticky-cookie")]
use sha2::{Digest, Sha256};
use std::borrow::Cow;
#[cfg(feature = "health-check")]
use std::sync::Arc;

#[derive(Debug, Clone)]
/// Handler for given path to route incoming request to path's corresponding upstream server(s).
pub struct PathManager {
  /// HashMap of upstream candidate server info, key is path name
  /// TODO: Reconsider HashMap + max_by_key for longest-prefix matching; a trie
  /// or radix tree may be a better fit.
  inner: HashMap<PathName, UpstreamCandidates>,
}

impl TryFrom<&AppConfig> for PathManager {
  type Error = RpxyError;
  fn try_from(app_config: &AppConfig) -> Result<Self, Self::Error> {
    let mut inner: HashMap<PathName, UpstreamCandidates> = HashMap::default();

    // A plain `for` loop (not `for_each`) so configuration errors - e.g. an invalid sticky-cookie
    // component caught while building the load balancer - propagate to the config loader instead
    // of panicking.
    for rpc in app_config.reverse_proxy.iter() {
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
        .replace_path(&rpc.replace_path);
      builder.load_balance(&rpc.load_balance, &upstream_vec, &app_config.server_name, &rpc.path)?;
      builder.options(&rpc.upstream_options);

      #[cfg(feature = "health-check")]
      builder.health_check_config(&rpc.health_check);

      let elem = builder.build().map_err(|e| {
        error!("Failed to build upstream candidates: {e}");
        RpxyError::InvalidReverseProxyConfig
      })?;
      inner.insert(elem.path.clone(), elem);
    }

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
    // Match directly on the request path bytes. `to_path_name()`/`PathName::from(&str)` does not
    // lowercase (paths are case-sensitive), so `path_str.as_bytes()` is exactly the bytes that
    // matching used before, just without allocating a `PathName` per request.
    let path_str = path_str.into();
    let path_bytes = path_str.as_bytes();

    let matched_upstream = self
      .inner
      .iter()
      .filter(|(route, _)| {
        let route_bytes: &[u8] = route.as_ref();
        path_bytes.starts_with(route_bytes) && {
          route_bytes.len() == 1 // route = '/', i.e., default
            || path_bytes.get(route_bytes.len()).map_or(
              true, // exact case
              |p| p == &b'/'
            ) // sub-path case
        }
      })
      .max_by_key(|(route, _)| route.len());
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
  /// Pre-rendered `Host` header value (host, or host:port) for the `set_upstream_host` option,
  /// computed once from `uri` at config-build time so the per-request override clone-inserts it
  /// instead of re-formatting and re-validating this constant. `None` when `uri` has no host, or
  /// (practically unreachable for a host from a valid `Uri` plus a numeric port) when the rendered
  /// value fails `HeaderValue` validation.
  host_header: Option<HeaderValue>,
  /// Health state shared with the health checker task.
  /// None if health check is not configured or explicitly disabled for upstream group this upstream belongs to.
  #[cfg(feature = "health-check")]
  pub health: Option<Arc<super::health_check::UpstreamHealth>>,
}
impl From<&UpstreamUri> for Upstream {
  fn from(value: &UpstreamUri) -> Self {
    // Render the `Host` value once. None when there is no host, or (practically unreachable for a
    // host taken from a valid `Uri` plus a numeric port) when the value fails HeaderValue
    // validation; the per-request override then yields the existing "No hostname is given" error.
    let host_header = value.inner.host().and_then(|host| {
      match value.inner.port_u16() {
        Some(port) => HeaderValue::from_str(&format!("{host}:{port}")),
        None => HeaderValue::from_str(host),
      }
      .ok()
    });
    Self {
      uri: value.inner.clone(),
      host_header,
      #[cfg(feature = "health-check")]
      health: None,
    }
  }
}
impl Upstream {
  /// The pre-rendered `Host` header value (host or host:port) used by the `set_upstream_host`
  /// option. `None` when this upstream's uri has no host, or (practically unreachable) when the
  /// rendered value failed `HeaderValue` validation at build time.
  pub(crate) fn host_header(&self) -> Option<&HeaderValue> {
    self.host_header.as_ref()
  }

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
  /// Set the load balancing option. Fallible: building the sticky-cookie config validates its
  /// AAD components (and precomputes the AAD), so an invalid configuration is rejected here -
  /// at backend build time - instead of panicking or failing per request.
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
  ) -> Result<&mut Self, RpxyError> {
    let lb = if let Some(x) = v {
      match x.as_str() {
        lb_opts::FIX_TO_FIRST => LoadBalance::FixToFirst,
        lb_opts::RANDOM => LoadBalance::Random(LoadBalanceRandomBuilder::default().build().unwrap()),
        lb_opts::ROUND_ROBIN => LoadBalance::RoundRobin(LoadBalanceRoundRobinBuilder::default().build().unwrap()),
        #[cfg(feature = "sticky-cookie")]
        lb_opts::STICKY_ROUND_ROBIN => {
          // TODO: Make sticky cookie name and duration configurable.
          let sticky_config = StickyCookieConfig::try_new(STICKY_COOKIE_NAME, server_name, path_opt, 300)?;
          LoadBalance::StickyRoundRobin(
            LoadBalanceStickyBuilder::default()
              .sticky_config(sticky_config)
              .upstream_maps(upstream_vec)
              .build()
              .unwrap(),
          )
        }
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
    Ok(self)
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

  #[test]
  fn path_manager_get_matches_longest_prefix_and_path_boundary() {
    use crate::globals::{AppConfig, ReverseProxyConfig, UpstreamUri};

    fn rp(path: Option<&str>) -> ReverseProxyConfig {
      ReverseProxyConfig {
        path: path.map(str::to_string),
        replace_path: None,
        upstream: vec![UpstreamUri {
          inner: "http://127.0.0.1:8080".parse().unwrap(),
        }],
        upstream_options: None,
        load_balance: None,
        #[cfg(feature = "health-check")]
        health_check: None,
      }
    }

    let cfg = AppConfig {
      app_name: "test".to_string(),
      server_name: "example.com".to_string(),
      reverse_proxy: vec![rp(None), rp(Some("/foo"))], // None => default "/"
      tls: None,
    };
    let pm = PathManager::try_from(&cfg).unwrap();

    // exact match on /foo
    assert_eq!(pm.get("/foo").unwrap().path, "/foo".to_path_name());
    // sub-path under /foo matches /foo (the boundary after the prefix is '/')
    assert_eq!(pm.get("/foo/bar").unwrap().path, "/foo".to_path_name());
    // /foobar must NOT be matched by /foo (boundary is not '/'); falls back to default "/"
    assert_eq!(pm.get("/foobar").unwrap().path, "/".to_path_name());
    // default route
    assert_eq!(pm.get("/").unwrap().path, "/".to_path_name());
    assert_eq!(pm.get("/other").unwrap().path, "/".to_path_name());
  }

  #[cfg(feature = "sticky-cookie")]
  #[test]
  fn calc_id_works() {
    let uri = "https://www.rust-lang.org".parse::<hyper::Uri>().unwrap();
    let upstream = Upstream {
      uri,
      host_header: None,
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
