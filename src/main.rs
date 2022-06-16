#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod acceptor;
mod config;
mod constants;
mod error;
mod globals;
mod log;
mod proxy;
#[cfg(feature = "tls")]
mod tls;

use crate::{config::parse_opts, constants::*, globals::Globals, log::*, proxy::Proxy};
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
  runtime_builder.thread_name("rust-rpxy");
  let runtime = runtime_builder.build().unwrap();

  // TODO:
  let listen_addresses: Vec<std::net::SocketAddr> = LISTEN_ADDRESSES
    .to_vec()
    .iter()
    .map(|x| x.parse().unwrap())
    .collect();

  runtime.block_on(async {
    let mut globals = Globals {
      listen_addresses,
      timeout: Duration::from_secs(TIMEOUT_SEC),
      max_clients: MAX_CLIENTS,
      clients_count: Default::default(),
      max_concurrent_streams: MAX_CONCURRENT_STREAMS,
      keepalive: true,
      runtime_handle: runtime.handle().clone(),

      #[cfg(feature = "tls")]
      tls_cert_path: None,
      #[cfg(feature = "tls")]
      tls_cert_key_path: None,
    };

    parse_opts(&mut globals);

    let proxy = Proxy {
      globals: Arc::new(globals),
    };
    proxy.entrypoint().await.unwrap()
  });
  warn!("Exit the program");
}
