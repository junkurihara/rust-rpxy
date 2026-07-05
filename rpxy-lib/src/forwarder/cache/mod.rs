mod cache_error;
mod cache_main;

pub use cache_error::CacheError;
pub(crate) use cache_main::{RpxyCache, get_policy_if_cacheable};

/// Client-facing effective request URI (scheme + authority + path/query), captured by the
/// handler before the upstream rewrite and carried to the forwarder via request extensions.
/// Used as the cache key input so cache entries are partitioned per client-facing vhost and
/// scheme instead of per upstream target. When this extension is absent the forwarder bypasses
/// the cache (fail closed) rather than keying on the upstream-rewritten URI.
#[derive(Clone, Debug)]
pub(crate) struct ClientFacingEffectiveUri(pub(crate) http::Uri);
