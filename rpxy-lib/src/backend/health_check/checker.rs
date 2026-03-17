use super::{UpstreamHealth, check_http::HealthCheckHttpClient, check_tcp::check_tcp, counter::ConsecutiveCounter};
use crate::{
  backend::BackendAppManager,
  error::RpxyResult,
  globals::{HealthCheckConfig, HealthCheckType},
  log::*,
};
use futures::future::join_all;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Spawn health checker tasks for all upstream candidates that have health check enabled.
/// Returns join handles for the spawned tasks.
pub(crate) fn spawn_health_checkers(
  app_manager: &Arc<BackendAppManager>,
  cancel_token: CancellationToken,
  runtime_handle: &tokio::runtime::Handle,
) -> Vec<tokio::task::JoinHandle<RpxyResult<()>>> {
  // Build a shared HTTP client for all HTTP health checks.
  // Shared via Arc to avoid creating one client per checker task.
  let http_client = match HealthCheckHttpClient::try_new(runtime_handle) {
    Ok(c) => Some(Arc::new(c)),
    Err(e) => {
      error!(
        "Failed to build health check HTTP client: {}. HTTP health checks will fall back to TCP.",
        e
      );
      None
    }
  };

  let mut handles = Vec::new();

  app_manager.apps.iter().for_each(|(_app_name, backend_app)| {
    let sub_handles = backend_app.path_manager.inner.iter().filter_map(|(path, candidates)| {
      // Collect upstreams that have health check enabled (i.e., have UpstreamHealth)
      let health_upstreams: Vec<_> = candidates
        .inner
        .iter()
        .filter_map(|upstream| upstream.health.as_ref().map(|h| (upstream.uri.clone(), Arc::clone(h))))
        .collect();

      if health_upstreams.is_empty() {
        return None;
      }

      let Some(ref config) = candidates.health_check_config else {
        return None;
      };

      let path_str: String = path.try_into().unwrap_or_else(|_| "<none>".to_string());
      let num_upstreams = health_upstreams.len();

      info!(
        "Health checker started for path \"{path_str}\" ({num_upstreams} upstreams, {:?}, interval={:?})",
        config.check_type, config.interval
      );

      let config = config.clone();
      let cancel = cancel_token.clone();
      let task_http_client = match config.check_type {
        HealthCheckType::Http { .. } => http_client.clone(),
        _ => None,
      };
      let handle =
        runtime_handle.spawn(async move { run_health_checker(health_upstreams, config, cancel, task_http_client).await });

      Some(handle)
    });
    handles.extend(sub_handles);
  });

  handles
}

/// Run a single health checker task for a group of upstreams.
/// Checks all upstreams concurrently with join_all, then sleeps for interval.
async fn run_health_checker(
  upstreams: Vec<(hyper::Uri, Arc<UpstreamHealth>)>,
  config: HealthCheckConfig,
  cancel: CancellationToken,
  http_client: Option<Arc<HealthCheckHttpClient>>,
) -> RpxyResult<()> {
  let mut counters: Vec<ConsecutiveCounter> = upstreams
    .iter()
    .map(|_| ConsecutiveCounter::new(config.unhealthy_threshold, config.healthy_threshold))
    .collect();

  loop {
    tokio::select! {
      _ = cancel.cancelled() => {
        debug!("Health checker terminated");
        return Ok(());
      }
      _ = tokio::time::sleep(config.interval) => {
        let checks = upstreams.iter().enumerate().map(|(i, (uri, _health))| {
          let uri = uri.clone();
          let timeout = config.timeout;
          let check_type = config.check_type.clone();
          let http_client = http_client.clone();
          async move {
            let ok = match check_type {
              HealthCheckType::Tcp => check_tcp(&uri, timeout).await,
              HealthCheckType::Http { ref path, expected_status } => {
                if let Some(ref client) = http_client {
                  client.check(&uri, path, expected_status, timeout).await
                } else {
                  // Fallback to TCP if HTTP client failed to build
                  warn!("HTTP health check client unavailable, falling back to TCP for {}", uri);
                  check_tcp(&uri, timeout).await
                }
              }
            };
            (i, ok)
          }
        });

        let results = join_all(checks).await;

        results.into_iter().for_each(|(i, ok)| {
          let (ref uri, ref health) = upstreams[i];
          if !ok {
            debug!("Health check failed for {}", uri);
          }
          if let Some(new_state) = counters[i].record(ok) {
            if new_state {
              info!("Upstream {} is now healthy ({} consecutive successes)", uri, config.healthy_threshold);
            } else {
              info!("Upstream {} is now unhealthy ({} consecutive failures)", uri, config.unhealthy_threshold);
            }
            health.set(new_state);
          }
        });

        // Warn if all upstreams are unhealthy
        if upstreams.iter().all(|(_, h)| !h.is_healthy()) {
          warn!("All upstreams are unhealthy, serving best-effort");
        }
      }
    }
  }
}
