pub const LISTEN_ADDRESSES_V4: &[&str] = &["0.0.0.0"];
pub const LISTEN_ADDRESSES_V6: &[&str] = &["[::]"];
pub const CONFIG_WATCH_DELAY_SECS: u32 = 20;

// Cache directory
pub const CACHE_DIR: &str = "./cache";
// # of entries in cache
pub const MAX_CACHE_ENTRY: usize = 1_000;
// max size for each file in bytes
pub const MAX_CACHE_EACH_SIZE: usize = 65_535;

// TODO: max cache size in total
