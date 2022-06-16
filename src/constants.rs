pub const LISTEN_ADDRESSES: &[&str] = &["127.0.0.1:8443", "[::1]:8443"];
pub const TIMEOUT_SEC: u64 = 10;
pub const MAX_CLIENTS: usize = 512;
pub const MAX_CONCURRENT_STREAMS: u32 = 16;
#[cfg(feature = "tls")]
pub const CERTS_WATCH_DELAY_SECS: u32 = 10;
