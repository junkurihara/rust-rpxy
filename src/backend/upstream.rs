#[cfg(feature = "sticky-cookie")]
use super::load_balance::LbStickyRoundRobinBuilder;
use super::load_balance::{load_balance_options as lb_opts, LbRandomBuilder, LbRoundRobinBuilder, LoadBalance};
use super::{BytesName, LbContext, PathNameBytesExp, UpstreamOption};
use crate::log::*;
#[cfg(feature = "sticky-cookie")]
use base64::{engine::general_purpose, Engine as _};
use derive_builder::Builder;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
#[cfg(feature = "sticky-cookie")]
use sha2::{Digest, Sha256};
use std::borrow::Cow;
#[derive(Debug, Clone)]
pub struct ReverseProxy {
  pub upstream: HashMap<PathNameBytesExp, UpstreamGroup>, // TODO: HashMapでいいのかは疑問。max_by_keyでlongest prefix matchしてるのも無駄っぽいが。。。
}

impl ReverseProxy {
  /// Get an appropriate upstream destination for given path string.
  pub fn get<'a>(&self, path_str: impl Into<Cow<'a, str>>) -> Option<&UpstreamGroup> {
    // trie使ってlongest prefix match させてもいいけどルート記述は少ないと思われるので、
    // コスト的にこの程度で十分
    let path_bytes = &path_str.to_path_name_vec();

    let matched_upstream = self
      .upstream
      .iter()
      .filter(|(route_bytes, _)| {
        match path_bytes.starts_with(route_bytes) {
          true => {
            route_bytes.len() == 1 // route = '/', i.e., default
            || match path_bytes.get(route_bytes.len()) {
              None => true, // exact case
              Some(p) => p == &b'/', // sub-path case
            }
          }
          _ => false,
        }
      })
      .max_by_key(|(route_bytes, _)| route_bytes.len());
    if let Some((_path, u)) = matched_upstream {
      debug!(
        "Found upstream: {:?}",
        String::from_utf8(_path.0.clone()).unwrap_or_else(|_| "<none>".to_string())
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
pub struct UpstreamGroup {
  #[builder(setter(custom))]
  /// Upstream server(s)
  pub upstream: Vec<Upstream>,
  #[builder(setter(custom), default)]
  /// Path like "/path" in [[PathNameBytesExp]] associated with the upstream server(s)
  pub path: PathNameBytesExp,
  #[builder(setter(custom), default)]
  /// Path in [[PathNameBytesExp]] that will be used to replace the "path" part of incoming url
  pub replace_path: Option<PathNameBytesExp>,

  #[builder(setter(custom), default)]
  /// Load balancing option
  pub lb: LoadBalance,
  #[builder(setter(custom), default)]
  /// Activated upstream options defined in [[UpstreamOption]]
  pub opts: HashSet<UpstreamOption>,
}

impl UpstreamGroupBuilder {
  pub fn upstream(&mut self, upstream_vec: &[Upstream]) -> &mut Self {
    self.upstream = Some(upstream_vec.to_vec());
    self
  }
  pub fn path(&mut self, v: &Option<String>) -> &mut Self {
    let path = match v {
      Some(p) => p.to_path_name_vec(),
      None => "/".to_path_name_vec(),
    };
    self.path = Some(path);
    self
  }
  pub fn replace_path(&mut self, v: &Option<String>) -> &mut Self {
    self.replace_path = Some(
      v.to_owned()
        .as_ref()
        .map_or_else(|| None, |v| Some(v.to_path_name_vec())),
    );
    self
  }
  pub fn lb(
    &mut self,
    v: &Option<String>,
    // upstream_num: &usize,
    upstream_vec: &Vec<Upstream>,
    _server_name: &str,
    _path_opt: &Option<String>,
  ) -> &mut Self {
    let upstream_num = &upstream_vec.len();
    let lb = if let Some(x) = v {
      match x.as_str() {
        lb_opts::FIX_TO_FIRST => LoadBalance::FixToFirst,
        lb_opts::RANDOM => LoadBalance::Random(LbRandomBuilder::default().num_upstreams(upstream_num).build().unwrap()),
        lb_opts::ROUND_ROBIN => LoadBalance::RoundRobin(
          LbRoundRobinBuilder::default()
            .num_upstreams(upstream_num)
            .build()
            .unwrap(),
        ),
        #[cfg(feature = "sticky-cookie")]
        lb_opts::STICKY_ROUND_ROBIN => LoadBalance::StickyRoundRobin(
          LbStickyRoundRobinBuilder::default()
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
    self.lb = Some(lb);
    self
  }
  pub fn opts(&mut self, v: &Option<Vec<String>>) -> &mut Self {
    let opts = if let Some(opts) = v {
      opts
        .iter()
        .filter_map(|str| UpstreamOption::try_from(str.as_str()).ok())
        .collect::<HashSet<UpstreamOption>>()
    } else {
      Default::default()
    };
    self.opts = Some(opts);
    self
  }
}

impl UpstreamGroup {
  /// Get an enabled option of load balancing [[LoadBalance]]
  pub fn get(&self, context_to_lb: &Option<LbContext>) -> (Option<&Upstream>, Option<LbContext>) {
    let pointer_to_upstream = self.lb.get_context(context_to_lb);
    debug!("Upstream of index {} is chosen.", pointer_to_upstream.ptr);
    debug!("Context to LB (Cookie in Req): {:?}", context_to_lb);
    debug!(
      "Context from LB (Set-Cookie in Res): {:?}",
      pointer_to_upstream.context_lb
    );
    (
      self.upstream.get(pointer_to_upstream.ptr),
      pointer_to_upstream.context_lb,
    )
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
