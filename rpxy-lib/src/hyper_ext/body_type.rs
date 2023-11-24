use crate::error::*;
use http::{Response, StatusCode};
use http_body_util::{combinators, BodyExt, Either, Empty, Full};
use hyper::body::{Bytes, Incoming};

/// Type for synthetic boxed body
pub(crate) type BoxBody = combinators::BoxBody<Bytes, hyper::Error>;
/// Type for either passthrough body or given body type, specifically synthetic boxed body
pub(crate) type IncomingOr<B> = Either<Incoming, B>;

/// helper function to build http response with passthrough body
pub(crate) fn passthrough_response<B>(response: Response<Incoming>) -> RpxyResult<Response<IncomingOr<B>>>
where
  B: hyper::body::Body,
{
  Ok(response.map(IncomingOr::Left))
}

/// helper function to build http response with synthetic body
pub(crate) fn synthetic_response<B>(response: Response<B>) -> RpxyResult<Response<IncomingOr<B>>> {
  Ok(response.map(IncomingOr::Right))
}

/// build http response with status code of 4xx and 5xx
pub(crate) fn synthetic_error_response(status_code: StatusCode) -> RpxyResult<Response<IncomingOr<BoxBody>>> {
  let res = Response::builder()
    .status(status_code)
    .body(IncomingOr::Right(BoxBody::new(empty())))
    .unwrap();
  Ok(res)
}

/// helper function to build a empty body
fn empty() -> BoxBody {
  Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

/// helper function to build a full body
pub(crate) fn full(body: Bytes) -> BoxBody {
  Full::new(body).map_err(|never| match never {}).boxed()
}
