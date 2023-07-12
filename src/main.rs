use certs::CryptoSource;
#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod backend;
mod cert_file_reader;
mod certs;
mod config;
mod constants;
mod error;
mod globals;
mod handler;
mod log;
mod proxy;
mod utils;

use crate::{
  cert_file_reader::CryptoFileSource, config::build_globals, error::*, globals::*, handler::HttpMessageHandlerBuilder,
  log::*, proxy::ProxyBuilder,
};
use futures::future::select_all;
use hyper::Client;
// use hyper_trust_dns::TrustDnsResolver;
use std::sync::Arc;

fn main() {
  init_logger();

  let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
  runtime_builder.enable_all();
  runtime_builder.thread_name("rpxy");
  let runtime = runtime_builder.build().unwrap();

  runtime.block_on(async {
    let globals: Globals<CryptoFileSource> = match build_globals(runtime.handle().clone()) {
      Ok(g) => g,
      Err(e) => {
        error!("Invalid configuration: {}", e);
        std::process::exit(1);
      }
    };

    entrypoint(Arc::new(globals)).await.unwrap()
  });
  warn!("rpxy exited!");
}

// entrypoint creates and spawns tasks of proxy services
async fn entrypoint<T>(globals: Arc<Globals<T>>) -> Result<()>
where
  T: CryptoSource + Clone + Send + Sync + 'static,
{
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
  if let (Ok(_), _, _) = futures.await {
    error!("Some proxy services are down");
  };

  Ok(())
}
