/// Tracks consecutive success/failure counts to determine health state transitions.
/// Only triggers a state change when a threshold is crossed.
pub(super) struct ConsecutiveCounter {
  consecutive_ok: u32,
  consecutive_fail: u32,
  unhealthy_threshold: u32,
  healthy_threshold: u32,
  /// Current health state as tracked by this counter
  is_healthy: bool,
}

impl ConsecutiveCounter {
  pub fn new(unhealthy_threshold: u32, healthy_threshold: u32) -> Self {
    Self {
      consecutive_ok: 0,
      consecutive_fail: 0,
      unhealthy_threshold,
      healthy_threshold,
      is_healthy: true, // optimistic boot
    }
  }

  /// Record a check result. Returns `Some(new_state)` if a state transition occurred.
  pub fn record(&mut self, ok: bool) -> Option<bool> {
    if ok {
      self.consecutive_ok = self.consecutive_ok.saturating_add(1);
      self.consecutive_fail = 0;

      if !self.is_healthy && self.consecutive_ok >= self.healthy_threshold {
        self.is_healthy = true;
        self.consecutive_ok = 0;
        return Some(true);
      }
    } else {
      self.consecutive_fail = self.consecutive_fail.saturating_add(1);
      self.consecutive_ok = 0;

      if self.is_healthy && self.consecutive_fail >= self.unhealthy_threshold {
        self.is_healthy = false;
        self.consecutive_fail = 0;
        return Some(false);
      }
    }
    None
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn becomes_unhealthy_after_threshold() {
    let mut c = ConsecutiveCounter::new(3, 2);
    assert!(c.record(false).is_none()); // 1 fail
    assert!(c.record(false).is_none()); // 2 fails
    assert_eq!(c.record(false), Some(false)); // 3 fails -> unhealthy
  }

  #[test]
  fn recovers_after_healthy_threshold() {
    let mut c = ConsecutiveCounter::new(3, 2);
    // First become unhealthy
    c.record(false);
    c.record(false);
    c.record(false);
    // Now recover
    assert!(c.record(true).is_none()); // 1 ok
    assert_eq!(c.record(true), Some(true)); // 2 ok -> healthy
  }

  #[test]
  fn intermittent_failures_reset_counter() {
    let mut c = ConsecutiveCounter::new(3, 2);
    c.record(false); // 1 fail
    c.record(false); // 2 fails
    c.record(true); // reset fail counter
    c.record(false); // 1 fail (restarted)
    c.record(false); // 2 fails
    // Still healthy — never reached 3 consecutive
    assert!(c.record(true).is_none());
  }

  #[test]
  fn no_spurious_transitions_when_already_healthy() {
    let mut c = ConsecutiveCounter::new(3, 2);
    // Already healthy, consecutive successes should not trigger transition
    assert!(c.record(true).is_none());
    assert!(c.record(true).is_none());
    assert!(c.record(true).is_none());
  }

  #[test]
  fn no_spurious_transitions_when_already_unhealthy() {
    let mut c = ConsecutiveCounter::new(1, 2);
    assert_eq!(c.record(false), Some(false)); // -> unhealthy
    // Further failures should not re-trigger
    assert!(c.record(false).is_none());
    assert!(c.record(false).is_none());
  }
}
