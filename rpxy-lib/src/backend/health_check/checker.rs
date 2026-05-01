use super::{UpstreamHealth, check_http::HealthCheckHttpClient, check_tcp::check_tcp, counter::ConsecutiveCounter};
use crate::{
  backend::BackendAppManager,
  error::RpxyResult,
  globals::{HealthCheckConfig, HealthCheckType},
  log::*,
};
use futures::future::join_all;
use std::sync::Arc;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

/// Check if any configured health check uses HTTP type.
fn has_http_health_check(app_manager: &BackendAppManager) -> bool {
  app_manager.apps.iter().any(|(_name, backend_app)| {
    backend_app.path_manager.iter_candidates().any(|(_path, candidates)| {
      candidates
        .health_check_config
        .as_ref()
        .is_some_and(|c| matches!(&c.check_type, HealthCheckType::Http { .. }))
    })
  })
}

/// Spawn health checker tasks for all upstream candidates that have health check enabled.
/// Returns join handles for the spawned tasks.
/// Fails if HTTP health checks are configured but the HTTP client cannot be built.
pub(crate) fn spawn_health_checkers(
  app_manager: &Arc<BackendAppManager>,
  cancel_token: CancellationToken,
  runtime_handle: &tokio::runtime::Handle,
) -> RpxyResult<Vec<tokio::task::JoinHandle<RpxyResult<()>>>> {
  // Only build the HTTP client if at least one health check uses HTTP type.
  // Fail hard if HTTP health checks are configured but the client cannot be built.
  let http_client = if has_http_health_check(app_manager) {
    let client = HealthCheckHttpClient::try_new(runtime_handle)?;
    Some(Arc::new(client))
  } else {
    None
  };

  let mut handles = Vec::new();

  app_manager.apps.iter().for_each(|(_app_name, backend_app)| {
    let server_name = (&backend_app.server_name).try_into().unwrap_or_else(|_| "<none>".to_string());
    let sub_handles = backend_app.path_manager.iter_candidates().filter_map(|(path, candidates)| {
      // Collect upstreams that have health check enabled (i.e., have UpstreamHealth)
      let health_upstreams: Vec<_> = candidates
        .inner
        .iter()
        .filter_map(|upstream| upstream.health.as_ref().map(|h| (upstream.uri.clone(), Arc::clone(h))))
        .collect();

      if health_upstreams.is_empty() {
        return None;
      }

      let config = candidates.health_check_config.as_ref()?;

      let path_str: String = path.try_into().unwrap_or_else(|_| "<none>".to_string());
      let num_upstreams = health_upstreams.len();

      info!(
        "[{server_name}] Health checker started for path \"{path_str}\" ({num_upstreams} upstreams, {:?}, interval={:?}, timeout={:?}, healthy_threshold={}, unhealthy_threshold={})",
        config.check_type, config.interval, config.timeout, config.healthy_threshold, config.unhealthy_threshold
      );

      let config = config.clone();
      let cancel = cancel_token.clone();
      let server_name = server_name.clone();
      let task_http_client = match config.check_type {
        HealthCheckType::Http { .. } => http_client.clone(),
        _ => None,
      };
      let handle = runtime_handle.spawn(async move {
        run_health_checker(server_name, path_str, health_upstreams, config, cancel, task_http_client).await
      });

      Some(handle)
    });
    handles.extend(sub_handles);
  });

  Ok(handles)
}

/// Run a single health checker task for a group of upstreams.
/// Runs an immediate first probe, then schedules subsequent probes with a fixed interval.
async fn run_health_checker(
  server_name: String,
  path_str: String,
  upstreams: Vec<(hyper::Uri, Arc<UpstreamHealth>)>,
  config: HealthCheckConfig,
  cancel: CancellationToken,
  http_client: Option<Arc<HealthCheckHttpClient>>,
) -> RpxyResult<()> {
  let mut counters: Vec<ConsecutiveCounter> = upstreams
    .iter()
    .map(|_| ConsecutiveCounter::new(config.unhealthy_threshold, config.healthy_threshold))
    .collect();
  let mut ticker = tokio::time::interval(config.interval);
  ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

  loop {
    tokio::select! {
      _ = cancel.cancelled() => {
        debug!("[{server_name}:{path_str}] Health checker terminated");
        return Ok(());
      }
      _ = ticker.tick() => {
        let server_name = &server_name;
        let config = &config;
        let http_client = http_client.as_deref();
        let checks = upstreams.iter().enumerate().map(|(i, (uri, _health))| async move {
          let ok = match &config.check_type {
            HealthCheckType::Tcp => check_tcp(server_name, uri, config.timeout).await,
            HealthCheckType::Http { path, expected_status } => match http_client {
              Some(client) => client.check(server_name, uri, path, *expected_status, config.timeout).await,
              None => {
                error!("[{server_name}] HTTP health check client is unavailable, treating as unhealthy");
                false
              }
            },
          };
          (i, ok)
        });

        let results = join_all(checks).await;

        results.into_iter().for_each(|(i, ok)| {
          let (ref uri, ref health) = upstreams[i];
          if !ok {
            debug!("[{server_name}:{path_str}] Health check failed for {uri}");
          }
          if let Some(new_state) = counters[i].record(ok) {
            if new_state {
              info!("[{server_name}:{path_str}] Upstream {uri} is now healthy ({} consecutive successes)", config.healthy_threshold);
            } else {
              info!("[{server_name}:{path_str}] Upstream {uri} is now unhealthy ({} consecutive failures)", config.unhealthy_threshold);
            }
            health.set(new_state);
          }
        });

        // Warn if all upstreams are unhealthy
        if upstreams.iter().all(|(_, h)| !h.is_healthy()) {
          warn!("[{server_name}:{path_str}] All upstreams are unhealthy, serving best-effort");
        }
      }
    }
  }
}
