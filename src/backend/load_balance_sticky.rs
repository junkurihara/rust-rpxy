use super::{
  load_balance::{LbContext, LbWithPointer, PointerToUpstream},
  sticky_cookie::StickyCookieConfig,
  Upstream,
};
use crate::{constants::STICKY_COOKIE_NAME, log::*};
use derive_builder::Builder;
use rustc_hash::FxHashMap as HashMap;
use std::{
  borrow::Cow,
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
  },
};

#[derive(Debug, Clone, Builder)]
/// Round Robin LB object in the sticky cookie manner
pub struct LbStickyRoundRobin {
  #[builder(default)]
  /// Pointer to the index of the last served upstream destination
  ptr: Arc<AtomicUsize>,
  #[builder(setter(custom), default)]
  /// Number of upstream destinations
  num_upstreams: usize,
  #[builder(setter(custom))]
  /// Information to build the cookie to stick clients to specific backends
  pub sticky_config: StickyCookieConfig,
  #[builder(setter(custom))]
  /// Hashmaps:
  /// - Hashmap that maps server indices to server id (string)
  /// - Hashmap that maps server ids (string) to server indices, for fast reverse lookup
  upstream_maps: UpstreamMap,
}
#[derive(Debug, Clone)]
pub struct UpstreamMap {
  /// Hashmap that maps server indices to server id (string)
  upstream_index_map: Vec<String>,
  /// Hashmap that maps server ids (string) to server indices, for fast reverse lookup
  upstream_id_map: HashMap<String, usize>,
}
impl LbStickyRoundRobinBuilder {
  pub fn num_upstreams(&mut self, v: &usize) -> &mut Self {
    self.num_upstreams = Some(*v);
    self
  }
  pub fn sticky_config(&mut self, server_name: &str, path_opt: &Option<String>) -> &mut Self {
    self.sticky_config = Some(StickyCookieConfig {
      name: STICKY_COOKIE_NAME.to_string(), // TODO: config等で変更できるように
      domain: server_name.to_ascii_lowercase(),
      path: if let Some(v) = path_opt {
        v.to_ascii_lowercase()
      } else {
        "/".to_string()
      },
      duration: 300, // TODO: config等で変更できるように
    });
    self
  }
  pub fn upstream_maps(&mut self, upstream_vec: &[Upstream]) -> &mut Self {
    let upstream_index_map: Vec<String> = upstream_vec
      .iter()
      .enumerate()
      .map(|(i, v)| v.calculate_id_with_index(i))
      .collect();
    let mut upstream_id_map = HashMap::default();
    for (i, v) in upstream_index_map.iter().enumerate() {
      upstream_id_map.insert(v.to_string(), i);
    }
    self.upstream_maps = Some(UpstreamMap {
      upstream_index_map,
      upstream_id_map,
    });
    self
  }
}
impl<'a> LbStickyRoundRobin {
  fn simple_increment_ptr(&self) -> usize {
    // Get a current count of upstream served
    let current_ptr = self.ptr.load(Ordering::Relaxed);

    if current_ptr < self.num_upstreams - 1 {
      self.ptr.fetch_add(1, Ordering::Relaxed)
    } else {
      // Clear the counter
      self.ptr.fetch_and(0, Ordering::Relaxed)
    }
  }
  /// This is always called only internally. So 'unwrap()' is executed.
  fn get_server_id_from_index(&self, index: usize) -> String {
    self.upstream_maps.upstream_index_map.get(index).unwrap().to_owned()
  }
  /// This function takes value passed from outside. So 'result' is used.
  fn get_server_index_from_id(&self, id: impl Into<Cow<'a, str>>) -> Option<usize> {
    let id_str = id.into().to_string();
    self.upstream_maps.upstream_id_map.get(&id_str).map(|v| v.to_owned())
  }
}
impl LbWithPointer for LbStickyRoundRobin {
  fn get_ptr(&self, req_info: Option<&LbContext>) -> PointerToUpstream {
    // If given context is None or invalid (not contained), get_ptr() is invoked to increment the pointer.
    // Otherwise, get the server index indicated by the server_id inside the cookie
    let ptr = match req_info {
      None => {
        debug!("No sticky cookie");
        self.simple_increment_ptr()
      }
      Some(context) => {
        let server_id = &context.sticky_cookie.value.value;
        if let Some(server_index) = self.get_server_index_from_id(server_id) {
          debug!("Valid sticky cookie: id={}, index={}", server_id, server_index);
          server_index
        } else {
          debug!("Invalid sticky cookie: id={}", server_id);
          self.simple_increment_ptr()
        }
      }
    };

    // Get the server id from the ptr.
    // TODO: This should be simplified and optimized if ptr is not changed (id value exists in cookie).
    let upstream_id = self.get_server_id_from_index(ptr);
    let new_cookie = self.sticky_config.build_sticky_cookie(upstream_id).unwrap();
    let new_context = Some(LbContext {
      sticky_cookie: new_cookie,
    });
    PointerToUpstream {
      ptr,
      context_lb: new_context,
    }
  }
}
