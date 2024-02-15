#[cfg(feature = "cache")]
mod cache;
mod client;

use crate::hyper_ext::body::RequestBody;

pub(crate) type Forwarder<C> = client::Forwarder<C, RequestBody>;
pub(crate) use client::ForwardRequest;

#[cfg(feature = "cache")]
pub(crate) use cache::CacheError;
