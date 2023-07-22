#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

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
    // Initially load config
    let Ok(config_path) = parse_opts() else {
        error!("Invalid toml file");
        std::process::exit(1);
    };
    let (config_service, config_rx) =
      ReloaderService::<ConfigTomlReloader, ConfigToml>::new(&config_path, CONFIG_WATCH_DELAY_SECS, false)
        .await
        .unwrap();

    tokio::select! {
      _ = config_service.start() => {
        error!("config reloader service exited");
      }
      _ = rpxy_service(config_rx, runtime.handle().clone()) => {
        error!("rpxy service existed");
      }
    }
  });
}

async fn rpxy_service(
  mut config_rx: ReloaderReceiver<ConfigToml>,
  runtime_handle: tokio::runtime::Handle,
) -> Result<(), anyhow::Error> {
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

  // Continuous monitoring
  loop {
    tokio::select! {
      _ = entrypoint(&proxy_conf, &app_conf, &runtime_handle) => {
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
        info!("Configuration updated. Force to re-bind TCP/UDP sockets");
      }
      else => break
    }
  }
  Ok(())
}
