pub use tracing::{debug, error, info, warn};

pub fn init_logger() {
  use tracing_subscriber::{fmt, prelude::*, EnvFilter};

  let format_layer = fmt::layer()
    .with_line_number(false)
    .with_thread_ids(false)
    .with_target(false)
    .with_thread_names(true)
    .with_target(true)
    .with_level(true)
    .compact();

  // This limits the logger to emits only proxy crate
  let pkg_name = env!("CARGO_PKG_NAME").replace('-', "_");
  // let level_string = std::env::var(EnvFilter::DEFAULT_ENV).unwrap_or_else(|_| "info".to_string());
  // let filter_layer = EnvFilter::new(format!("{}={}", pkg_name, level_string));
  let filter_layer = EnvFilter::try_from_default_env()
    .unwrap_or_else(|_| EnvFilter::new("info"))
    .add_directive(format!("{}=trace", pkg_name).parse().unwrap());

  tracing_subscriber::registry()
    .with(format_layer)
    .with(filter_layer)
    .init();
}
