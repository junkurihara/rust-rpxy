use http_body_util::{combinators, BodyExt, Either, Empty, Full};
use hyper::body::{Bytes, Incoming};

/// Type for synthetic boxed body
pub(crate) type BoxBody = combinators::BoxBody<Bytes, hyper::Error>;
/// Type for either passthrough body or given body type, specifically synthetic boxed body
pub(crate) type IncomingOr<B> = Either<Incoming, B>;

/// helper function to build a empty body
pub(crate) fn empty() -> BoxBody {
  Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

/// helper function to build a full body
pub(crate) fn full(body: Bytes) -> BoxBody {
  Full::new(body).map_err(|never| match never {}).boxed()
}
