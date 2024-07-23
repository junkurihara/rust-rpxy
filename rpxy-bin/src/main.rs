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
use rpxy_lib::{entrypoint, RpxyOptions, RpxyOptionsBuilder};

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

  let (cert_service, cert_rx) = build_cert_manager(&config_toml)
    .await
    .map_err(|e| anyhow!("Invalid cert configuration: {e}"))?
    .map(|(s, r)| (Some(s), Some(r)))
    .unwrap_or((None, None));

  #[cfg(feature = "acme")]
  {
    let acme_manager = build_acme_manager(&config_toml, runtime_handle.clone()).await?;
    let (acme_join_handles, server_config_acme_challenge) = acme_manager
      .as_ref()
      .map(|m| m.spawn_manager_tasks(None))
      .unwrap_or((vec![], Default::default()));
    let rpxy_opts = RpxyOptionsBuilder::default()
      .proxy_config(proxy_conf)
      .app_config_list(app_conf)
      .cert_rx(cert_rx)
      .runtime_handle(runtime_handle.clone())
      .server_configs_acme_challenge(std::sync::Arc::new(server_config_acme_challenge))
      .build()?;
    rpxy_entrypoint(&rpxy_opts, cert_service.as_ref(), acme_join_handles) //, &runtime_handle)
      .await
      .map_err(|e| anyhow!(e))
  }

  #[cfg(not(feature = "acme"))]
  {
    let rpxy_opts = RpxyOptionsBuilder::default()
      .proxy_config(proxy_conf.clone())
      .app_config_list(app_conf.clone())
      .cert_rx(cert_rx.clone())
      .runtime_handle(runtime_handle.clone())
      .build()?;
    rpxy_entrypoint(&rpxy_opts, cert_service.as_ref()) //, &runtime_handle)
      .await
      .map_err(|e| anyhow!(e))
  }
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

  #[cfg(feature = "acme")]
  let mut acme_manager = build_acme_manager(&config_toml, runtime_handle.clone()).await?;

  let mut cert_service_and_rx = build_cert_manager(&config_toml)
    .await
    .map_err(|e| anyhow!("Invalid cert configuration: {e}"))?;

  // Notifier for proxy service termination
  let term_notify = std::sync::Arc::new(tokio::sync::Notify::new());

  // Continuous monitoring
  loop {
    let (cert_service, cert_rx) = cert_service_and_rx
      .as_ref()
      .map(|(s, r)| (Some(s), Some(r)))
      .unwrap_or((None, None));

    #[cfg(feature = "acme")]
    let (acme_join_handles, server_config_acme_challenge) = acme_manager
      .as_ref()
      .map(|m| m.spawn_manager_tasks(Some(term_notify.clone())))
      .unwrap_or((vec![], Default::default()));

    let rpxy_opts = {
      #[cfg(feature = "acme")]
      let res = RpxyOptionsBuilder::default()
        .proxy_config(proxy_conf.clone())
        .app_config_list(app_conf.clone())
        .cert_rx(cert_rx.cloned())
        .runtime_handle(runtime_handle.clone())
        .term_notify(Some(term_notify.clone()))
        .server_configs_acme_challenge(std::sync::Arc::new(server_config_acme_challenge))
        .build();

      #[cfg(not(feature = "acme"))]
      let res = RpxyOptionsBuilder::default()
        .proxy_config(proxy_conf.clone())
        .app_config_list(app_conf.clone())
        .cert_rx(cert_rx.cloned())
        .runtime_handle(runtime_handle.clone())
        .term_notify(Some(term_notify.clone()))
        .build();
      res
    }?;

    tokio::select! {
      rpxy_res = {
        #[cfg(feature = "acme")]
        {
          rpxy_entrypoint(&rpxy_opts, cert_service, acme_join_handles)//, &runtime_handle)
        }
        #[cfg(not(feature = "acme"))]
        {
          rpxy_entrypoint(&rpxy_opts, cert_service)//, &runtime_handle)
        }
      } => {
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
        #[cfg(feature = "acme")]
        {
          match build_acme_manager(&config_toml, runtime_handle.clone()).await {
            Ok(m) => {
              acme_manager = m;
            },
            Err(e) => {
              error!("Invalid acme configuration. Configuration does not updated: {e}");
              continue;
            }
          }
        }

        info!("Configuration updated. Terminate all spawned services and force to re-bind TCP/UDP sockets");
        term_notify.notify_waiters();
        // tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
      }
      else => break
    }
  }

  Ok(())
}

#[cfg(not(feature = "acme"))]
/// Wrapper of entry point for rpxy service with certificate management service
async fn rpxy_entrypoint(
  rpxy_opts: &RpxyOptions,
  cert_service: Option<&ReloaderService<rpxy_certs::CryptoReloader, rpxy_certs::ServerCryptoBase>>,
  // runtime_handle: &tokio::runtime::Handle,
) -> Result<(), anyhow::Error> {
  // TODO: refactor: update routine
  if let Some(cert_service) = cert_service {
    tokio::select! {
      rpxy_res = entrypoint(rpxy_opts) => {
        error!("rpxy entrypoint exited");
        rpxy_res.map_err(|e| anyhow!(e))
      }
      cert_res = cert_service.start() => {
        error!("cert reloader service exited");
        cert_res.map_err(|e| anyhow!(e))
      }
    }
  } else {
    entrypoint(rpxy_opts).await.map_err(|e| anyhow!(e))
  }
}

#[cfg(feature = "acme")]
/// Wrapper of entry point for rpxy service with certificate management service
async fn rpxy_entrypoint(
  rpxy_opts: &RpxyOptions,
  cert_service: Option<&ReloaderService<rpxy_certs::CryptoReloader, rpxy_certs::ServerCryptoBase>>,
  acme_task_handles: Vec<tokio::task::JoinHandle<()>>,
  // runtime_handle: &tokio::runtime::Handle,
) -> Result<(), anyhow::Error> {
  // TODO: refactor: update routine
  if let Some(cert_service) = cert_service {
    if acme_task_handles.is_empty() {
      tokio::select! {
        rpxy_res = entrypoint(rpxy_opts) => {
          error!("rpxy entrypoint exited");
          rpxy_res.map_err(|e| anyhow!(e))
        }
        cert_res = cert_service.start() => {
          error!("cert reloader service exited");
          cert_res.map_err(|e| anyhow!(e))
        }
      }
    } else {
      let select_all = futures_util::future::select_all(acme_task_handles);
      tokio::select! {
        rpxy_res = entrypoint(rpxy_opts) => {
          error!("rpxy entrypoint exited");
          rpxy_res.map_err(|e| anyhow!(e))
        }
        (acme_res, _, _) = select_all => {
          error!("acme manager exited");
          acme_res.map_err(|e| anyhow!(e))
        }
        cert_res = cert_service.start() => {
          error!("cert reloader service exited");
          cert_res.map_err(|e| anyhow!(e))
        }
      }
    }
  } else {
    entrypoint(rpxy_opts).await.map_err(|e| anyhow!(e))
  }
}
