use super::{
  Upstream,
  load_balance_main::{LoadBalanceContext, LoadBalanceWithPointer, PointerToUpstream, pick_nth_available_index},
  sticky_cookie::StickyCookieConfig,
};
use crate::{constants::STICKY_COOKIE_NAME, log::*};
use ahash::HashMap;
use derive_builder::Builder;
use std::{
  borrow::Cow,
  sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
  },
};

#[derive(Debug, Clone, Builder)]
/// Round Robin LB object in the sticky cookie manner
pub struct LoadBalanceSticky {
  #[builder(default)]
  /// Pointer to the index of the last served upstream destination
  ptr: Arc<AtomicUsize>,
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
impl LoadBalanceStickyBuilder {
  /// Set the information to build the cookie to stick clients to specific backends
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
  /// Set the hashmaps: upstream_index_map and upstream_id_map
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
impl<'a> LoadBalanceSticky {
  /// Atomically increment ptr, reset near overflow.
  fn fetch_and_advance(&self) -> usize {
    let prev = self.ptr.fetch_add(1, Ordering::Relaxed);
    if prev >= usize::MAX - 1 {
      self.ptr.store(0, Ordering::Relaxed);
    }
    prev
  }

  /// Round-robin among healthy upstreams, falling back to all upstreams if all are unhealthy.
  fn rr_next_index(&self, upstreams: &[Upstream]) -> usize {
    let count = self.fetch_and_advance();
    pick_nth_available_index(upstreams, count)
  }

  #[cfg(test)]
  fn get_server_id_from_index(&self, index: usize) -> String {
    self.upstream_maps.upstream_index_map.get(index).unwrap().to_owned()
  }
  /// This function takes value passed from outside. So 'result' is used.
  fn get_server_index_from_id(&self, id: impl Into<Cow<'a, str>>) -> Option<usize> {
    let id_str = id.into().to_string();
    self.upstream_maps.upstream_id_map.get(&id_str).map(|v| v.to_owned())
  }

  /// Build a PointerToUpstream with a new cookie context for the given index
  fn build_ptr_with_new_cookie(&self, ptr: usize) -> PointerToUpstream {
    PointerToUpstream {
      ptr,
      context: self.build_lb_context_for_index(ptr),
    }
  }

  /// Build a fresh sticky-cookie context bound to the given upstream index. Returns
  /// `None` if the index is out of range or the cookie can't be built. Used internally
  /// for fresh LB picks and externally by the failover path so Set-Cookie tracks the
  /// upstream that actually served the response (not the one originally pinned).
  pub(crate) fn build_lb_context_for_index(&self, idx: usize) -> Option<LoadBalanceContext> {
    let upstream_id = self.upstream_maps.upstream_index_map.get(idx)?.to_owned();
    let cookie = self.sticky_config.build_sticky_cookie(upstream_id).ok()?;
    Some(LoadBalanceContext { sticky_cookie: cookie })
  }
}
impl LoadBalanceWithPointer for LoadBalanceSticky {
  /// Get the pointer to the upstream server to serve the incoming request.
  fn get_ptr(&self, req_info: Option<&LoadBalanceContext>, upstreams: &[Upstream]) -> PointerToUpstream {
    match req_info {
      None => {
        debug!("No sticky cookie");
        let ptr = self.rr_next_index(upstreams);
        self.build_ptr_with_new_cookie(ptr)
      }
      Some(context) => {
        let server_id = &context.sticky_cookie.value.value;
        match self.get_server_index_from_id(server_id) {
          Some(index) if upstreams.get(index).is_some_and(|u| u.is_healthy()) => {
            // Valid cookie, target is healthy -> use it, NO re-issue
            debug!("Valid sticky cookie: id={server_id}, index={index}, healthy",);
            PointerToUpstream {
              ptr: index,
              context: None,
            }
          }
          Some(index) => {
            // Valid cookie but target is unhealthy -> fallback + new cookie
            debug!("Valid sticky cookie: id={server_id}, index={index}, unhealthy -> fallback",);
            let ptr = self.rr_next_index(upstreams);
            self.build_ptr_with_new_cookie(ptr)
          }
          None => {
            // Invalid cookie -> RR + new cookie
            debug!("Invalid sticky cookie: id={}", server_id);
            let ptr = self.rr_next_index(upstreams);
            self.build_ptr_with_new_cookie(ptr)
          }
        }
      }
    }
  }
}

/* --------------------------------------------------------------------- */
#[cfg(test)]
mod tests {
  use super::*;
  use crate::backend::load_balance::sticky_cookie::{StickyCookie, StickyCookieValue};

  fn make_upstream(uri_str: &str) -> Upstream {
    Upstream {
      uri: uri_str.parse::<hyper::Uri>().unwrap(),
      #[cfg(feature = "health-check")]
      health: None,
    }
  }

  #[cfg(feature = "health-check")]
  fn make_upstream_with_health(uri_str: &str, healthy: bool) -> Upstream {
    let health = Arc::new(crate::backend::health_check::UpstreamHealth::new());
    health.set(healthy);
    Upstream {
      uri: uri_str.parse::<hyper::Uri>().unwrap(),
      health: Some(health),
    }
  }

  fn build_sticky_lb(upstreams: &[Upstream]) -> LoadBalanceSticky {
    LoadBalanceStickyBuilder::default()
      .sticky_config("example.com", &Some("/".to_string()))
      .upstream_maps(upstreams)
      .build()
      .unwrap()
  }

  fn make_context_for(lb: &LoadBalanceSticky, index: usize) -> LoadBalanceContext {
    let server_id = lb.get_server_id_from_index(index);
    LoadBalanceContext {
      sticky_cookie: StickyCookie {
        value: StickyCookieValue {
          name: STICKY_COOKIE_NAME.to_string(),
          value: server_id,
        },
        info: None,
      },
    }
  }

  fn make_invalid_context() -> LoadBalanceContext {
    LoadBalanceContext {
      sticky_cookie: StickyCookie {
        value: StickyCookieValue {
          name: STICKY_COOKIE_NAME.to_string(),
          value: "invalid_server_id_garbage".to_string(),
        },
        info: None,
      },
    }
  }

  #[test]
  fn no_cookie_returns_new_cookie() {
    let upstreams = vec![make_upstream("http://a:8080"), make_upstream("http://b:8080")];
    let lb = build_sticky_lb(&upstreams);

    let result = lb.get_ptr(None, &upstreams);
    // Should issue a new cookie (context is Some)
    assert!(result.context.is_some());
    assert!(result.ptr < upstreams.len());
  }

  #[test]
  fn valid_cookie_healthy_target_no_reissue() {
    let upstreams = vec![make_upstream("http://a:8080"), make_upstream("http://b:8080")];
    let lb = build_sticky_lb(&upstreams);
    let ctx = make_context_for(&lb, 1);

    let result = lb.get_ptr(Some(&ctx), &upstreams);
    // Should stick to index 1, NO cookie re-issue
    assert_eq!(result.ptr, 1);
    assert!(result.context.is_none());
  }

  #[test]
  fn invalid_cookie_falls_back_with_new_cookie() {
    let upstreams = vec![make_upstream("http://a:8080"), make_upstream("http://b:8080")];
    let lb = build_sticky_lb(&upstreams);
    let ctx = make_invalid_context();

    let result = lb.get_ptr(Some(&ctx), &upstreams);
    // Should fallback to RR and issue a new cookie
    assert!(result.context.is_some());
    assert!(result.ptr < upstreams.len());
  }

  #[test]
  fn no_cookie_round_robins() {
    let upstreams = vec![
      make_upstream("http://a:8080"),
      make_upstream("http://b:8080"),
      make_upstream("http://c:8080"),
    ];
    let lb = build_sticky_lb(&upstreams);

    let mut seen = std::collections::HashSet::new();
    for _ in 0..6 {
      let result = lb.get_ptr(None, &upstreams);
      seen.insert(result.ptr);
    }
    // After 6 calls across 3 upstreams, all should be visited
    assert_eq!(seen.len(), 3);
  }

  #[cfg(feature = "health-check")]
  #[test]
  fn valid_cookie_unhealthy_target_fallback_with_new_cookie() {
    let upstreams = vec![
      make_upstream_with_health("http://a:8080", true),
      make_upstream_with_health("http://b:8080", false), // target is down
      make_upstream_with_health("http://c:8080", true),
    ];
    let lb = build_sticky_lb(&upstreams);
    let ctx = make_context_for(&lb, 1); // cookie points to index 1 (unhealthy)

    let result = lb.get_ptr(Some(&ctx), &upstreams);
    // Should NOT stick to index 1, should fallback and issue new cookie
    assert_ne!(result.ptr, 1);
    assert!(result.context.is_some());
    // Should pick from healthy: index 0 or 2
    assert!(result.ptr == 0 || result.ptr == 2);
  }

  #[cfg(feature = "health-check")]
  #[test]
  fn no_cookie_skips_unhealthy() {
    let upstreams = vec![
      make_upstream_with_health("http://a:8080", false),
      make_upstream_with_health("http://b:8080", true),
      make_upstream_with_health("http://c:8080", false),
    ];
    let lb = build_sticky_lb(&upstreams);

    // All calls should go to index 1 (only healthy)
    for _ in 0..5 {
      let result = lb.get_ptr(None, &upstreams);
      assert_eq!(result.ptr, 1);
      assert!(result.context.is_some());
    }
  }

  #[cfg(feature = "health-check")]
  #[test]
  fn all_unhealthy_best_effort() {
    let upstreams = vec![
      make_upstream_with_health("http://a:8080", false),
      make_upstream_with_health("http://b:8080", false),
    ];
    let lb = build_sticky_lb(&upstreams);

    let result = lb.get_ptr(None, &upstreams);
    // Should still pick one (best-effort)
    assert!(result.ptr < upstreams.len());
    assert!(result.context.is_some());
  }
}
