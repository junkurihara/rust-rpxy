// pub const LISTEN_ADDRESSES_V4: &[&str] = &["0.0.0.0"];
// pub const LISTEN_ADDRESSES_V6: &[&str] = &["[::]"];
pub const TCP_LISTEN_BACKLOG: u32 = 1024;
// pub const HTTP_LISTEN_PORT: u16 = 8080;
// pub const HTTPS_LISTEN_PORT: u16 = 8443;
pub const PROXY_TIMEOUT_SEC: u64 = 60;
pub const UPSTREAM_TIMEOUT_SEC: u64 = 60;
pub const TLS_HANDSHAKE_TIMEOUT_SEC: u64 = 15; // default as with firefox browser
pub const MAX_CLIENTS: usize = 512;
pub const MAX_CONCURRENT_STREAMS: u32 = 64;
pub const CERTS_WATCH_DELAY_SECS: u32 = 60;
pub const LOAD_CERTS_ONLY_WHEN_UPDATED: bool = true;

// #[cfg(feature = "http3")]
// pub const H3_RESPONSE_BUF_SIZE: usize = 65_536; // 64KB
// #[cfg(feature = "http3")]
// pub const H3_REQUEST_BUF_SIZE: usize = 65_536; // 64KB // handled by quinn

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
