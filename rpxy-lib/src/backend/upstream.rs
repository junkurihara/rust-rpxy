#[cfg(feature = "sticky-cookie")]
use super::load_balance::LoadBalanceStickyBuilder;
use super::load_balance::{
  load_balance_options as lb_opts, LoadBalance, LoadBalanceContext, LoadBalanceRandomBuilder, LoadBalanceRoundRobinBuilder,
};
// use super::{BytesName, LbContext, PathNameBytesExp, UpstreamOption};
use super::upstream_opts::UpstreamOption;
use crate::{
  error::RpxyError,
  globals::{AppConfig, UpstreamUri},
  log::*,
  name_exp::{ByteName, PathName},
};
#[cfg(feature = "sticky-cookie")]
use base64::{engine::general_purpose, Engine as _};
use derive_builder::Builder;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
#[cfg(feature = "sticky-cookie")]
use sha2::{Digest, Sha256};
use std::borrow::Cow;

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
      let upstream_vec: Vec<Upstream> = rpc.upstream.iter().map(Upstream::from).collect();
      let elem = UpstreamCandidatesBuilder::default()
        .upstream(&upstream_vec)
        .path(&rpc.path)
        .replace_path(&rpc.replace_path)
        .load_balance(&rpc.load_balance, &upstream_vec, &app_config.server_name, &rpc.path)
        .options(&rpc.upstream_options)
        .build()
        .unwrap();
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
  /// Get an appropriate upstream destinations for given path string.
  /// trie使ってlongest prefix match させてもいいけどルート記述は少ないと思われるので、
  /// コスト的にこの程度で十分では。
  pub fn get<'a>(&self, path_str: impl Into<Cow<'a, str>>) -> Option<&UpstreamCandidates> {
    let path_name = &path_str.to_path_name();

    let matched_upstream = self
      .inner
      .iter()
      .filter(|(route_bytes, _)| {
        match path_name.starts_with(route_bytes) {
          true => {
            route_bytes.len() == 1 // route = '/', i.e., default
              || match path_name.get(route_bytes.len()) {
                None => true, // exact case
                Some(p) => p == &b'/', // sub-path case
              }
          }
          _ => false,
        }
      })
      .max_by_key(|(route_bytes, _)| route_bytes.len());
    if let Some((path, u)) = matched_upstream {
      debug!(
        "Found upstream: {:?}",
        path.try_into().unwrap_or_else(|_| "<none>".to_string())
      );
      Some(u)
    } else {
      None
    }
  }
}

#[derive(Debug, Clone)]
/// Upstream struct just containing uri without path
pub struct Upstream {
  /// Base uri without specific path
  pub uri: hyper::Uri,
}
impl From<&UpstreamUri> for Upstream {
  fn from(value: &UpstreamUri) -> Self {
    Self {
      uri: value.inner.clone(),
    }
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
    upstream_vec: &[Upstream],
    _server_name: &str,
    _path_opt: &Option<String>,
  ) -> &mut Self {
    let upstream_num = &upstream_vec.len();
    let lb = if let Some(x) = v {
      match x.as_str() {
        lb_opts::FIX_TO_FIRST => LoadBalance::FixToFirst,
        lb_opts::RANDOM => LoadBalance::Random(
          LoadBalanceRandomBuilder::default()
            .num_upstreams(upstream_num)
            .build()
            .unwrap(),
        ),
        lb_opts::ROUND_ROBIN => LoadBalance::RoundRobin(
          LoadBalanceRoundRobinBuilder::default()
            .num_upstreams(upstream_num)
            .build()
            .unwrap(),
        ),
        #[cfg(feature = "sticky-cookie")]
        lb_opts::STICKY_ROUND_ROBIN => LoadBalance::StickyRoundRobin(
          LoadBalanceStickyBuilder::default()
            .num_upstreams(upstream_num)
            .sticky_config(_server_name, _path_opt)
            .upstream_maps(upstream_vec) // TODO:
            .build()
            .unwrap(),
        ),
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
  /// Set the activated upstream options defined in [[UpstreamOption]]
  pub fn options(&mut self, v: &Option<Vec<String>>) -> &mut Self {
    let opts = if let Some(opts) = v {
      opts
        .iter()
        .filter_map(|str| UpstreamOption::try_from(str.as_str()).ok())
        .collect::<HashSet<UpstreamOption>>()
    } else {
      Default::default()
    };
    self.options = Some(opts);
    self
  }
}

impl UpstreamCandidates {
  /// Get an enabled option of load balancing [[LoadBalance]]
  pub fn get(&self, context_to_lb: &Option<LoadBalanceContext>) -> (Option<&Upstream>, Option<LoadBalanceContext>) {
    let pointer_to_upstream = self.load_balance.get_context(context_to_lb);
    debug!("Upstream of index {} is chosen.", pointer_to_upstream.ptr);
    debug!("Context to LB (Cookie in Request): {:?}", context_to_lb);
    debug!("Context from LB (Set-Cookie in Response): {:?}", pointer_to_upstream.context);
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
    let upstream = Upstream { uri };
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
