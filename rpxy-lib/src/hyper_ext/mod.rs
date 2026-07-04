// Channel-backed request-body plumbing (`IncomingLike` and its `watch` want-primitive) is
// HTTP/3 infrastructure: the h3 body-forwarding task feeds request bodies through a channel.
// Unit tests are the only other consumer (they build handler-level requests from it), so the
// two modules are compiled exactly for those cases instead of carrying blanket allows.
#[cfg(any(feature = "http3-quinn", feature = "http3-s2n", test))]
mod body_incoming_like;
mod body_type;
mod executor;
mod tokio_timer;
#[cfg(any(feature = "http3-quinn", feature = "http3-s2n", test))]
mod watch;

pub(crate) mod rt {
  pub(crate) use super::executor::LocalExecutor;
  pub(crate) use super::tokio_timer::TokioTimer;
}
pub(crate) mod body {
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n", test))]
  pub(crate) use super::body_incoming_like::IncomingLike;
  // Cache is the only consumer of these three re-exported NAMES: its small-object hit path
  // boxes bytes via `full` into a `BoxBody`, and its store/read path streams over
  // `BoundedStreamBody`. The `BoxBody` TYPE, `ResponseBody::Boxed`, and `empty()` themselves
  // stay compiled unconditionally (synthetic responses use them); off the cache feature only
  // these re-export names, the `full()` definition, and `BoundedStreamBody`/`Streamed` go away.
  #[cfg(feature = "cache")]
  pub(crate) use super::body_type::{BoundedStreamBody, BoxBody, full};
  pub(crate) use super::body_type::{LimitedIncoming, RequestBody, ResponseBody, empty};
}
