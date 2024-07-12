#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod config;
mod constants;
mod error;
mod log;

#[cfg(feature = "acme")]
use crate::config::build_acme_manager;
use crate::{
  config::{build_cert_manager, build_settings, parse_opts, ConfigToml, ConfigTomlReloader},
  constants::CONFIG_WATCH_DELAY_SECS,
  error::*,
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
      let (config_service, config_rx) =
        ReloaderService::<ConfigTomlReloader, ConfigToml>::new(&parsed_opts.config_file_path, CONFIG_WATCH_DELAY_SECS, false)
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
        else => {
          std::process::exit(0);
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
  let config_toml = ConfigToml::new(config_file_path).map_err(|e| anyhow!("Invalid toml file: {e}"))?;
  let (proxy_conf, app_conf) = build_settings(&config_toml).map_err(|e| anyhow!("Invalid configuration: {e}"))?;

  #[cfg(feature = "acme")] // TODO: CURRENTLY NOT IMPLEMENTED, UNDER DESIGNING
  let acme_manager = build_acme_manager(&config_toml).await;

  let cert_service_and_rx = build_cert_manager(&config_toml)
    .await
    .map_err(|e| anyhow!("Invalid cert configuration: {e}"))?;

  rpxy_entrypoint(&proxy_conf, &app_conf, cert_service_and_rx.as_ref(), &runtime_handle, None)
    .await
    .map_err(|e| anyhow!(e))
}

async fn rpxy_service_with_watcher(
  mut config_rx: ReloaderReceiver<ConfigToml>,
  runtime_handle: tokio::runtime::Handle,
) -> Result<(), anyhow::Error> {
  info!("Start rpxy service with dynamic config reloader");
  // Initial loading
  config_rx.changed().await?;
  let config_toml = config_rx
    .borrow()
    .clone()
    .ok_or(anyhow!("Something wrong in config reloader receiver"))?;
  let (mut proxy_conf, mut app_conf) = build_settings(&config_toml).map_err(|e| anyhow!("Invalid configuration: {e}"))?;

  #[cfg(feature = "acme")] // TODO: CURRENTLY NOT IMPLEMENTED, UNDER DESIGNING
  let acme_manager = build_acme_manager(&config_toml).await;

  let mut cert_service_and_rx = build_cert_manager(&config_toml)
    .await
    .map_err(|e| anyhow!("Invalid cert configuration: {e}"))?;

  // Notifier for proxy service termination
  let term_notify = std::sync::Arc::new(tokio::sync::Notify::new());

  // Continuous monitoring
  loop {
    tokio::select! {
      rpxy_res = rpxy_entrypoint(&proxy_conf, &app_conf, cert_service_and_rx.as_ref(), &runtime_handle, Some(term_notify.clone())) => {
        error!("rpxy entrypoint or cert service exited");
        return rpxy_res.map_err(|e| anyhow!(e));
      }
      _ = config_rx.changed() => {
        let Some(config_toml) = config_rx.borrow().clone() else {
          error!("Something wrong in config reloader receiver");
          return Err(anyhow!("Something wrong in config reloader receiver"));
        };
        match build_settings(&config_toml) {
          Ok((p, a)) => {
            (proxy_conf, app_conf) = (p, a)
          },
          Err(e) => {
            error!("Invalid configuration. Configuration does not updated: {e}");
            continue;
          }
        };
        match build_cert_manager(&config_toml).await {
          Ok(c) => {
            cert_service_and_rx = c;
          },
          Err(e) => {
            error!("Invalid cert configuration. Configuration does not updated: {e}");
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

  Ok(())
}

/// Wrapper of entry point for rpxy service with certificate management service
async fn rpxy_entrypoint(
  proxy_config: &rpxy_lib::ProxyConfig,
  app_config_list: &rpxy_lib::AppConfigList,
  cert_service_and_rx: Option<&(
    ReloaderService<rpxy_certs::CryptoReloader, rpxy_certs::ServerCryptoBase>,
    ReloaderReceiver<rpxy_certs::ServerCryptoBase>,
  )>, // TODO:
  runtime_handle: &tokio::runtime::Handle,
  term_notify: Option<std::sync::Arc<tokio::sync::Notify>>,
) -> Result<(), anyhow::Error> {
  if let Some((cert_service, cert_rx)) = cert_service_and_rx {
    tokio::select! {
      rpxy_res = entrypoint(proxy_config, app_config_list, Some(cert_rx), runtime_handle, term_notify) => {
        error!("rpxy entrypoint exited");
        rpxy_res.map_err(|e| anyhow!(e))
      }
      cert_res = cert_service.start() => {
        error!("cert reloader service exited");
        cert_res.map_err(|e| anyhow!(e))
      }
    }
  } else {
    entrypoint(proxy_config, app_config_list, None, runtime_handle, term_notify)
      .await
      .map_err(|e| anyhow!(e))
  }
}
