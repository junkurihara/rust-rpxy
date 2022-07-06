pub const LISTEN_ADDRESSES_V4: &[&str] = &["0.0.0.0"];
pub const LISTEN_ADDRESSES_V6: &[&str] = &["[::]"];
// pub const HTTP_LISTEN_PORT: u16 = 8080;
// pub const HTTPS_LISTEN_PORT: u16 = 8443;
pub const TIMEOUT_SEC: u64 = 10;
pub const MAX_CLIENTS: usize = 512;
pub const MAX_CONCURRENT_STREAMS: u32 = 16;
// #[cfg(feature = "tls")]
pub const CERTS_WATCH_DELAY_SECS: u32 = 10;

pub const GET_LISTENER_RETRY_TIMEOUT_SEC: u64 = 2;

#[cfg(feature = "h3")]
pub const H3_ALT_SVC_MAX_AGE: u32 = 60;
#[cfg(feature = "h3")]
pub const H3_CONN_TIMEOUT_MILLIS: u64 = 2000;
