#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod cert_file_reader;
mod config;
mod constants;
mod error;
mod log;

use crate::{
  config::{build_settings, parse_opts, ConfigToml, ConfigTomlReloader},
  constants::CONFIG_WATCH_DELAY_SECS,
  log::*,
};
use hot_reload::{ReloaderReceiver, ReloaderService};
use rpxy_lib::entrypoint;

fn main() {
  init_logger();

  let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
  runtime_builder.enable_all();
  runtime_builder.thread_name("rpxy");
  let runtime = runtime_builder.build().unwrap();

  runtime.block_on(async {
    // Initially load options
    let Ok(parsed_opts) = parse_opts() else {
      error!("Invalid toml file");
      std::process::exit(1);
    };

    if !parsed_opts.watch {
      if let Err(e) = rpxy_service_without_watcher(&parsed_opts.config_file_path, runtime.handle().clone()).await {
        error!("rpxy service existed: {e}");
        std::process::exit(1);
      }
    } else {
      let (config_service, config_rx) = ReloaderService::<ConfigTomlReloader, ConfigToml>::new(
        &parsed_opts.config_file_path,
        CONFIG_WATCH_DELAY_SECS,
        false,
      )
      .await
      .unwrap();

      tokio::select! {
        Err(e) = config_service.start() => {
          error!("config reloader service exited: {e}");
          std::process::exit(1);
        }
        Err(e) = rpxy_service_with_watcher(config_rx, runtime.handle().clone()) => {
          error!("rpxy service existed: {e}");
          std::process::exit(1);
        }
      }
    }
  });
}

async fn rpxy_service_without_watcher(
  config_file_path: &str,
  runtime_handle: tokio::runtime::Handle,
) -> Result<(), anyhow::Error> {
  info!("Start rpxy service");
  let config_toml = match ConfigToml::new(config_file_path) {
    Ok(v) => v,
    Err(e) => {
      error!("Invalid toml file: {e}");
      std::process::exit(1);
    }
  };
  let (proxy_conf, app_conf) = match build_settings(&config_toml) {
    Ok(v) => v,
    Err(e) => {
      error!("Invalid configuration: {e}");
      return Err(anyhow::anyhow!(e));
    }
  };
  entrypoint(&proxy_conf, &app_conf, &runtime_handle, None)
    .await
    .map_err(|e| anyhow::anyhow!(e))
}

async fn rpxy_service_with_watcher(
  mut config_rx: ReloaderReceiver<ConfigToml>,
  runtime_handle: tokio::runtime::Handle,
) -> Result<(), anyhow::Error> {
  info!("Start rpxy service with dynamic config reloader");
  // Initial loading
  config_rx.changed().await?;
  let config_toml = config_rx.borrow().clone().unwrap();
  let (mut proxy_conf, mut app_conf) = match build_settings(&config_toml) {
    Ok(v) => v,
    Err(e) => {
      error!("Invalid configuration: {e}");
      return Err(anyhow::anyhow!(e));
    }
  };

  // Notifier for proxy service termination
  let term_notify = std::sync::Arc::new(tokio::sync::Notify::new());

  // Continuous monitoring
  loop {
    tokio::select! {
      _ = entrypoint(&proxy_conf, &app_conf, &runtime_handle, Some(term_notify.clone())) => {
        error!("rpxy entrypoint exited");
        break;
      }
      _ = config_rx.changed() => {
        if config_rx.borrow().is_none() {
          error!("Something wrong in config reloader receiver");
          break;
        }
        let config_toml = config_rx.borrow().clone().unwrap();
        match build_settings(&config_toml) {
          Ok((p, a)) => {
            (proxy_conf, app_conf) = (p, a)
          },
          Err(e) => {
            error!("Invalid configuration. Configuration does not updated: {e}");
            continue;
          }
        };
        info!("Configuration updated. Terminate all spawned proxy services and force to re-bind TCP/UDP sockets");
        term_notify.notify_waiters();
        // tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
      }
      else => break
    }
  }

  Err(anyhow::anyhow!("rpxy or continuous monitoring service exited"))
}
