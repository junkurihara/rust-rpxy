pub const LISTEN_ADDRESSES_V4: &[&str] = &["0.0.0.0"];
pub const LISTEN_ADDRESSES_V6: &[&str] = &["[::]"];
pub const CONFIG_WATCH_DELAY_SECS: u32 = 15;

#[cfg(feature = "cache")]
// Cache directory
pub const CACHE_DIR: &str = "./cache";
