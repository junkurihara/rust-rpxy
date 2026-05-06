use ahash::HashSet;
use std::sync::Arc;

/// Default HTTP status codes that trigger failover when no list is configured.
const DEFAULT_TRIGGER_STATUSES: &[u16] = &[502, 503, 504];

/// Configuration for failover behavior when upstreams return errors
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailoverConfig {
  /// HTTP status codes that trigger failover (e.g., 502, 503, 504). Wrapped in `Arc` so
  /// the failover path can attach it to per-attempt request extensions without cloning
  /// the underlying set on every request.
  pub trigger_statuses: Arc<HashSet<u16>>,
  /// Whether to failover on connection failures (timeout, refused, etc.)
  pub on_connection_failure: bool,
  /// Maximum number of retry attempts (default: number of upstreams - 1)
  pub max_retries: usize,
  /// Opt-in: retry non-idempotent methods (POST, PATCH). Default `false`. RFC 9110 §9.2.2 only
  /// guarantees idempotency for GET/HEAD/PUT/DELETE/OPTIONS/TRACE; retrying others risks
  /// double-write side effects when an upstream processes the request then fails the response.
  pub retry_non_idempotent: bool,
}

impl FailoverConfig {
  /// Create a new FailoverConfig with custom settings.
  /// `max_retries` defaults to `num_upstreams - 1` when not specified.
  pub fn new(
    trigger_statuses: Option<Vec<u16>>,
    on_connection_failure: Option<bool>,
    max_retries: Option<usize>,
    retry_non_idempotent: Option<bool>,
    num_upstreams: usize,
  ) -> Self {
    let statuses: HashSet<u16> = trigger_statuses
      .map(|v| v.into_iter().collect())
      .unwrap_or_else(|| DEFAULT_TRIGGER_STATUSES.iter().copied().collect());
    Self {
      trigger_statuses: Arc::new(statuses),
      on_connection_failure: on_connection_failure.unwrap_or(true),
      max_retries: max_retries.unwrap_or_else(|| num_upstreams.saturating_sub(1)),
      retry_non_idempotent: retry_non_idempotent.unwrap_or(false),
    }
  }

  /// Validate that status codes are in the 4xx/5xx range
  pub fn validate(&self) -> Result<(), String> {
    for &status in self.trigger_statuses.iter() {
      if !(400..600).contains(&status) {
        return Err(format!("Failover status code {status} must be in range 400-599"));
      }
    }
    Ok(())
  }
}

/// Context tracking state during failover retries
#[derive(Debug, Clone)]
pub struct FailoverContext {
  /// Set of upstream indices that have been tried
  tried_upstreams: HashSet<usize>,
  /// Current retry count
  pub retry_count: usize,
  /// Index of the initial upstream selected by load balancer
  pub initial_upstream_idx: usize,
}

impl FailoverContext {
  /// Create a new failover context starting from the given upstream index
  pub fn new(initial_upstream_idx: usize) -> Self {
    Self {
      tried_upstreams: HashSet::default(),
      retry_count: 0,
      initial_upstream_idx,
    }
  }

  /// Check if an upstream has already been tried
  pub fn has_tried(&self, upstream_idx: usize) -> bool {
    self.tried_upstreams.contains(&upstream_idx)
  }

  /// Mark an upstream as tried
  pub fn mark_tried(&mut self, upstream_idx: usize) {
    self.tried_upstreams.insert(upstream_idx);
  }

  /// Check if we can retry based on max_retries limit
  pub fn can_retry(&self, max_retries: usize) -> bool {
    self.retry_count < max_retries
  }

  /// Increment retry counter
  pub fn increment_retry(&mut self) {
    self.retry_count += 1;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_failover_config_defaults() {
    let config = FailoverConfig::new(None, None, None, None, 3);
    assert_eq!(config.trigger_statuses.len(), 3);
    assert!(config.trigger_statuses.contains(&502));
    assert!(config.trigger_statuses.contains(&503));
    assert!(config.trigger_statuses.contains(&504));
    assert!(config.on_connection_failure);
    assert_eq!(config.max_retries, 2);
    assert!(!config.retry_non_idempotent);
  }

  #[test]
  fn test_failover_config_new_overrides() {
    let config = FailoverConfig::new(Some(vec![404, 502]), Some(false), Some(2), Some(true), 3);
    assert_eq!(config.trigger_statuses.len(), 2);
    assert!(config.trigger_statuses.contains(&404));
    assert!(config.trigger_statuses.contains(&502));
    assert!(!config.on_connection_failure);
    assert_eq!(config.max_retries, 2);
    assert!(config.retry_non_idempotent);
  }

  #[test]
  fn test_failover_config_default_max_retries_from_upstream_count() {
    let config = FailoverConfig::new(None, None, None, None, 3);
    assert_eq!(config.max_retries, 2);

    let config_zero = FailoverConfig::new(None, None, None, None, 0);
    assert_eq!(config_zero.max_retries, 0);
  }

  #[test]
  fn test_failover_config_validate() {
    let valid = FailoverConfig::new(Some(vec![404, 502, 503]), None, None, None, 2);
    assert!(valid.validate().is_ok());

    let invalid_low = FailoverConfig::new(Some(vec![200, 502]), None, None, None, 2);
    assert!(invalid_low.validate().is_err());

    let invalid_high = FailoverConfig::new(Some(vec![600]), None, None, None, 2);
    assert!(invalid_high.validate().is_err());
  }

  #[test]
  fn test_failover_context_tracking() {
    let mut ctx = FailoverContext::new(0);
    assert_eq!(ctx.initial_upstream_idx, 0);
    assert_eq!(ctx.retry_count, 0);
    // Initial upstream is NOT pre-marked to avoid skipping it
    assert!(!ctx.has_tried(0));
    assert!(!ctx.has_tried(1));

    ctx.mark_tried(0);
    assert!(ctx.has_tried(0));
    assert!(!ctx.has_tried(1));

    ctx.mark_tried(1);
    assert!(ctx.has_tried(1));
    assert!(!ctx.has_tried(2));

    ctx.increment_retry();
    assert_eq!(ctx.retry_count, 1);
  }

  #[test]
  fn test_failover_context_can_retry() {
    let mut ctx = FailoverContext::new(0);
    assert!(ctx.can_retry(2));

    ctx.increment_retry();
    assert!(ctx.can_retry(2));

    ctx.increment_retry();
    assert!(!ctx.can_retry(2));
  }
}
