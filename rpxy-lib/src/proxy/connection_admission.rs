use std::sync::{
  Arc,
  atomic::{AtomicUsize, Ordering},
};

/// Shared admission state for concurrent HTTP/1.1 and HTTP/2 connections.
#[derive(Clone)]
pub struct ConnectionAdmission {
  inner: Arc<ConnectionAdmissionInner>,
}

struct ConnectionAdmissionInner {
  active: AtomicUsize,
  limit: usize,
}

/// Releases one connection slot when the admitted connection leaves scope.
#[must_use = "dropping the permit immediately releases the admitted connection slot"]
pub struct ConnectionPermit {
  inner: Arc<ConnectionAdmissionInner>,
}

impl ConnectionAdmission {
  pub fn new(limit: usize) -> Self {
    Self {
      inner: Arc::new(ConnectionAdmissionInner {
        active: AtomicUsize::new(0),
        limit,
      }),
    }
  }

  /// Attempts to reserve one slot without blocking.
  pub fn try_acquire(&self) -> Option<ConnectionPermit> {
    // This atomic protects only the numerical admission invariant. It does not
    // publish or synchronize connection data, so no acquire/release ordering is
    // required between otherwise independent connection tasks.
    self
      .inner
      .active
      .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
        if active < self.inner.limit { Some(active + 1) } else { None }
      })
      .ok()?;

    Some(ConnectionPermit {
      inner: Arc::clone(&self.inner),
    })
  }

  #[cfg(test)]
  fn current(&self) -> usize {
    self.inner.active.load(Ordering::Relaxed)
  }
}

impl Drop for ConnectionPermit {
  fn drop(&mut self) {
    let previous = self.inner.active.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(previous > 0, "connection admission count underflow");
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn admits_exactly_the_limit_and_reuses_released_slots() {
    let admission = ConnectionAdmission::new(2);
    let first = admission.try_acquire().unwrap();
    let second = admission.try_acquire().unwrap();

    assert!(admission.try_acquire().is_none());
    assert_eq!(admission.current(), 2);

    drop(first);
    let replacement = admission.try_acquire().unwrap();
    assert_eq!(admission.current(), 2);

    drop(second);
    drop(replacement);
    assert_eq!(admission.current(), 0);
  }

  #[test]
  fn zero_limit_rejects_all_connections() {
    let admission = ConnectionAdmission::new(0);

    assert!(admission.try_acquire().is_none());
    assert_eq!(admission.current(), 0);
  }

  #[test]
  fn listener_clones_share_the_same_limit() {
    let admission = ConnectionAdmission::new(1);
    let clone = admission.clone();
    let permit = clone.try_acquire().unwrap();

    assert!(admission.try_acquire().is_none());
    drop(permit);
    assert!(admission.try_acquire().is_some());
  }

  #[test]
  fn separate_generations_have_independent_capacity() {
    let old_generation = ConnectionAdmission::new(1);
    let new_generation = ConnectionAdmission::new(1);

    let _old_permit = old_generation.try_acquire().unwrap();
    let _new_permit = new_generation.try_acquire().unwrap();

    assert!(old_generation.try_acquire().is_none());
    assert!(new_generation.try_acquire().is_none());
  }

  #[test]
  fn saturated_usize_max_does_not_overflow() {
    // Dev and test profiles enable overflow checks. This would panic if the update
    // closure eagerly evaluated `usize::MAX + 1` on the rejected path.
    let admission = ConnectionAdmission {
      inner: Arc::new(ConnectionAdmissionInner {
        active: AtomicUsize::new(usize::MAX),
        limit: usize::MAX,
      }),
    };

    assert!(admission.try_acquire().is_none());
    assert_eq!(admission.current(), usize::MAX);
  }

  #[test]
  fn usize_max_limit_admits_the_final_slot() {
    let admission = ConnectionAdmission {
      inner: Arc::new(ConnectionAdmissionInner {
        active: AtomicUsize::new(usize::MAX - 1),
        limit: usize::MAX,
      }),
    };

    let permit = admission.try_acquire().unwrap();
    assert_eq!(admission.current(), usize::MAX);
    assert!(admission.try_acquire().is_none());

    drop(permit);
    assert_eq!(admission.current(), usize::MAX - 1);
  }

  #[test]
  fn rejection_does_not_retain_an_extra_arc() {
    let admission = ConnectionAdmission::new(1);
    let _permit = admission.try_acquire().unwrap();
    let strong_count = Arc::strong_count(&admission.inner);

    assert!(admission.try_acquire().is_none());
    assert_eq!(Arc::strong_count(&admission.inner), strong_count);
  }

  #[test]
  fn early_return_releases_the_slot() {
    fn acquire_then_return(admission: &ConnectionAdmission) {
      let _permit = admission.try_acquire().unwrap();
    }

    let admission = ConnectionAdmission::new(1);
    acquire_then_return(&admission);

    assert_eq!(admission.current(), 0);
    assert!(admission.try_acquire().is_some());
  }

  #[test]
  fn concurrent_churn_never_leaks_slots() {
    const LIMIT: usize = 8;
    const ITERATIONS: usize = 2_000;

    let admission = ConnectionAdmission::new(LIMIT);
    let in_flight = Arc::new(AtomicUsize::new(0));
    let high_watermark = Arc::new(AtomicUsize::new(0));
    let handles = (0..(LIMIT * 2))
      .map(|_| {
        let admission = admission.clone();
        let in_flight = Arc::clone(&in_flight);
        let high_watermark = Arc::clone(&high_watermark);
        std::thread::spawn(move || {
          for _ in 0..ITERATIONS {
            loop {
              if let Some(permit) = admission.try_acquire() {
                let current = in_flight.fetch_add(1, Ordering::Relaxed) + 1;
                high_watermark.fetch_max(current, Ordering::Relaxed);
                std::thread::yield_now();
                in_flight.fetch_sub(1, Ordering::Relaxed);
                drop(permit);
                break;
              }
              std::thread::yield_now();
            }
          }
        })
      })
      .collect::<Vec<_>>();

    for handle in handles {
      handle.join().unwrap();
    }

    assert_eq!(admission.current(), 0);
    assert_eq!(in_flight.load(Ordering::Relaxed), 0);
    assert!(high_watermark.load(Ordering::Relaxed) <= LIMIT);
  }

  #[tokio::test]
  async fn aborting_a_task_releases_its_slot() {
    let admission = ConnectionAdmission::new(1);
    let permit = admission.try_acquire().unwrap();
    let task = tokio::spawn(async move {
      let _permit = permit;
      std::future::pending::<()>().await;
    });

    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(admission.current(), 0);
  }
}
