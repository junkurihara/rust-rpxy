#[cfg(feature = "cache")]
mod cache;
mod client;

use crate::hyper_ext::body::{IncomingLike, IncomingOr};

pub(crate) type Forwarder<C> = client::Forwarder<C, IncomingOr<IncomingLike>>;
pub(crate) use client::ForwardRequest;

#[cfg(feature = "cache")]
pub(crate) use cache::CacheError;
