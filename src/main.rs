use mimalloc_rust::*;
#[global_allocator]
static GLOBAL_MIMALLOC: GlobalMiMalloc = GlobalMiMalloc;

mod backend;
mod backend_opt;
mod config;
mod constants;
mod error;
mod globals;
mod log;
mod msg_handler;
mod proxy;
mod utils;

use crate::{
  backend::{Backend, Backends, ServerNameLC},
  config::parse_opts,
  constants::*,
  error::*,
  globals::*,
  log::*,
  proxy::Proxy,
};
use futures::future::select_all;
use hyper::Client;
use msg_handler::HttpMessageHandler;
// use hyper_trust_dns::TrustDnsResolver;
use rustc_hash::FxHashMap as HashMap;
use std::{io::Write, sync::Arc};
use tokio::time::Duration;

fn main() {
  // env::set_var("RUST_LOG", "info");
  env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
    .format(|buf, record| {
      let ts = buf.timestamp();
      writeln!(
        buf,
        "{} [{}] {}",
        ts,
        record.level(),
        // record.target(),
        record.args(),
        // record.file().unwrap_or("unknown"),
        // record.line().unwrap_or(0),
      )
    })
    .init();
  info!("Start http (reverse) proxy");

  let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
  runtime_builder.enable_all();
  runtime_builder.thread_name("rpxy");
  let runtime = runtime_builder.build().unwrap();

  runtime.block_on(async {
    let mut globals = Globals {
      listen_sockets: Vec::new(),
      http_port: None,
      https_port: None,
      http3: false,

      // TODO: Reconsider each timeout values
      proxy_timeout: Duration::from_secs(PROXY_TIMEOUT_SEC),
      upstream_timeout: Duration::from_secs(UPSTREAM_TIMEOUT_SEC),

      max_clients: MAX_CLIENTS,
      clients_count: Default::default(),
      max_concurrent_streams: MAX_CONCURRENT_STREAMS,
      keepalive: true,
      runtime_handle: runtime.handle().clone(),

      backends: Backends {
        default_server_name: None,
        apps: HashMap::<ServerNameLC, Backend>::default(),
      },
    };

    parse_opts(&mut globals).expect("Invalid configuration");

    entrypoint(Arc::new(globals)).await.unwrap()
  });
  warn!("Exit the program");
}

// entrypoint creates and spawns tasks of proxy services
async fn entrypoint(globals: Arc<Globals>) -> Result<()> {
  // let connector = TrustDnsResolver::default().into_rustls_webpki_https_connector();
  let connector = hyper_rustls::HttpsConnectorBuilder::new()
    .with_webpki_roots()
    .https_or_http()
    .enable_http1()
    .enable_http2()
    .build();
  let msg_handler = HttpMessageHandler {
    forwarder: Arc::new(Client::builder().build::<_, hyper::Body>(connector)),
    globals: globals.clone(),
  };

  let addresses = globals.listen_sockets.clone();
  let futures = select_all(addresses.into_iter().map(|addr| {
    let mut tls_enabled = false;
    if let Some(https_port) = globals.https_port {
      tls_enabled = https_port == (addr.port() as u16)
    }

    let proxy = Proxy {
      globals: globals.clone(),
      listening_on: addr,
      tls_enabled,
      msg_handler: msg_handler.clone(),
    };
    globals.runtime_handle.spawn(proxy.start())
  }));

  // wait for all future
  if let (Ok(_), _, _) = futures.await {
    error!("Some proxy services are down");
  };

  Ok(())
}
