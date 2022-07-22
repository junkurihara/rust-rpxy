pub const LISTEN_ADDRESSES_V4: &[&str] = &["0.0.0.0"];
pub const LISTEN_ADDRESSES_V6: &[&str] = &["[::]"];
// pub const HTTP_LISTEN_PORT: u16 = 8080;
// pub const HTTPS_LISTEN_PORT: u16 = 8443;
pub const PROXY_TIMEOUT_SEC: u64 = 60;
pub const UPSTREAM_TIMEOUT_SEC: u64 = 60;
pub const MAX_CLIENTS: usize = 512;
pub const MAX_CONCURRENT_STREAMS: u32 = 64;
// #[cfg(feature = "tls")]
pub const CERTS_WATCH_DELAY_SECS: u32 = 30;

// #[cfg(feature = "http3")]
// pub const H3_RESPONSE_BUF_SIZE: usize = 65_536; // 64KB
// #[cfg(feature = "http3")]
// pub const H3_REQUEST_BUF_SIZE: usize = 65_536; // 64KB // handled by quinn

#[allow(non_snake_case)]
#[cfg(feature = "http3")]
pub mod H3 {
  pub const ALT_SVC_MAX_AGE: u32 = 3600;
  pub const REQUEST_MAX_BODY_SIZE: usize = 268_435_456; // 256MB
  pub const MAX_CONCURRENT_CONNECTIONS: u32 = 4096;
  pub const MAX_CONCURRENT_BIDISTREAM: u32 = 64;
  pub const MAX_CONCURRENT_UNISTREAM: u32 = 64;
}
