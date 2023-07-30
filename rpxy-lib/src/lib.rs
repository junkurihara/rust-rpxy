mod backend;
mod certs;
mod constants;
mod error;
mod globals;
mod handler;
mod log;
mod proxy;
mod utils;

use crate::{error::*, globals::Globals, handler::HttpMessageHandlerBuilder, log::*, proxy::ProxyBuilder};
use futures::future::select_all;
use hyper::Client;
// use hyper_trust_dns::TrustDnsResolver;
use std::sync::Arc;

pub use crate::{
  certs::{CertsAndKeys, CryptoSource},
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
) -> Result<()>
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

  // build global
  let globals = Arc::new(Globals {
    proxy_config: proxy_config.clone(),
    backends: app_config_list.clone().try_into()?,
    request_count: Default::default(),
    runtime_handle: runtime_handle.clone(),
  });
  // let connector = TrustDnsResolver::default().into_rustls_webpki_https_connector();
  let connector = hyper_rustls::HttpsConnectorBuilder::new()
    .with_webpki_roots()
    .https_or_http()
    .enable_http1()
    .enable_http2()
    .build();

  let msg_handler = HttpMessageHandlerBuilder::default()
    .forwarder(Arc::new(Client::builder().build::<_, hyper::Body>(connector)))
    .globals(globals.clone())
    .build()?;

  let addresses = globals.proxy_config.listen_sockets.clone();
  let futures = select_all(addresses.into_iter().map(|addr| {
    let mut tls_enabled = false;
    if let Some(https_port) = globals.proxy_config.https_port {
      tls_enabled = https_port == addr.port()
    }

    let proxy = ProxyBuilder::default()
      .globals(globals.clone())
      .listening_on(addr)
      .tls_enabled(tls_enabled)
      .msg_handler(msg_handler.clone())
      .build()
      .unwrap();

    globals.runtime_handle.spawn(proxy.start())
  }));

  // wait for all future
  if let (Ok(Err(e)), _, _) = futures.await {
    error!("Some proxy services are down: {:?}", e);
  };

  Ok(())
}
