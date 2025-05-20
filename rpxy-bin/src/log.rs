use crate::constants::{ACCESS_LOG_FILE, SYSTEM_LOG_FILE};
use rpxy_lib::log_event_names;
use std::str::FromStr;
use tracing_subscriber::{filter::filter_fn, fmt, prelude::*};

#[allow(unused)]
pub use tracing::{debug, error, info, warn};

/// Initialize the logger with the RUST_LOG environment variable.
pub fn init_logger(log_dir_path: Option<&str>) {
  let level = std::env::var("RUST_LOG")
    .ok()
    .and_then(|s| tracing::Level::from_str(&s).ok())
    .unwrap_or(tracing::Level::INFO);

  match log_dir_path {
    None => init_stdio_logger(level),
    Some(path) => init_file_logger(level, path),
  }
}

/// file logging
fn init_file_logger(level: tracing::Level, log_dir_path: &str) {
  println!("Activate logging to files: {}", log_dir_path);
  let log_dir = std::path::Path::new(log_dir_path);

  if !log_dir.exists() {
    println!("Directory does not exist, creating: {}", log_dir.display());
    std::fs::create_dir_all(log_dir).expect("Failed to create log directory");
  }

  let access_log_path = log_dir.join(ACCESS_LOG_FILE);
  let system_log_path = log_dir.join(SYSTEM_LOG_FILE);

  println!("Access log: {}", access_log_path.display());
  println!("System and error log: {}", system_log_path.display());

  let access_log = open_log_file(&access_log_path);
  let system_log = open_log_file(&system_log_path);

  let access_layer = fmt::layer()
    .with_line_number(false)
    .with_thread_ids(false)
    .with_thread_names(false)
    .with_target(false)
    .with_level(false)
    .compact()
    .with_ansi(false)
    .with_writer(access_log)
    .with_filter(AccessLogFilter);

  let system_layer = fmt::layer()
    .with_line_number(false)
    .with_thread_ids(false)
    .with_thread_names(false)
    .with_target(false)
    .with_level(true)
    .compact()
    .with_ansi(false)
    .with_writer(system_log)
    .with_filter(filter_fn(move |metadata| {
      (is_cargo_pkg(metadata) && metadata.name() != log_event_names::ACCESS_LOG && metadata.level() <= &level)
        || metadata.level() <= &tracing::Level::WARN.min(level)
    }));

  tracing_subscriber::registry().with(access_layer).with(system_layer).init();
}

/// stdio logging
fn init_stdio_logger(level: tracing::Level) {
  // This limits the logger to emit only this crate with any level above RUST_LOG,
  // for included crates it will emit only ERROR (in prod)/INFO (in dev) or above level.
  let base_layer = fmt::layer().with_level(true).with_thread_ids(false);

  let debug = level > tracing::Level::INFO;
  let filter = filter_fn(move |metadata| {
    if debug {
      (is_cargo_pkg(metadata) && metadata.level() <= &level) || metadata.level() <= &tracing::Level::INFO.min(level)
    } else {
      (is_cargo_pkg(metadata) && metadata.level() <= &level) || metadata.level() <= &tracing::Level::WARN.min(level)
    }
  });

  let stdio_layer = if debug {
    base_layer
      .with_line_number(true)
      .with_target(true)
      .with_thread_names(true)
      .with_target(true)
      .compact()
      .with_filter(filter)
  } else {
    base_layer.with_target(false).compact().with_filter(filter)
  };

  tracing_subscriber::registry().with(stdio_layer).init();
}

/// Access log filter
struct AccessLogFilter;
impl<S> tracing_subscriber::layer::Filter<S> for AccessLogFilter {
  fn enabled(&self, metadata: &tracing::Metadata<'_>, _: &tracing_subscriber::layer::Context<'_, S>) -> bool {
    is_cargo_pkg(metadata) && metadata.name().contains(log_event_names::ACCESS_LOG) && metadata.level() <= &tracing::Level::INFO
  }
}

#[inline]
/// Create a file for logging
fn open_log_file<P>(path: P) -> std::fs::File
where
  P: AsRef<std::path::Path>,
{
  // create a file if it does not exist
  std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(path)
    .expect("Failed to open the log file")
}

#[inline]
/// Matches cargo package name with `_` instead of `-`
fn is_cargo_pkg(metadata: &tracing::Metadata<'_>) -> bool {
  let pkg_name = env!("CARGO_PKG_NAME").replace('-', "_");
  metadata.target().starts_with(&pkg_name)
}
