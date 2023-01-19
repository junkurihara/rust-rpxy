use super::{BytesName, PathNameBytesExp, UpstreamOption};
use crate::log::*;
use derive_builder::Builder;
use rand::Rng;
use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::{
  borrow::Cow,
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  },
};

#[derive(Debug, Clone)]
pub struct ReverseProxy {
  pub upstream: HashMap<PathNameBytesExp, UpstreamGroup>, // TODO: HashMapでいいのかは疑問。max_by_keyでlongest prefix matchしてるのも無駄っぽいが。。。
}

impl ReverseProxy {
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

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum LoadBalance {
  RoundRobin,
  Random,
}
impl Default for LoadBalance {
  fn default() -> Self {
    Self::RoundRobin
  }
}

#[derive(Debug, Clone)]
pub struct Upstream {
  pub uri: hyper::Uri, // base uri without specific path
}

#[derive(Debug, Clone, Builder)]
pub struct UpstreamGroup {
  pub upstream: Vec<Upstream>,
  #[builder(setter(custom), default)]
  pub path: PathNameBytesExp,
  #[builder(setter(custom), default)]
  pub replace_path: Option<PathNameBytesExp>,
  #[builder(default)]
  pub lb: LoadBalance,
  #[builder(default)]
  pub cnt: UpstreamCount, // counter for load balancing
  #[builder(setter(custom), default)]
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

#[derive(Debug, Clone, Default)]
pub struct UpstreamCount(Arc<AtomicUsize>);

impl UpstreamGroup {
  pub fn get(&self) -> Option<&Upstream> {
    match self.lb {
      LoadBalance::RoundRobin => {
        let idx = self.increment_cnt();
        self.upstream.get(idx)
      }
      LoadBalance::Random => {
        let mut rng = rand::thread_rng();
        let max = self.upstream.len() - 1;
        self.upstream.get(rng.gen_range(0..max))
      }
    }
  }

  fn current_cnt(&self) -> usize {
    self.cnt.0.load(Ordering::Relaxed)
  }

  fn increment_cnt(&self) -> usize {
    if self.current_cnt() < self.upstream.len() - 1 {
      self.cnt.0.fetch_add(1, Ordering::Relaxed)
    } else {
      self.cnt.0.fetch_and(0, Ordering::Relaxed)
    }
  }
}
