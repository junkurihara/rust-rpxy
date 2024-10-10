mod backend;
mod constants;
mod count;
mod error;
mod forwarder;
mod globals;
mod hyper_ext;
mod log;
mod message_handler;
mod name_exp;
mod proxy;
/* ------------------------------------------------ */
use crate::{
  // crypto::build_cert_reloader,
  error::*,
  forwarder::Forwarder,
  globals::Globals,
  log::*,
  message_handler::HttpMessageHandlerBuilder,
  proxy::Proxy,
};
use futures::future::join_all;
use hot_reload::ReloaderReceiver;
use rpxy_certs::ServerCryptoBase;
use rustls::crypto::{aws_lc_rs, CryptoProvider};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/* ------------------------------------------------ */
pub use crate::globals::{AppConfig, AppConfigList, ProxyConfig, ReverseProxyConfig, TlsConfig, UpstreamUri};
pub mod reexports {
  pub use hyper::Uri;
}

#[derive(derive_builder::Builder)]
/// rpxy entrypoint args
pub struct RpxyOptions {
  /// Configuration parameters for proxy transport and request handlers
  pub proxy_config: ProxyConfig,
  /// List of application configurations
  pub app_config_list: AppConfigList,
  /// Certificate reloader service receiver
  pub cert_rx: Option<ReloaderReceiver<ServerCryptoBase>>, // TODO:
  /// Async task runtime handler
  pub runtime_handle: tokio::runtime::Handle,
  /// Notify object to stop async tasks
  pub cancel_token: Option<CancellationToken>,

  #[cfg(feature = "acme")]
  /// ServerConfig used for only ACME challenge for ACME domains
  pub server_configs_acme_challenge: Arc<rustc_hash::FxHashMap<String, Arc<rustls::ServerConfig>>>,
}

/// Entrypoint that creates and spawns tasks of reverse proxy services
pub async fn entrypoint(
  RpxyOptions {
    proxy_config,
    app_config_list,
    cert_rx, // TODO:
    runtime_handle,
    cancel_token,
    #[cfg(feature = "acme")]
    server_configs_acme_challenge,
  }: &RpxyOptions,
) -> RpxyResult<()> {
  #[cfg(all(feature = "http3-quinn", feature = "http3-s2n"))]
  warn!("Both \"http3-quinn\" and \"http3-s2n\" features are enabled. \"http3-quinn\" will be used");

  #[cfg(all(feature = "native-tls-backend", feature = "rustls-backend"))]
  warn!("Both \"native-tls-backend\" and \"rustls-backend\" features are enabled. \"rustls-backend\" will be used");

  // For initial message logging
  if proxy_config.listen_sockets.iter().any(|addr| addr.is_ipv6()) {
    info!("Listen both IPv4 and IPv6")
  } else {
    info!("Listen IPv4")
  }
  if proxy_config.http_port.is_some() {
    info!("Listen port: {}", proxy_config.http_port.unwrap());
  }
  if proxy_config.https_port.is_some() {
    info!("Listen port: {} (for TLS)", proxy_config.https_port.unwrap());
  }
  if proxy_config.connection_handling_timeout.is_some() {
    info!(
      "Force connection handling timeout: {:?} sec",
      proxy_config.connection_handling_timeout.unwrap_or_default().as_secs()
    );
  }
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  if proxy_config.http3 {
    info!("Experimental HTTP/3.0 is enabled. Note it is still very unstable.");
  }
  if !proxy_config.sni_consistency {
    info!("Ignore consistency between TLS SNI and Host header (or Request line). Note it violates RFC.");
  }
  #[cfg(feature = "cache")]
  if proxy_config.cache_enabled {
    info!("Cache is enabled: cache dir = {:?}", proxy_config.cache_dir.as_ref().unwrap());
  } else {
    info!("Cache is disabled")
  }

  // Install aws_lc_rs as default crypto provider for rustls
  let _ = CryptoProvider::install_default(aws_lc_rs::default_provider());

  // 1. build backends, and make it contained in Arc
  let app_manager = Arc::new(backend::BackendAppManager::try_from(app_config_list)?);

  // 2. build global shared context
  let globals = Arc::new(Globals {
    proxy_config: proxy_config.clone(),
    request_count: Default::default(),
    runtime_handle: runtime_handle.clone(),
    cancel_token: cancel_token.clone(),
    cert_reloader_rx: cert_rx.clone(),

    #[cfg(feature = "acme")]
    server_configs_acme_challenge: server_configs_acme_challenge.clone(),
  });

  // 3. build message handler containing Arc-ed http_client and backends, and make it contained in Arc as well
  let forwarder = Arc::new(Forwarder::try_new(&globals).await?);
  let message_handler = Arc::new(
    HttpMessageHandlerBuilder::default()
      .globals(globals.clone())
      .app_manager(app_manager.clone())
      .forwarder(forwarder)
      .build()?,
  );

  // 4. spawn each proxy for a given socket with copied Arc-ed message_handler.
  // build hyper connection builder shared with proxy instances
  let connection_builder = proxy::connection_builder(&globals);

  // spawn each proxy for a given socket with copied Arc-ed backend, message_handler and connection builder.
  let addresses = globals.proxy_config.listen_sockets.clone();
  let join_handles = addresses.into_iter().map(|listening_on| {
    let mut tls_enabled = false;
    if let Some(https_port) = globals.proxy_config.https_port {
      tls_enabled = https_port == listening_on.port()
    }
    let proxy = Proxy {
      globals: globals.clone(),
      listening_on,
      tls_enabled,
      connection_builder: connection_builder.clone(),
      message_handler: message_handler.clone(),
    };

    let cancel_token = globals.cancel_token.as_ref().map(|t| t.child_token());
    let parent_cancel_token_clone = globals.cancel_token.clone();
    globals.runtime_handle.spawn(async move {
      info!("rpxy proxy service for {listening_on} started");
      if let Some(cancel_token) = cancel_token {
        tokio::select! {
          _ = cancel_token.cancelled() => {
            debug!("rpxy proxy service for {listening_on} terminated");
            Ok(())
          },
          proxy_res = proxy.start() => {
            info!("rpxy proxy service for {listening_on} exited");
            // cancel other proxy tasks
            parent_cancel_token_clone.unwrap().cancel();
            proxy_res
          }
        }
      } else {
        proxy.start().await
      }
    })
  });

  let join_res = join_all(join_handles).await;
  let mut errs = join_res.into_iter().filter_map(|res| {
    if let Ok(Err(e)) = res {
      error!("Some proxy services are down: {}", e);
      Some(e)
    } else {
      None
    }
  });
  // returns the first error as the representative error
  if let Some(e) = errs.next() {
    return Err(e);
  }

  Ok(())
}
