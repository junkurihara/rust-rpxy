#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod backend;
mod config;
mod constants;
mod error;
mod globals;
mod log;
mod proxy;

use crate::{
  backend::Backend, config::parse_opts, constants::*, error::*, globals::*, log::*, proxy::Proxy,
};
use futures::future::select_all;
use hyper::Client;
#[cfg(feature = "forward-hyper-trust-dns")]
use hyper_trust_dns::TrustDnsResolver;
use std::{collections::HashMap, io::Write, sync::Arc};
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
  runtime_builder.thread_name("rust-rpxy");
  let runtime = runtime_builder.build().unwrap();

  runtime.block_on(async {
    let mut globals = Globals {
      listen_sockets: Vec::new(),
      http_port: None,
      https_port: None,
      timeout: Duration::from_secs(TIMEOUT_SEC),
      max_clients: MAX_CLIENTS,
      clients_count: Default::default(),
      max_concurrent_streams: MAX_CONCURRENT_STREAMS,
      keepalive: true,
      runtime_handle: runtime.handle().clone(),
    };

    let mut backends: HashMap<String, Backend> = HashMap::new();

    parse_opts(&mut globals, &mut backends);

    entrypoint(Arc::new(globals), Arc::new(backends))
      .await
      .unwrap()
  });
  warn!("Exit the program");
}

// entrypoint creates and spawns tasks of proxy services
async fn entrypoint(globals: Arc<Globals>, backends: Arc<HashMap<String, Backend>>) -> Result<()> {
  #[cfg(feature = "forward-hyper-trust-dns")]
  let connector = TrustDnsResolver::default().into_rustls_webpki_https_connector();
  #[cfg(not(feature = "forward-hyper-trust-dns"))]
  let connector = hyper_tls::HttpsConnector::new();
  let forwarder = Arc::new(Client::builder().build::<_, hyper::Body>(connector));

  let addresses = globals.listen_sockets.clone();
  let futures = select_all(addresses.into_iter().map(|addr| {
    let mut tls_enabled = false;
    if let Some(https_port) = globals.https_port {
      tls_enabled = https_port == (addr.port() as u32)
    }

    info!("Listen address: {:?} (TLS = {})", addr, tls_enabled);

    let proxy = Proxy {
      globals: globals.clone(),
      listening_on: addr,
      tls_enabled,
      backends: backends.clone(),
      forwarder: forwarder.clone(),
    };
    globals.runtime_handle.spawn(proxy.start())
  }));

  // wait for all future
  if let (Ok(_), _, _) = futures.await {
    error!("Some proxy services are down");
  };

  Ok(())
}
