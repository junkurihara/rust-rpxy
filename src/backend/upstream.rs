use super::{
  load_balance::{load_balance_options as lb_opts, LbRoundRobinCountBuilder, LoadBalance},
  BytesName, PathNameBytesExp, UpstreamOption,
};
use crate::log::*;
use derive_builder::Builder;
use rand::Rng;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
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

#[derive(Debug, Clone, Builder)]
/// Struct serving multiple upstream servers for, e.g., load balancing.
pub struct UpstreamGroup {
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
  pub fn lb(&mut self, v: &Option<String>, upstream_num: &usize) -> &mut Self {
    let lb = if let Some(x) = v {
      match x.as_str() {
        lb_opts::FIX_TO_FIRST => LoadBalance::FixToFirst,
        lb_opts::RANDOM => LoadBalance::Random,
        lb_opts::ROUND_ROBIN => LoadBalance::RoundRobin(
          LbRoundRobinCountBuilder::default()
            .max_val(upstream_num)
            .build()
            .unwrap(),
        ),
        lb_opts::STICKY_ROUND_ROBIN => LoadBalance::StickyRoundRobin(
          LbRoundRobinCountBuilder::default()
            .max_val(upstream_num)
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
  pub fn get(&self) -> Option<&Upstream> {
    match &self.lb {
      LoadBalance::FixToFirst => self.upstream.get(0),
      LoadBalance::RoundRobin(cnt) => {
        let idx = cnt.increment_cnt();
        self.upstream.get(idx)
      }
      LoadBalance::Random => {
        let mut rng = rand::thread_rng();
        let max = self.upstream.len() - 1;
        self.upstream.get(rng.gen_range(0..max))
      }
      LoadBalance::StickyRoundRobin(_cnt) => todo!(), // TODO: TODO:
    }
  }
}
