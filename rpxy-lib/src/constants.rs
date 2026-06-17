pub const RESPONSE_HEADER_SERVER: &str = "rpxy";
pub const TCP_LISTEN_BACKLOG: u32 = 1024;
pub const PROXY_IDLE_TIMEOUT_SEC: u64 = 20;
pub const UPSTREAM_IDLE_TIMEOUT_SEC: u64 = 20;
pub const TLS_HANDSHAKE_TIMEOUT_SEC: u64 = 15; // default as with firefox browser
pub const MAX_CLIENTS: usize = 512;
pub const MAX_CLIENTS_PER_IP: usize = 0; // 0 disables the per-IP connection limit
pub const MAX_CONCURRENT_STREAMS: u32 = 64;

/// Protocol-agnostic defaults that apply to h1/h2/h3 alike.
#[allow(non_snake_case)]
pub mod DEFAULTS {
  /// Default `request_max_body_size` applied to every protocol when unconfigured. 256 MiB.
  /// Conservative bound that closes the unbounded-body DoS hole on h1/h2 while matching the
  /// historical h3-only default so existing h3 deployments see no change. Operators with
  /// larger uploads override via the top-level `request_max_body_size` TOML key; an
  /// effectively-unlimited deployment uses a deliberately large value within TOML's signed
  /// 64-bit integer range (e.g. `9000000000000` for ~9 TB).
  pub const REQUEST_MAX_BODY_SIZE: usize = 268_435_456;
}

#[allow(non_snake_case)]
#[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
pub mod H3 {
  pub const ALT_SVC_MAX_AGE: u32 = 3600;
  pub const MAX_CONCURRENT_CONNECTIONS: u32 = 4096;
  pub const MAX_CONCURRENT_BIDISTREAM: u32 = 64;
  pub const MAX_CONCURRENT_UNISTREAM: u32 = 64;
  pub const MAX_IDLE_TIMEOUT: u64 = 10; // secs
}

#[cfg(feature = "sticky-cookie")]
/// Current cookie name for sticky load-balancing tokens.
pub const STICKY_COOKIE_NAME: &str = "rpxy_sticky_token";

#[cfg(feature = "cache")]
// # of entries in cache
pub const MAX_CACHE_ENTRY: usize = 1_000;
#[cfg(feature = "cache")]
// max size for each file in bytes
pub const MAX_CACHE_EACH_SIZE: usize = 65_535;
#[cfg(feature = "cache")]
// on memory cache if less than or equal to. Defaults to the same value as MAX_CACHE_EACH_SIZE:
// serving a hit from memory is several times faster than the file-backed path (which opens and
// reads the cache file on every hit), so by default every cacheable object stays on memory and
// the file tier engages only when an operator raises max_cache_each_size beyond this. Worst-case
// on-memory footprint at defaults is MAX_CACHE_ENTRY x this value (~64 MB).
pub const MAX_CACHE_EACH_SIZE_ON_MEMORY: usize = 65_535;

#[cfg(feature = "proxy-protocol")]
pub mod proxy_protocol {
  /// Timeout in milliseconds for receiving the PROXY protocol header (enabled with "proxy-protocol" feature).
  pub const TIMEOUT_MSEC: u64 = 50;
}

// TODO: Add a total cache size ceiling; current cache limits cover entry count and per-entry size only.

#[cfg(feature = "health-check")]
/// Default health check constants
pub mod health_check {
  /// Default health check interval in seconds
  pub const DEFAULT_INTERVAL_SEC: u64 = 10;
  /// Default health check timeout in seconds
  pub const DEFAULT_TIMEOUT_SEC: u64 = 5;
  /// Default consecutive failures to mark unhealthy
  pub const DEFAULT_UNHEALTHY_THRESHOLD: u32 = 3;
  /// Default consecutive successes to mark healthy again
  pub const DEFAULT_HEALTHY_THRESHOLD: u32 = 2;
  /// Default expected HTTP status code
  pub const DEFAULT_EXPECTED_STATUS: u16 = 200;
}

/// Logging event names.
///
/// TODO: Split access, operational, and error logs into separate targets if logging needs diverge.
pub mod log_event_names {
  /// access log
  pub const ACCESS_LOG: &str = "rpxy::access";
}
