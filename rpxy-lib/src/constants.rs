pub const RESPONSE_HEADER_SERVER: &str = "rpxy";
pub const TCP_LISTEN_BACKLOG: u32 = 1024;
pub const PROXY_IDLE_TIMEOUT_SEC: u64 = 20;
pub const UPSTREAM_IDLE_TIMEOUT_SEC: u64 = 20;
pub const TLS_HANDSHAKE_TIMEOUT_SEC: u64 = 15; // default as with firefox browser
pub const MAX_CLIENTS: usize = 512;
pub const MAX_CONCURRENT_STREAMS: u32 = 64;

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

// TODO: max cache size in total

/// Logging event name TODO: Other separated logs?
pub mod log_event_names {
  /// access log
  pub const ACCESS_LOG: &str = "rpxy::access";
}
