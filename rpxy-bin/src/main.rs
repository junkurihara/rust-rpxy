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

use crate::{config::build_settings, log::*};
use rpxy_lib::entrypoint;

fn main() {
  init_logger();

  let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
  runtime_builder.enable_all();
  runtime_builder.thread_name("rpxy");
  let runtime = runtime_builder.build().unwrap();

  runtime.block_on(async {
    let (proxy_conf, app_conf) = match build_settings() {
      Ok(g) => g,
      Err(e) => {
        error!("Invalid configuration: {}", e);
        std::process::exit(1);
      }
    };

    entrypoint(proxy_conf, app_conf, runtime.handle().clone())
      .await
      .unwrap()
  });
  warn!("rpxy exited!");
}
