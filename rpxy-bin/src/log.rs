use crate::constants::{ACCESS_LOG_FILE, SYSTEM_LOG_FILE};
use rpxy_lib::log_event_names;
use std::str::FromStr;
use tracing_subscriber::{filter::filter_fn, fmt, layer::Layer, prelude::*, registry::LookupSpan};

#[allow(unused)]
pub use tracing::{debug, error, info, warn};

/// Environment variable that disables credential-header redaction in DEBUG
/// request logs. Troubleshooting-only; see `unsafe_debug_headers_enabled`.
const UNSAFE_DEBUG_HEADERS_ENV: &str = "RPXY_UNSAFE_DEBUG_HEADERS";

/// Strict parse of the `RPXY_UNSAFE_DEBUG_HEADERS` opt-out value. Only `1`,
/// `true`, or `yes` (case-insensitive, trimmed) enable it; every other value -
/// including typos and `0` / `false` - is treated as disabled, so the fail-safe
/// posture is always toward redaction.
fn parse_unsafe_debug_headers(value: Option<&str>) -> bool {
  value
    .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
    .unwrap_or(false)
}

/// Read the `RPXY_UNSAFE_DEBUG_HEADERS` opt-out once at startup. When enabled,
/// emit a single `warn!` so that even at `RUST_LOG=info` an operator sees that
/// credential redaction is disabled (the header dump itself still only appears
/// at `RUST_LOG=debug`). Not hot-reloaded: read once and threaded into rpxy-lib.
pub fn unsafe_debug_headers_enabled() -> bool {
  let enabled = parse_unsafe_debug_headers(std::env::var(UNSAFE_DEBUG_HEADERS_ENV).ok().as_deref());
  if enabled {
    warn!(
      "unsafe debug header logging enabled via {UNSAFE_DEBUG_HEADERS_ENV}: Authorization / Cookie / Proxy-Authorization values will be printed verbatim at RUST_LOG=debug. Do not leave this enabled in production."
    );
  }
  enabled
}

/// Pure parse of a `RUST_LOG` value. `None` and unparsable values - including
/// the `name=level` directive form, which `tracing::Level` does not parse -
/// fall back to INFO. Split from the environment read so tests never mutate
/// process state (same pattern as `parse_unsafe_debug_headers`).
fn parse_log_level(value: Option<&str>) -> tracing::Level {
  value
    .and_then(|s| tracing::Level::from_str(s).ok())
    .unwrap_or(tracing::Level::INFO)
}

/// Resolved `RUST_LOG` level. Shared by `init_logger` and `access_log_enabled`
/// so the installed filters and the predicate can never disagree.
fn resolve_log_level() -> tracing::Level {
  parse_log_level(std::env::var("RUST_LOG").ok().as_deref())
}

/// Pure predicate behind `access_log_enabled`, split out for tests.
/// Must stay in lockstep with the filters installed by `init_logger`: in file
/// mode, `AccessLogFilter` passes access events regardless of `RUST_LOG`; in
/// stdio mode, the level filter in `stdio_layer` passes the INFO-level access
/// event iff the resolved level admits INFO (see `predicate_matches_*` tests).
fn access_log_enabled_for_level(log_dir_path: Option<&str>, level: tracing::Level) -> bool {
  match log_dir_path {
    Some(_) => true,
    None => tracing::Level::INFO <= level,
  }
}

/// Whether the logger installed by `init_logger(log_dir_path)` will emit
/// access-log lines. Read once at startup and threaded into rpxy-lib (via
/// `RpxyOptions`) so that per-request access-log data is not even constructed
/// when no line would be emitted. Not hot-reloaded, like the logger itself.
pub fn access_log_enabled(log_dir_path: Option<&str>) -> bool {
  access_log_enabled_for_level(log_dir_path, resolve_log_level())
}

/// Initialize the logger with the RUST_LOG environment variable.
pub fn init_logger(log_dir_path: Option<&str>) {
  let level = resolve_log_level();

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

  tracing_subscriber::registry()
    .with(access_layer(access_log))
    .with(system_layer(system_log, level))
    .init();
}

/// stdio logging
fn init_stdio_logger(level: tracing::Level) {
  tracing_subscriber::registry().with(stdio_layer(std::io::stdout, level)).init();
}

/// Build the file-mode access-log layer over `writer`: the dedicated minimal
/// formatter (`AccessLogFormat`) behind `AccessLogFilter`. Factored out of
/// `init_file_logger` so tests can run the production layer over an in-memory
/// writer.
fn access_layer<S, W>(writer: W) -> impl Layer<S>
where
  S: tracing::Subscriber + for<'a> LookupSpan<'a>,
  W: for<'w> fmt::MakeWriter<'w> + Send + Sync + 'static,
{
  fmt::layer()
    .event_format(AccessLogFormat::default())
    .with_writer(writer)
    .with_filter(AccessLogFilter)
}

/// Build the file-mode system-log layer over `writer`. Factored out of
/// `init_file_logger` for symmetry with `access_layer`.
fn system_layer<S, W>(writer: W, level: tracing::Level) -> impl Layer<S>
where
  S: tracing::Subscriber + for<'a> LookupSpan<'a>,
  W: for<'w> fmt::MakeWriter<'w> + Send + Sync + 'static,
{
  fmt::layer()
    .with_line_number(false)
    .with_thread_ids(false)
    .with_thread_names(false)
    .with_target(false)
    .with_level(true)
    .compact()
    .with_ansi(false)
    .with_writer(writer)
    .with_filter(filter_fn(move |metadata| {
      (is_cargo_pkg(metadata) && metadata.name() != log_event_names::ACCESS_LOG && metadata.level() <= &level)
        || metadata.level() <= &tracing::Level::WARN.min(level)
    }))
}

/// Build the stdio layer (system and access lines share it) over `writer`.
/// Factored out of `init_stdio_logger` so tests can run the production filter
/// over an in-memory writer.
fn stdio_layer<S, W>(writer: W, level: tracing::Level) -> impl Layer<S>
where
  S: tracing::Subscriber + for<'a> LookupSpan<'a>,
  W: for<'w> fmt::MakeWriter<'w> + Send + Sync + 'static,
{
  // This limits the logger to emit only this crate with any level above RUST_LOG,
  // for included crates it will emit only ERROR (in prod)/INFO (in dev) or above level.
  let base_layer = fmt::layer().with_level(true).with_thread_ids(false).with_writer(writer);

  let debug = level > tracing::Level::INFO;
  let filter = filter_fn(move |metadata| {
    if debug {
      (is_cargo_pkg(metadata) && metadata.level() <= &level) || metadata.level() <= &tracing::Level::INFO.min(level)
    } else {
      (is_cargo_pkg(metadata) && metadata.level() <= &level) || metadata.level() <= &tracing::Level::WARN.min(level)
    }
  });

  if debug {
    base_layer
      .with_line_number(true)
      .with_target(true)
      .with_thread_names(true)
      .with_target(true)
      .compact()
      .with_filter(filter)
  } else {
    base_layer.with_target(false).compact().with_filter(filter)
  }
}

/// Access log filter
struct AccessLogFilter;
impl<S> tracing_subscriber::layer::Filter<S> for AccessLogFilter {
  fn enabled(&self, metadata: &tracing::Metadata<'_>, _: &tracing_subscriber::layer::Context<'_, S>) -> bool {
    is_cargo_pkg(metadata) && metadata.name().contains(log_event_names::ACCESS_LOG) && metadata.level() <= &tracing::Level::INFO
  }
}

/// Minimal event formatter for the file access log: `{timestamp} {message}\n`.
///
/// The access event carries exactly one field (`message`, see
/// `HttpMessageLog::output()` in rpxy-lib), already rendered by a single
/// controlled `Display` implementation. The generic compact formatter would
/// re-process that string character by character through its ANSI-sanitizing
/// `EscapeGuard`; every byte that guard could transform (ESC / BEL / BS / FF /
/// DEL and C1 controls) is unreachable in the access line, because the
/// interpolated header values and URI components are restricted to visible
/// ASCII by `HeaderValue::to_str` / `http::Uri` parsing. A verbatim write is
/// therefore byte-identical to the compact output (pinned by the golden tests
/// below) at a fraction of the per-character cost.
struct AccessLogFormat<T = fmt::time::SystemTime> {
  timer: T,
}

impl Default for AccessLogFormat {
  fn default() -> Self {
    Self {
      timer: fmt::time::SystemTime,
    }
  }
}

impl<S, N, T> fmt::FormatEvent<S, N> for AccessLogFormat<T>
where
  S: tracing::Subscriber + for<'a> LookupSpan<'a>,
  N: for<'w> fmt::FormatFields<'w> + 'static,
  T: fmt::time::FormatTime,
{
  fn format_event(
    &self,
    _ctx: &fmt::FmtContext<'_, S, N>,
    mut writer: fmt::format::Writer<'_>,
    event: &tracing::Event<'_>,
  ) -> std::fmt::Result {
    // Same timestamp bytes and failure fallback as the generic formatter (ANSI disabled).
    if self.timer.format_time(&mut writer).is_err() {
      writer.write_str("<unknown time>")?;
    }
    writer.write_char(' ')?;
    let mut visitor = MessageVisitor {
      writer: &mut writer,
      result: Ok(()),
    };
    event.record(&mut visitor);
    visitor.result?;
    writeln!(writer)
  }
}

/// Field visitor that writes only the `message` field, verbatim.
struct MessageVisitor<'a, 'w> {
  writer: &'a mut fmt::format::Writer<'w>,
  result: std::fmt::Result,
}

impl tracing::field::Visit for MessageVisitor<'_, '_> {
  fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
    // The message of `info!("{}", ...)` arrives as `fmt::Arguments`, whose Debug
    // output is its rendered text; this is a single formatting pass, no escaping.
    if self.result.is_ok() && field.name() == "message" {
      self.result = write!(self.writer, "{:?}", value);
    }
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

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::{Arc, Mutex};
  use tracing::Level;

  #[test]
  fn enabled_values() {
    for v in ["1", "true", "yes", "TRUE", "Yes", " yes ", "True"] {
      assert!(parse_unsafe_debug_headers(Some(v)), "{v:?} should enable");
    }
  }

  #[test]
  fn disabled_values() {
    for v in ["0", "false", "no", "enabled", "", " ", "2", "on"] {
      assert!(!parse_unsafe_debug_headers(Some(v)), "{v:?} should not enable");
    }
  }

  #[test]
  fn unset_is_disabled() {
    assert!(!parse_unsafe_debug_headers(None));
  }

  #[test]
  fn parse_log_level_plain_levels() {
    assert_eq!(parse_log_level(Some("error")), Level::ERROR);
    assert_eq!(parse_log_level(Some("warn")), Level::WARN);
    assert_eq!(parse_log_level(Some("info")), Level::INFO);
    assert_eq!(parse_log_level(Some("debug")), Level::DEBUG);
    assert_eq!(parse_log_level(Some("trace")), Level::TRACE);
    assert_eq!(parse_log_level(Some("INFO")), Level::INFO);
  }

  #[test]
  fn parse_log_level_fallback_to_info() {
    // Unset, garbage, and `name=level` directives (not parsed by tracing::Level)
    // all fall back to INFO - the documented quirk.
    for v in [None, Some("verbose"), Some("rpxy=debug"), Some("")] {
      assert_eq!(parse_log_level(v), Level::INFO, "{v:?} should fall back to INFO");
    }
  }

  #[test]
  fn access_log_enabled_truth_table() {
    const ALL: [Level; 5] = [Level::TRACE, Level::DEBUG, Level::INFO, Level::WARN, Level::ERROR];
    // File mode: AccessLogFilter is independent of RUST_LOG; always enabled.
    for level in ALL {
      assert!(
        access_log_enabled_for_level(Some("/var/log/rpxy"), level),
        "file mode must always enable the access log (level {level})"
      );
    }
    // stdio mode: enabled iff the resolved level admits INFO.
    for (level, expected) in ALL.map(|l| (l, Level::INFO <= l)) {
      assert_eq!(
        access_log_enabled_for_level(None, level),
        expected,
        "stdio mode at level {level}"
      );
    }
    assert!(access_log_enabled_for_level(None, Level::INFO));
    assert!(!access_log_enabled_for_level(None, Level::WARN));
    assert!(!access_log_enabled_for_level(None, Level::ERROR));
  }

  /// In-memory `MakeWriter` for asserting on emitted bytes.
  #[derive(Clone, Default)]
  struct MemWriter {
    buf: Arc<Mutex<Vec<u8>>>,
  }

  impl MemWriter {
    fn contents(&self) -> String {
      String::from_utf8(self.buf.lock().unwrap().clone()).unwrap()
    }
  }

  struct MemGuard {
    buf: Arc<Mutex<Vec<u8>>>,
  }

  impl std::io::Write for MemGuard {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
      self.buf.lock().unwrap().extend_from_slice(b);
      Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
      Ok(())
    }
  }

  impl<'a> fmt::MakeWriter<'a> for MemWriter {
    type Writer = MemGuard;
    fn make_writer(&'a self) -> Self::Writer {
      MemGuard { buf: self.buf.clone() }
    }
  }

  /// Emit one event with the production access-log metadata (same `name:` and a
  /// `rpxy*` target) through the given subscriber, scoped to this thread.
  fn emit_access_probe<S: tracing::Subscriber + Send + Sync>(subscriber: S) {
    tracing::subscriber::with_default(subscriber, || {
      tracing::info!(name: log_event_names::ACCESS_LOG, "equivalence probe");
    });
  }

  /// The predicate must agree with the actual stdio filter for every level:
  /// this is the guard against silent access-log loss.
  #[test]
  fn predicate_matches_stdio_filter() {
    for level in [Level::TRACE, Level::DEBUG, Level::INFO, Level::WARN, Level::ERROR] {
      let writer = MemWriter::default();
      emit_access_probe(tracing_subscriber::registry().with(stdio_layer(writer.clone(), level)));
      let emitted = !writer.contents().is_empty();
      assert_eq!(
        emitted,
        access_log_enabled_for_level(None, level),
        "stdio filter and access_log_enabled disagree at level {level}"
      );
    }
  }

  /// Same agreement for the file-mode access layer, which ignores RUST_LOG.
  #[test]
  fn predicate_matches_file_access_filter() {
    let writer = MemWriter::default();
    emit_access_probe(tracing_subscriber::registry().with(access_layer(writer.clone())));
    assert!(!writer.contents().is_empty(), "file access layer must emit the access event");
    for level in [Level::TRACE, Level::DEBUG, Level::INFO, Level::WARN, Level::ERROR] {
      assert!(access_log_enabled_for_level(Some("dir"), level));
    }
    // and the access event does NOT leak into the system layer
    let writer = MemWriter::default();
    emit_access_probe(tracing_subscriber::registry().with(system_layer(writer.clone(), Level::TRACE)));
    assert!(
      writer.contents().is_empty(),
      "system layer must not emit access events: {:?}",
      writer.contents()
    );
  }

  /// Fixed timer so golden comparisons are deterministic.
  struct FixedTimer;
  impl fmt::time::FormatTime for FixedTimer {
    fn format_time(&self, w: &mut fmt::format::Writer<'_>) -> std::fmt::Result {
      w.write_str("2026-06-13T00:00:00.000000Z")
    }
  }

  /// The pre-R1b access layer (generic compact formatter), reconstructed here
  /// as the golden reference for byte-identical output.
  fn compact_reference_layer<S>(writer: MemWriter) -> impl Layer<S>
  where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
  {
    fmt::layer()
      .with_line_number(false)
      .with_thread_ids(false)
      .with_thread_names(false)
      .with_target(false)
      .with_level(false)
      .compact()
      .with_timer(FixedTimer)
      .with_ansi(false)
      .with_writer(writer)
      .with_filter(AccessLogFilter)
  }

  fn new_format_layer<S>(writer: MemWriter) -> impl Layer<S>
  where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
  {
    fmt::layer()
      .event_format(AccessLogFormat { timer: FixedTimer })
      .with_writer(writer)
      .with_filter(AccessLogFilter)
  }

  /// Representative access-log lines, including client-controlled bytes that
  /// could plausibly render differently (quotes, backslashes, empty values).
  /// CRLF and control bytes are unreachable (rejected by HeaderValue/Uri
  /// parsing), so they are intentionally absent.
  const GOLDEN_MESSAGES: [&str; 4] = [
    "example.com <- 192.168.1.1:8080 -- GET /path?query=value HTTP/1.1 -- 200 OK -- https://example.com/path \"Mozilla/5.0\", \"10.0.0.1\" \"https://backend.example.com/path\"",
    "example.com <- 192.168.1.1:8080 -- GET / HTTP/1.1 -- 200 OK -- https://example.com/ \"UA \\\"quoted\\\" inner\", \"10.0.0.1\" \"https://b/\"",
    "example.com <- 192.168.1.1:8080 -- GET / HTTP/1.1 -- 200 OK -- https://example.com/ \"back\\slash and 'single'\", \"10.0.0.1\" \"https://b/\"",
    "example.com <- 192.168.1.1:8080 -- GET / HTTP/1.1 -- 200 OK -- https://example.com/ \"\", \"\" \"\"",
  ];

  /// R1b acceptance criterion: the minimal formatter is byte-identical to the
  /// generic compact formatter it replaces, for every representative line.
  #[test]
  fn access_format_is_byte_identical_to_compact() {
    for msg in GOLDEN_MESSAGES {
      let (old_w, new_w) = (MemWriter::default(), MemWriter::default());
      tracing::subscriber::with_default(
        tracing_subscriber::registry().with(compact_reference_layer(old_w.clone())),
        || tracing::info!(name: log_event_names::ACCESS_LOG, "{}", msg),
      );
      tracing::subscriber::with_default(tracing_subscriber::registry().with(new_format_layer(new_w.clone())), || {
        tracing::info!(name: log_event_names::ACCESS_LOG, "{}", msg)
      });
      assert_eq!(old_w.contents(), new_w.contents(), "compact vs minimal differ for {msg:?}");
    }
  }

  /// Pin the absolute line shape so both formatters cannot drift together.
  #[test]
  fn access_format_absolute_golden() {
    let writer = MemWriter::default();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(new_format_layer(writer.clone())), || {
      tracing::info!(name: log_event_names::ACCESS_LOG, "{}", GOLDEN_MESSAGES[0])
    });
    assert_eq!(
      writer.contents(),
      format!("2026-06-13T00:00:00.000000Z {}\n", GOLDEN_MESSAGES[0])
    );
  }
}
