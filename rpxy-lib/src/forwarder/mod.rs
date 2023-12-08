mod cache;
mod client;

use crate::hyper_ext::body::{IncomingLike, IncomingOr};
pub type Forwarder<C> = client::Forwarder<C, IncomingOr<IncomingLike>>;

pub use client::ForwardRequest;
