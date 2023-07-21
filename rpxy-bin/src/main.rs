#[cfg(not(target_env = "msvc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod cert_file_reader;
mod config;
mod constants;
mod error;
mod log;

use crate::{cert_file_reader::CryptoFileSource, config::build_globals, log::*};
use rpxy_lib::{entrypoint, Globals};
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
