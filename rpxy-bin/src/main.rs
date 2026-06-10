// Global allocator selection.
// - `dhat-heap` (developer-only profiling): use dhat's allocator on every OS.
// - otherwise: mimalloc, except on illumos where the system allocator is used.
// The two are mutually exclusive so only one `#[global_allocator]` is defined.
#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[cfg(all(not(feature = "dhat-heap"), not(target_os = "illumos")))]
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod config;
mod constants;
mod error;
mod log;

#[cfg(feature = "acme")]
use crate::config::build_acme_manager;
#[cfg(feature = "sticky-cookie")]
use crate::config::build_sticky_cookie_secret;
use crate::{
  config::{ConfigToml, ConfigTomlReloader, build_cert_manager, build_settings, parse_opts},
  constants::CONFIG_WATCH_DELAY_SECS,
  error::*,
  log::*,
};
use hot_reload::{ReloaderConfig, ReloaderReceiver, ReloaderService};
#[cfg(feature = "sticky-cookie")]
use rpxy_lib::StickyCookieSecret;
use rpxy_lib::{RpxyOptions, RpxyOptionsBuilder, entrypoint};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn main() {
  // Keep the heap profiler alive for the whole process. On drop it writes
  // `dhat-heap.json`; because `std::process::exit` skips destructors, the exit
  // call below is moved out of the async block and the profiler is dropped first.
  #[cfg(feature = "dhat-heap")]
  let dhat_profiler = dhat::Profiler::new_heap();

  let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
  runtime_builder.enable_all();
  runtime_builder.thread_name("rpxy");
  let runtime = runtime_builder.build().unwrap();

  let exit_code: i32 = runtime.block_on(async {
    // Initially load options
    let Ok(parsed_opts) = parse_opts() else {
      return 1;
    };

    init_logger(parsed_opts.log_dir_path.as_deref());

    // Read the unsafe debug-header logging opt-out once at startup.
    // Not hot-reloaded; threaded into rpxy-lib via RpxyOptions.
    let unsafe_debug_headers = unsafe_debug_headers_enabled();

    let reloader_config = ReloaderConfig::hybrid(CONFIG_WATCH_DELAY_SECS);

    let (config_service, config_rx) =
      ReloaderService::<ConfigTomlReloader, ConfigToml, String>::new(&parsed_opts.config_file_path, reloader_config)
        .await
        .unwrap();

    // When profiling, allow Ctrl-C to return gracefully so the dhat profiler is
    // dropped and `dhat-heap.json` is flushed. In normal builds this future never
    // resolves, so the select arm is inert and process behavior is unchanged.
    let shutdown_signal = async {
      #[cfg(feature = "dhat-heap")]
      {
        let _ = tokio::signal::ctrl_c().await;
      }
      #[cfg(not(feature = "dhat-heap"))]
      {
        std::future::pending::<()>().await;
      }
    };

    tokio::select! {
      config_res = config_service.start_with_realtime() => {
        if let Err(e) = config_res {
          error!("config reloader service exited: {e}");
          return 1;
        }
      }
      rpxy_res = rpxy_service(config_rx, runtime.handle().clone(), unsafe_debug_headers) => {
        if let Err(e) = rpxy_res {
          error!("rpxy service exited: {e}");
          return 1;
        }
      }
      _ = shutdown_signal => {
        info!("SIGINT received; shutting down to flush the dhat heap profile");
      }
    }
    0
  });

  // Drop the profiler (flushing `dhat-heap.json`) before the exit that skips destructors.
  #[cfg(feature = "dhat-heap")]
  drop(dhat_profiler);

  std::process::exit(exit_code);
}

/// rpxy service definition
struct RpxyService {
  runtime_handle: tokio::runtime::Handle,
  proxy_conf: rpxy_lib::ProxyConfig,
  app_conf: rpxy_lib::AppConfigList,
  cert_service: Option<Arc<ReloaderService<rpxy_certs::CryptoReloader, rpxy_certs::ServerCryptoBase>>>,
  cert_rx: Option<ReloaderReceiver<rpxy_certs::ServerCryptoBase>>,
  /// Operator opt-out for credential-header redaction in DEBUG logs,
  /// read once at startup from `RPXY_UNSAFE_DEBUG_HEADERS`.
  unsafe_debug_headers: bool,
  #[cfg(feature = "sticky-cookie")]
  sticky_cookie_secret: Option<Arc<StickyCookieSecret>>,
  #[cfg(feature = "acme")]
  acme_manager: Option<rpxy_acme::AcmeManager>,
}

impl RpxyService {
  /// Create a new RpxyService from config and runtime handle.
  async fn new(
    config_toml: &ConfigToml,
    runtime_handle: tokio::runtime::Handle,
    unsafe_debug_headers: bool,
  ) -> Result<Self, anyhow::Error> {
    let (proxy_conf, app_conf) = build_settings(config_toml).map_err(|e| anyhow!("Invalid configuration: {e}"))?;
    #[cfg(feature = "sticky-cookie")]
    let sticky_cookie_secret =
      build_sticky_cookie_secret(config_toml).map_err(|e| anyhow!("Invalid sticky-cookie configuration: {e}"))?;

    let (cert_service, cert_rx) = build_cert_manager(config_toml)
      .await
      .map_err(|e| anyhow!("Invalid cert configuration: {e}"))?
      .map(|(s, r)| (Some(Arc::new(s)), Some(r)))
      .unwrap_or((None, None));

    Ok(Self {
      runtime_handle: runtime_handle.clone(),
      proxy_conf,
      app_conf,
      cert_service,
      cert_rx,
      unsafe_debug_headers,
      #[cfg(feature = "sticky-cookie")]
      sticky_cookie_secret,
      #[cfg(feature = "acme")]
      acme_manager: build_acme_manager(config_toml, runtime_handle.clone()).await?,
    })
  }

  async fn start(&self, cancel_token: CancellationToken) -> Result<(), anyhow::Error> {
    let RpxyService {
      runtime_handle,
      proxy_conf,
      app_conf,
      cert_service: _,
      cert_rx,
      unsafe_debug_headers,
      #[cfg(feature = "sticky-cookie")]
      sticky_cookie_secret,
      #[cfg(feature = "acme")]
      acme_manager,
    } = self;

    #[cfg(feature = "acme")]
    {
      let (acme_join_handles, server_config_acme_challenge) = acme_manager
        .as_ref()
        .map(|m| m.spawn_manager_tasks(cancel_token.child_token()))
        .unwrap_or((vec![], Default::default()));
      let mut builder = RpxyOptionsBuilder::default();
      builder
        .proxy_config(proxy_conf.clone())
        .app_config_list(app_conf.clone())
        .cert_rx(cert_rx.clone())
        .runtime_handle(runtime_handle.clone())
        .unsafe_debug_headers(*unsafe_debug_headers)
        .server_configs_acme_challenge(Arc::new(server_config_acme_challenge));
      #[cfg(feature = "sticky-cookie")]
      builder.sticky_cookie_secret(sticky_cookie_secret.clone());
      let rpxy_opts = builder.build()?;
      self
        .start_inner(rpxy_opts, cancel_token, acme_join_handles)
        .await
        .map_err(|e| anyhow!(e))
    }

    #[cfg(not(feature = "acme"))]
    {
      let mut builder = RpxyOptionsBuilder::default();
      builder
        .proxy_config(proxy_conf.clone())
        .app_config_list(app_conf.clone())
        .cert_rx(cert_rx.clone())
        .runtime_handle(runtime_handle.clone())
        .unsafe_debug_headers(*unsafe_debug_headers);
      #[cfg(feature = "sticky-cookie")]
      builder.sticky_cookie_secret(sticky_cookie_secret.clone());
      let rpxy_opts = builder.build()?;
      self.start_inner(rpxy_opts, cancel_token).await.map_err(|e| anyhow!(e))
    }
  }

  /// Wrapper of entry point for rpxy service with certificate management service
  async fn start_inner(
    &self,
    rpxy_opts: RpxyOptions,
    cancel_token: CancellationToken,
    #[cfg(feature = "acme")] acme_task_handles: Vec<tokio::task::JoinHandle<()>>,
  ) -> Result<(), anyhow::Error> {
    let cancel_token = cancel_token.clone();
    let runtime_handle = rpxy_opts.runtime_handle.clone();

    // spawn rpxy entrypoint, where cancellation token is possibly contained inside the service
    let cancel_token_clone = cancel_token.clone();
    let child_cancel_token = cancel_token.child_token();
    let rpxy_handle = runtime_handle.spawn(async move {
      if let Err(e) = entrypoint(&rpxy_opts, child_cancel_token).await {
        error!("rpxy entrypoint exited on error: {e}");
        cancel_token_clone.cancel();
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
    let child_cancel_token = cancel_token.child_token();
    let cert_handle = runtime_handle.spawn(async move {
      tokio::select! {
        cert_res = cert_service.start() => {
          if let Err(ref e) = cert_res {
            error!("cert reloader service exited on error: {e}");
          }
          cancel_token_clone.cancel();
          cert_res.map_err(|e| anyhow!(e))
        }
        _ = child_cancel_token.cancelled() => {
          debug!("cert reloader service terminated");
          Ok(())
        }
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
        cancel_token_clone.cancel();
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

async fn rpxy_service(
  mut config_rx: ReloaderReceiver<ConfigToml, String>,
  runtime_handle: tokio::runtime::Handle,
  unsafe_debug_headers: bool,
) -> Result<(), anyhow::Error> {
  info!("Start rpxy service with dynamic config reloader");
  // Initial loading
  config_rx.changed().await?;
  let config_toml = config_rx
    .borrow()
    .clone()
    .ok_or(anyhow!("Something wrong in config reloader receiver"))?;
  let mut service = RpxyService::new(&config_toml, runtime_handle.clone(), unsafe_debug_headers).await?;

  // Continuous monitoring
  loop {
    // Notifier for proxy service termination
    let cancel_token = tokio_util::sync::CancellationToken::new();

    tokio::select! {
      /* ---------- */
      rpxy_res = service.start(cancel_token.clone()) => {
        if let Err(ref e) = rpxy_res {
          error!("rpxy service exited on error: {e}");
        } else {
          error!("rpxy service exited");
        }
        return rpxy_res.map_err(|e| anyhow!(e));
      }
      /* ---------- */
      _ = config_rx.changed() => {
        let Some(new_config_toml) = config_rx.get() else {
          error!("Something wrong in config reloader receiver");
          return Err(anyhow!("Something wrong in config reloader receiver"));
        };
        match RpxyService::new(&new_config_toml, runtime_handle.clone(), unsafe_debug_headers).await {
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
