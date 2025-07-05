/// Default IPv4 listen addresses for the server.
pub const LISTEN_ADDRESSES_V4: &[&str] = &["0.0.0.0"];
/// Default IPv6 listen addresses for the server.
pub const LISTEN_ADDRESSES_V6: &[&str] = &["[::]"];
/// Delay in seconds before reloading the configuration after changes.
pub const CONFIG_WATCH_DELAY_SECS: u32 = 15;

#[cfg(feature = "cache")]
/// Directory path for cache storage (enabled with "cache" feature).
pub const CACHE_DIR: &str = "./cache";

pub(crate) const ACCESS_LOG_FILE: &str = "access.log";
pub(crate) const SYSTEM_LOG_FILE: &str = "rpxy.log";
