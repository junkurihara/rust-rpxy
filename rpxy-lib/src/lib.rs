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
  proxy::{ListenerKind, ListenerSpecBuilder, ProxyBuilder},
};
use futures::future::join_all;
use hot_reload::ReloaderReceiver;
use rpxy_certs::ServerCryptoBase;
use rustls::crypto::CryptoProvider;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/* ------------------------------------------------ */
pub use crate::{
  constants::log_event_names,
  globals::{AppConfig, AppConfigList, ProxyConfig, ReverseProxyConfig, TlsConfig, UpstreamUri},
};

#[cfg(feature = "health-check")]
pub const LOAD_BALANCE_PRIMARY_BACKUP: &str = crate::backend::LOAD_BALANCE_PRIMARY_BACKUP;

#[cfg(feature = "health-check")]
pub use crate::{
  constants::health_check as health_check_defaults,
  globals::{HealthCheckConfig, HealthCheckType},
};
#[cfg(feature = "proxy-protocol")]
pub use crate::{constants::proxy_protocol as proxy_protocol_defaults, globals::TcpRecvProxyProtocolConfig};

pub mod reexports {
  pub use hyper::Uri;
  #[cfg(feature = "proxy-protocol")]
  pub use ipnet::IpNet;
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

  #[cfg(feature = "acme")]
  /// ServerConfig used for only ACME challenge for ACME domains
  pub server_configs_acme_challenge: Arc<ahash::HashMap<String, Arc<rustls::ServerConfig>>>,
}

/// Entrypoint that creates and spawns tasks of reverse proxy services
pub async fn entrypoint(
  RpxyOptions {
    proxy_config,
    app_config_list,
    cert_rx, // TODO:
    runtime_handle,
    #[cfg(feature = "acme")]
    server_configs_acme_challenge,
  }: &RpxyOptions,
  cancel_token: CancellationToken,
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
  #[cfg(feature = "proxy-protocol")]
  if let Some(ref pp_config) = proxy_config.tcp_recv_proxy_protocol {
    info!(
      "PROXY Protocol is enabled for trusted proxies: {:?}",
      pp_config.trusted_proxies
    );
    warn!(
      "PROXY Protocol is enabled. Ensure that ALL TCP connections originate from a listed trusted proxy, as PROXY headers are not authenticated."
    );
    warn!(
      "All incoming TCP (i.e., HTTP/1.1 and HTTP/2) connections are expected to include a valid PROXY header. Connections without a valid PROXY header will be rejected. Configure your upstream L4 proxy (e.g., rpxy-l4) to send PROXY headers accordingly."
    );
    warn!(
      "Note that even if PROXY Protocol is enabled, HTTP/3 connections are not affected and will work without PROXY headers, as PROXY Protocol is only for TCP-based protocols. HTTP/3 connections will still be accepted without PROXY headers regardless of this setting."
    );
  }

  #[cfg(not(feature = "post-quantum"))]
  // Install aws_lc_rs as default crypto provider for rustls
  let _ = CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider());
  #[cfg(feature = "post-quantum")]
  let _ = CryptoProvider::install_default(rustls_post_quantum::provider());
  #[cfg(feature = "post-quantum")]
  info!("Post-quantum crypto provider is installed");

  // 1. build backends, and make it contained in Arc
  let app_manager = Arc::new(backend::BackendAppManager::try_from(app_config_list)?);

  // 2. build global shared context
  let globals = Arc::new(Globals {
    proxy_config: proxy_config.clone(),
    request_count: Default::default(),
    runtime_handle: runtime_handle.clone(),
    cert_reloader_rx: cert_rx.clone(),

    #[cfg(feature = "acme")]
    server_configs_acme_challenge: server_configs_acme_challenge.clone(),
  });

  // 3. build forwarder
  let forwarder = Arc::new(Forwarder::try_new(&globals).await?);

  // 4. build message handler containing Arc-ed http_client and backends, and make it contained in Arc as well
  let message_handler = Arc::new(
    HttpMessageHandlerBuilder::default()
      .globals(globals.clone())
      .app_manager(app_manager.clone())
      .forwarder(forwarder)
      .build()?,
  );

  // 5. spawn each proxy for a given socket with copied Arc-ed message_handler.
  // build hyper connection builder shared with proxy instances
  let connection_builder = proxy::connection_builder(&globals);

  // spawn each proxy for a given socket with copied Arc-ed backend, message_handler and connection builder.
  let addresses = globals.proxy_config.listen_sockets.clone();
  let mut listener_specs = Vec::new();
  addresses.into_iter().for_each(|listening_on| {
    let mut tls_enabled = false;
    if let Some(https_port) = globals.proxy_config.https_port {
      tls_enabled = https_port == listening_on.port()
    }
    let kind = match (tls_enabled, listening_on.is_ipv4()) {
      (false, true) => ListenerKind::HttpV4,
      (false, false) => ListenerKind::HttpV6,
      (true, true) => ListenerKind::HttpsV4,
      (true, false) => ListenerKind::HttpsV6,
    };
    let listener_spec = ListenerSpecBuilder::default().kind(kind).listening_on(listening_on).build();
    listener_specs.push(listener_spec);

    #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
    if tls_enabled && globals.proxy_config.http3 {
      let kind = if listening_on.is_ipv4() {
        ListenerKind::Http3V4
      } else {
        ListenerKind::Http3V6
      };
      let listener_spec_h3 = ListenerSpecBuilder::default().kind(kind).listening_on(listening_on).build();
      listener_specs.push(listener_spec_h3);
    }
  });
  let listener_specs = listener_specs.into_iter().collect::<Result<Vec<_>, _>>()?;

  let proxies = listener_specs
    .into_iter()
    .map(|listener_spec| {
      ProxyBuilder::default()
        .globals(globals.clone())
        .listener_spec(listener_spec)
        .connection_builder(connection_builder.clone())
        .message_handler(message_handler.clone())
        .build()
    })
    .collect::<Result<Vec<_>, _>>()?;

  let proxy_handles = proxies.into_iter().map(|proxy| {
    let cancel_token = cancel_token.clone();
    globals.runtime_handle.spawn(async move {
      info!("rpxy proxy service for {} started", proxy.listener_spec);

      tokio::select! {
        _ = cancel_token.cancelled() => {
          debug!("rpxy proxy service for {} terminated", proxy.listener_spec);
          Ok(())
        },
        proxy_res = proxy.start(cancel_token.child_token()) => {
          info!("rpxy proxy service for {} exited", proxy.listener_spec);
          // cancel other proxy tasks
          cancel_token.cancel();
          proxy_res
        }
      }
    })
  });

  #[cfg(feature = "health-check")]
  // 6. spawn health checker tasks (after globals/forwarder, before proxy tasks)
  let handles = {
    let health_checker_handles =
      backend::health_check::spawn_health_checkers(&app_manager, cancel_token.clone(), &globals.runtime_handle)?;
    health_checker_handles.into_iter().chain(proxy_handles.into_iter())
  };
  #[cfg(not(feature = "health-check"))]
  let handles = proxy_handles;

  // 7. wait for all proxy tasks to finish, and return the first error if exists
  let join_res = join_all(handles).await;
  let mut errs = join_res.into_iter().filter_map(|res| {
    if let Ok(Err(e)) = res {
      error!("Some proxy services or health checks are down: {}", e);
      Some(e)
    } else {
      None
    }
  });
  // returns the first error as the representative error
  errs.next().map_or(Ok(()), |e| Err(e))
}
