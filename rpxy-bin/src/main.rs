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
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

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
        config_res = config_service.start() => {
          if let Err(e) = config_res {
            error!("config reloader service exited: {e}");
            std::process::exit(1);
          }
        }
        rpxy_res = rpxy_service_with_watcher(config_rx, runtime.handle().clone()) => {
          if let Err(e) = rpxy_res {
            error!("rpxy service existed: {e}");
            std::process::exit(1);
          }
        }
      }
      std::process::exit(0);
    }
  });
}

/// rpxy service definition
struct RpxyService {
  runtime_handle: tokio::runtime::Handle,
  proxy_conf: rpxy_lib::ProxyConfig,
  app_conf: rpxy_lib::AppConfigList,
  cert_service: Option<Arc<ReloaderService<rpxy_certs::CryptoReloader, rpxy_certs::ServerCryptoBase>>>,
  cert_rx: Option<ReloaderReceiver<rpxy_certs::ServerCryptoBase>>,
  #[cfg(feature = "acme")]
  acme_manager: Option<rpxy_acme::AcmeManager>,
}

impl RpxyService {
  async fn new(config_toml: &ConfigToml, runtime_handle: tokio::runtime::Handle) -> Result<Self, anyhow::Error> {
    let (proxy_conf, app_conf) = build_settings(config_toml).map_err(|e| anyhow!("Invalid configuration: {e}"))?;

    let (cert_service, cert_rx) = build_cert_manager(config_toml)
      .await
      .map_err(|e| anyhow!("Invalid cert configuration: {e}"))?
      .map(|(s, r)| (Some(Arc::new(s)), Some(r)))
      .unwrap_or((None, None));

    Ok(RpxyService {
      runtime_handle: runtime_handle.clone(),
      proxy_conf,
      app_conf,
      cert_service,
      cert_rx,
      #[cfg(feature = "acme")]
      acme_manager: build_acme_manager(config_toml, runtime_handle.clone()).await?,
    })
  }

  async fn start(&self, cancel_token: Option<CancellationToken>) -> Result<(), anyhow::Error> {
    let RpxyService {
      runtime_handle,
      proxy_conf,
      app_conf,
      cert_service: _,
      cert_rx,
      #[cfg(feature = "acme")]
      acme_manager,
    } = self;

    #[cfg(feature = "acme")]
    {
      let (acme_join_handles, server_config_acme_challenge) = acme_manager
        .as_ref()
        .map(|m| m.spawn_manager_tasks(cancel_token.as_ref().map(|t| t.child_token())))
        .unwrap_or((vec![], Default::default()));
      let rpxy_opts = RpxyOptionsBuilder::default()
        .proxy_config(proxy_conf.clone())
        .app_config_list(app_conf.clone())
        .cert_rx(cert_rx.clone())
        .runtime_handle(runtime_handle.clone())
        .cancel_token(cancel_token.as_ref().map(|t| t.child_token()))
        .server_configs_acme_challenge(Arc::new(server_config_acme_challenge))
        .build()?;
      self.start_inner(rpxy_opts, acme_join_handles).await.map_err(|e| anyhow!(e))
    }

    #[cfg(not(feature = "acme"))]
    {
      let rpxy_opts = RpxyOptionsBuilder::default()
        .proxy_config(proxy_conf.clone())
        .app_config_list(app_conf.clone())
        .cert_rx(cert_rx.clone())
        .runtime_handle(runtime_handle.clone())
        .cancel_token(cancel_token.as_ref().map(|t| t.child_token()))
        .build()?;
      self.start_inner(rpxy_opts).await.map_err(|e| anyhow!(e))
    }
  }

  /// Wrapper of entry point for rpxy service with certificate management service
  async fn start_inner(
    &self,
    rpxy_opts: RpxyOptions,
    #[cfg(feature = "acme")] acme_task_handles: Vec<tokio::task::JoinHandle<()>>,
  ) -> Result<(), anyhow::Error> {
    let cancel_token = rpxy_opts.cancel_token.clone();
    let runtime_handle = rpxy_opts.runtime_handle.clone();

    // spawn rpxy entrypoint, where cancellation token is possibly contained inside the service
    let cancel_token_clone = cancel_token.clone();
    let rpxy_handle = runtime_handle.spawn(async move {
      if let Err(e) = entrypoint(&rpxy_opts).await {
        error!("rpxy entrypoint exited on error: {e}");
        if let Some(cancel_token) = cancel_token_clone {
          cancel_token.cancel();
        }
        return Err(anyhow!(e));
      }
      Ok(())
    });

    if self.cert_service.is_none() {
      return rpxy_handle.await?;
    }

    // spawn certificate reloader service, where cert service does not have cancellation token inside the service
    let cert_service = self.cert_service.as_ref().unwrap().clone();
    let cancel_token_clone = cancel_token.clone();
    let child_cancel_token = cancel_token.as_ref().map(|c| c.child_token());
    let cert_handle = runtime_handle.spawn(async move {
      if let Some(child_cancel_token) = child_cancel_token {
        tokio::select! {
          cert_res = cert_service.start() => {
            if let Err(ref e) = cert_res {
              error!("cert reloader service exited on error: {e}");
            }
            cancel_token_clone.unwrap().cancel();
            cert_res.map_err(|e| anyhow!(e))
          }
          _ = child_cancel_token.cancelled() => {
            debug!("cert reloader service terminated");
            Ok(())
          }
        }
      } else {
        cert_service.start().await.map_err(|e| anyhow!(e))
      }
    });

    #[cfg(not(feature = "acme"))]
    {
      let (rpxy_res, cert_res) = tokio::join!(rpxy_handle, cert_handle);
      let (rpxy_res, cert_res) = (rpxy_res?, cert_res?);
      match (rpxy_res, cert_res) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(e), _) => Err(e),
        (_, Err(e)) => Err(e),
      }
    }

    #[cfg(feature = "acme")]
    {
      if acme_task_handles.is_empty() {
        let (rpxy_res, cert_res) = tokio::join!(rpxy_handle, cert_handle);
        let (rpxy_res, cert_res) = (rpxy_res?, cert_res?);
        return match (rpxy_res, cert_res) {
          (Ok(()), Ok(())) => Ok(()),
          (Err(e), _) => Err(e),
          (_, Err(e)) => Err(e),
        };
      }

      // spawn acme manager tasks, where cancellation token is possibly contained inside the service
      let select_all = futures_util::future::select_all(acme_task_handles);
      let cancel_token_clone = cancel_token.clone();
      let acme_handle = runtime_handle.spawn(async move {
        let (acme_res, _, _) = select_all.await;
        if let Err(ref e) = acme_res {
          error!("acme manager exited on error: {e}");
        }
        if let Some(cancel_token) = cancel_token_clone {
          cancel_token.cancel();
        }
        acme_res.map_err(|e| anyhow!(e))
      });
      let (rpxy_res, cert_res, acme_res) = tokio::join!(rpxy_handle, cert_handle, acme_handle);
      let (rpxy_res, cert_res, acme_res) = (rpxy_res?, cert_res?, acme_res?);
      match (rpxy_res, cert_res, acme_res) {
        (Ok(()), Ok(()), Ok(())) => Ok(()),
        (Err(e), _, _) => Err(e),
        (_, Err(e), _) => Err(e),
        (_, _, Err(e)) => Err(e),
      }
    }
  }
}

async fn rpxy_service_without_watcher(
  config_file_path: &str,
  runtime_handle: tokio::runtime::Handle,
) -> Result<(), anyhow::Error> {
  info!("Start rpxy service");
  let config_toml = ConfigToml::new(config_file_path).map_err(|e| anyhow!("Invalid toml file: {e}"))?;
  let service = RpxyService::new(&config_toml, runtime_handle).await?;
  service.start(None).await
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
  let mut service = RpxyService::new(&config_toml, runtime_handle.clone()).await?;

  // Continuous monitoring
  loop {
    // Notifier for proxy service termination
    let cancel_token = tokio_util::sync::CancellationToken::new();

    tokio::select! {
      /* ---------- */
      rpxy_res = service.start(Some(cancel_token.clone())) => {
        if let Err(ref e) = rpxy_res {
          error!("rpxy service exited on error: {e}");
        } else {
          error!("rpxy service exited");
        }
        return rpxy_res.map_err(|e| anyhow!(e));
      }
      /* ---------- */
      _ = config_rx.changed() => {
        let Some(new_config_toml) = config_rx.borrow().clone() else {
          error!("Something wrong in config reloader receiver");
          return Err(anyhow!("Something wrong in config reloader receiver"));
        };
        match RpxyService::new(&new_config_toml, runtime_handle.clone()).await {
          Ok(new_service) => {
            info!("Configuration updated.");
            service = new_service;
          },
          Err(e) => {
            error!("rpxy failed to be ready. Configuration does not updated: {e}");
          }
        };
        info!("Terminate all spawned services and force to re-bind TCP/UDP sockets");
        cancel_token.cancel();
      }
    }
  }
}
