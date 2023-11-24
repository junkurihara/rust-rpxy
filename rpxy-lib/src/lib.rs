mod backend;
mod constants;
mod count;
mod crypto;
mod error;
mod globals;
mod hyper_executor;
mod log;
mod name_exp;
mod proxy;

use crate::{crypto::build_cert_reloader, error::*, globals::Globals, log::*, proxy::Proxy};
use futures::future::select_all;
use std::sync::Arc;

pub use crate::{
  crypto::{CertsAndKeys, CryptoSource},
  globals::{AppConfig, AppConfigList, ProxyConfig, ReverseProxyConfig, TlsConfig, UpstreamUri},
};
pub mod reexports {
  pub use hyper::Uri;
  pub use rustls::{Certificate, PrivateKey};
}

#[cfg(all(feature = "http3-quinn", feature = "http3-s2n"))]
compile_error!("feature \"http3-quinn\" and feature \"http3-s2n\" cannot be enabled at the same time");

/// Entrypoint that creates and spawns tasks of reverse proxy services
pub async fn entrypoint<T>(
  proxy_config: &ProxyConfig,
  app_config_list: &AppConfigList<T>,
  runtime_handle: &tokio::runtime::Handle,
  term_notify: Option<Arc<tokio::sync::Notify>>,
) -> RpxyResult<()>
where
  T: CryptoSource + Clone + Send + Sync + 'static,
{
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
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  if proxy_config.http3 {
    info!("Experimental HTTP/3.0 is enabled. Note it is still very unstable.");
  }
  if !proxy_config.sni_consistency {
    info!("Ignore consistency between TLS SNI and Host header (or Request line). Note it violates RFC.");
  }
  #[cfg(feature = "cache")]
  if proxy_config.cache_enabled {
    info!(
      "Cache is enabled: cache dir = {:?}",
      proxy_config.cache_dir.as_ref().unwrap()
    );
  } else {
    info!("Cache is disabled")
  }

  // 1. build backends, and make it contained in Arc
  let app_manager = Arc::new(backend::BackendAppManager::try_from(app_config_list)?);

  // 2. build crypto reloader service
  let (cert_reloader_service, cert_reloader_rx) = match proxy_config.https_port {
    Some(_) => {
      let (s, r) = build_cert_reloader(&app_manager).await?;
      (Some(s), Some(r))
    }
    None => (None, None),
  };

  // 3. build global shared context
  let globals = Arc::new(Globals {
    proxy_config: proxy_config.clone(),
    request_count: Default::default(),
    runtime_handle: runtime_handle.clone(),
    term_notify: term_notify.clone(),
    cert_reloader_rx: cert_reloader_rx.clone(),
  });

  // TODO: 2. build message handler with Arc-ed http_client and backends, and make it contained in Arc as well
  // // build message handler including a request forwarder
  // let msg_handler = Arc::new(
  //   HttpMessageHandlerBuilder::default()
  //     // .forwarder(Arc::new(Forwarder::new(&globals).await))
  //     .globals(globals.clone())
  //     .build()?,
  // );

  // TODO: 3. spawn each proxy for a given socket with copied Arc-ed message_handler.
  // build hyper connection builder shared with proxy instances
  let connection_builder = proxy::connection_builder(&globals);

  // spawn each proxy for a given socket with copied Arc-ed backend, message_handler and connection builder.
  let addresses = globals.proxy_config.listen_sockets.clone();
  let futures_iter = addresses.into_iter().map(|listening_on| {
    let mut tls_enabled = false;
    if let Some(https_port) = globals.proxy_config.https_port {
      tls_enabled = https_port == listening_on.port()
    }
    let proxy = Proxy {
      globals: globals.clone(),
      listening_on,
      tls_enabled,
      connection_builder: connection_builder.clone(),
      // TODO: message_handler
    };
    globals.runtime_handle.spawn(async move { proxy.start().await })
  });

  // wait for all future
  match cert_reloader_service {
    Some(cert_service) => {
      tokio::select! {
        _ = cert_service.start() => {
          error!("Certificate reloader service got down");
        }
        _ = select_all(futures_iter) => {
          error!("Some proxy services are down");
        }
      }
    }
    None => {
      if let (Ok(Err(e)), _, _) = select_all(futures_iter).await {
        error!("Some proxy services are down: {}", e);
      }
    }
  }

  Ok(())
}
