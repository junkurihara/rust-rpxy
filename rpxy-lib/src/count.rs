use std::{
  collections::HashMap,
  net::IpAddr,
  sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
  },
};

#[derive(Debug, Clone, Default)]
/// Counter for serving requests
pub struct RequestCount(Arc<AtomicUsize>);

impl RequestCount {
  pub fn current(&self) -> usize {
    self.0.load(Ordering::Relaxed)
  }

  pub fn increment(&self) -> usize {
    self.0.fetch_add(1, Ordering::Relaxed)
  }

  pub fn decrement(&self) -> usize {
    let mut count;
    while {
      count = self.0.load(Ordering::Relaxed);
      count > 0
        && self
          .0
          .compare_exchange(count, count - 1, Ordering::Relaxed, Ordering::Relaxed)
          != Ok(count)
    } {}
    count
  }
}

type IpConnectionMap = HashMap<IpAddr, usize, ahash::RandomState>;

/// Locks the map, recovering the guard if the mutex was poisoned. This counter is a best-effort
/// availability guard, so a panic elsewhere must not cascade into rejecting every connection.
fn lock_map(map: &Mutex<IpConnectionMap>) -> std::sync::MutexGuard<'_, IpConnectionMap> {
  map.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug, Clone)]
/// Per-source-IP concurrent connection counter, in addition to the global RequestCount.
/// `max_per_ip == 0` disables the limit, in which case the map is never touched.
pub struct PerIpConnectionCount {
  inner: Arc<Mutex<IpConnectionMap>>,
  max_per_ip: usize,
}

impl PerIpConnectionCount {
  pub fn new(max_per_ip: usize) -> Self {
    Self {
      inner: Arc::new(Mutex::new(IpConnectionMap::default())),
      max_per_ip,
    }
  }

  /// Reserves one connection slot for the given IP, returning a guard that releases the slot on drop.
  /// Returns None when the IP already holds max_per_ip slots. When the limit is disabled
  /// (max_per_ip == 0), always returns a no-op guard without touching the map.
  pub fn try_acquire(&self, ip: IpAddr) -> Option<PerIpConnectionGuard> {
    if self.max_per_ip == 0 {
      return Some(PerIpConnectionGuard { state: None });
    }
    let mut map = lock_map(&self.inner);
    let count = map.entry(ip).or_insert(0);
    // A freshly inserted entry is 0, which is below max_per_ip (>= 1 here), so the rejection
    // branch is only reached for an already-positive count and never leaves a stale zero entry.
    if *count >= self.max_per_ip {
      return None;
    }
    *count += 1;
    Some(PerIpConnectionGuard {
      state: Some((self.inner.clone(), ip)),
    })
  }
}

/// RAII guard releasing one per-IP connection slot on drop, removing the entry when it reaches zero.
pub struct PerIpConnectionGuard {
  state: Option<(Arc<Mutex<IpConnectionMap>>, IpAddr)>,
}

impl Drop for PerIpConnectionGuard {
  fn drop(&mut self) {
    let Some((map, ip)) = &self.state else {
      return;
    };
    let mut map = lock_map(map);
    if let Some(count) = map.get_mut(ip) {
      *count = count.saturating_sub(1);
      if *count == 0 {
        map.remove(ip);
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::net::Ipv4Addr;

  fn ip(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 0, 2, last))
  }

  impl PerIpConnectionCount {
    fn tracked_ips(&self) -> usize {
      self.inner.lock().unwrap().len()
    }
    fn current(&self, ip: IpAddr) -> usize {
      self.inner.lock().unwrap().get(&ip).copied().unwrap_or(0)
    }
  }

  #[test]
  fn disabled_never_touches_map() {
    let counter = PerIpConnectionCount::new(0);
    let g1 = counter.try_acquire(ip(1)).unwrap();
    let g2 = counter.try_acquire(ip(1)).unwrap();
    assert_eq!(counter.tracked_ips(), 0);
    drop((g1, g2));
    assert_eq!(counter.tracked_ips(), 0);
  }

  #[test]
  fn enforces_cap_per_ip() {
    let counter = PerIpConnectionCount::new(2);
    let _g1 = counter.try_acquire(ip(1)).unwrap();
    let _g2 = counter.try_acquire(ip(1)).unwrap();
    assert!(counter.try_acquire(ip(1)).is_none());
    assert_eq!(counter.current(ip(1)), 2);
  }

  #[test]
  fn distinct_ips_are_independent() {
    let counter = PerIpConnectionCount::new(1);
    let _g1 = counter.try_acquire(ip(1)).unwrap();
    assert!(counter.try_acquire(ip(1)).is_none());
    let _g2 = counter.try_acquire(ip(2)).unwrap();
    assert_eq!(counter.tracked_ips(), 2);
  }

  #[test]
  fn drop_releases_slot_and_removes_entry_at_zero() {
    let counter = PerIpConnectionCount::new(1);
    let g1 = counter.try_acquire(ip(1)).unwrap();
    assert!(counter.try_acquire(ip(1)).is_none());
    drop(g1);
    assert_eq!(counter.current(ip(1)), 0);
    assert_eq!(counter.tracked_ips(), 0);
    let _g2 = counter.try_acquire(ip(1)).unwrap();
    assert_eq!(counter.current(ip(1)), 1);
  }

  #[test]
  fn concurrent_acquire_release_never_exceeds_cap() {
    let counter = PerIpConnectionCount::new(4);
    let mut handles = Vec::new();
    for _ in 0..16 {
      let counter = counter.clone();
      handles.push(std::thread::spawn(move || {
        for _ in 0..1000 {
          for last in 0..3u8 {
            if let Some(guard) = counter.try_acquire(ip(last)) {
              assert!(counter.current(ip(last)) <= 4);
              drop(guard);
            }
          }
        }
      }));
    }
    for h in handles {
      h.join().unwrap();
    }
    assert_eq!(counter.tracked_ips(), 0);
  }
}
