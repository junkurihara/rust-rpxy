use std::str::FromStr;
use tracing_subscriber::{fmt, prelude::*};

#[allow(unused)]
pub use tracing::{debug, error, info, warn};

/// Initialize the logger with the RUST_LOG environment variable.
pub fn init_logger(log_dir_path: Option<&str>) {
  let level_string = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
  let level = tracing::Level::from_str(level_string.as_str()).unwrap_or(tracing::Level::INFO);

  match log_dir_path {
    None => {
      // log to stdout
      init_stdio_logger(level);
    }
    Some(log_dir_path) => {
      // log to files
      println!("Activate logging to files: {log_dir_path}");
      init_file_logger(level, log_dir_path);
    }
  }
}

/// file logging
fn init_file_logger(level: tracing::Level, log_dir_path: &str) {
  // TODO: implement
  init_stdio_logger(level);
}

/// stdio logging
fn init_stdio_logger(level: tracing::Level) {
  // This limits the logger to emits only this crate with any level above RUST_LOG, for included crates it will emit only ERROR (in prod)/INFO (in dev) or above level.
  let stdio_layer = fmt::layer().with_level(true).with_thread_ids(false);
  if level <= tracing::Level::INFO {
    // in normal deployment environment
    let stdio_layer = stdio_layer
      .with_target(false)
      .compact()
      .with_filter(tracing_subscriber::filter::filter_fn(move |metadata| {
        (metadata
          .target()
          .starts_with(env!("CARGO_PKG_NAME").replace('-', "_").as_str())
          && metadata.level() <= &level)
          || metadata.level() <= &tracing::Level::WARN.min(level)
      }));
    tracing_subscriber::registry().with(stdio_layer).init();
  } else {
    // debugging
    let stdio_layer = stdio_layer
      .with_line_number(true)
      .with_target(true)
      .with_thread_names(true)
      .with_target(true)
      .compact()
      .with_filter(tracing_subscriber::filter::filter_fn(move |metadata| {
        (metadata
          .target()
          .starts_with(env!("CARGO_PKG_NAME").replace('-', "_").as_str())
          && metadata.level() <= &level)
          || metadata.level() <= &tracing::Level::INFO.min(level)
      }));
    tracing_subscriber::registry().with(stdio_layer).init();
  };
}

#[inline]
/// Create a file for logging
fn open_log_file(path: &str) -> std::fs::File {
  // crate a file if it does not exist
  std::fs::OpenOptions::new()
    .create(true)
    .append(true)
    .open(path)
    .expect("Failed to open the log file")
}
