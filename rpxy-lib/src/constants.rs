pub const RESPONSE_HEADER_SERVER: &str = "rpxy";
pub const TCP_LISTEN_BACKLOG: u32 = 1024;
pub const PROXY_IDLE_TIMEOUT_SEC: u64 = 20;
pub const UPSTREAM_IDLE_TIMEOUT_SEC: u64 = 20;
pub const TLS_HANDSHAKE_TIMEOUT_SEC: u64 = 15; // default as with firefox browser
pub const MAX_CLIENTS: usize = 512;
pub const MAX_CONCURRENT_STREAMS: u32 = 64;
/// Maximum request body size (bytes) buffered in memory to enable failover retries.
/// Requests larger than this will not be retried.
pub const MAX_BUFFERED_BODY_SIZE: usize = 1024 * 1024; // 1MB

#[allow(non_snake_case)]
#[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
pub mod H3 {
  pub const ALT_SVC_MAX_AGE: u32 = 3600;
  pub const REQUEST_MAX_BODY_SIZE: usize = 268_435_456; // 256MB
  pub const MAX_CONCURRENT_CONNECTIONS: u32 = 4096;
  pub const MAX_CONCURRENT_BIDISTREAM: u32 = 64;
  pub const MAX_CONCURRENT_UNISTREAM: u32 = 64;
  pub const MAX_IDLE_TIMEOUT: u64 = 10; // secs
}

#[cfg(feature = "sticky-cookie")]
/// For load-balancing with sticky cookie
pub const STICKY_COOKIE_NAME: &str = "rpxy_srv_id";

#[cfg(feature = "cache")]
// # of entries in cache
pub const MAX_CACHE_ENTRY: usize = 1_000;
#[cfg(feature = "cache")]
// max size for each file in bytes
pub const MAX_CACHE_EACH_SIZE: usize = 65_535;
#[cfg(feature = "cache")]
// on memory cache if less than or equel to
pub const MAX_CACHE_EACH_SIZE_ON_MEMORY: usize = 4_096;

#[cfg(feature = "proxy-protocol")]
pub mod proxy_protocol {
  /// Timeout in milliseconds for receiving the PROXY protocol header (enabled with "proxy-protocol" feature).
  pub const TIMEOUT_MSEC: u64 = 50;
}

// TODO: max cache size in total

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

/// Logging event name TODO: Other separated logs?
pub mod log_event_names {
  /// access log
  pub const ACCESS_LOG: &str = "rpxy::access";
}
