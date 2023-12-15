mod crypto_service;
mod proxy_client_cert;
#[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
mod proxy_h3;
mod proxy_main;
#[cfg(feature = "http3-quinn")]
mod proxy_quic_quinn;
#[cfg(feature = "http3-s2n")]
mod proxy_quic_s2n;
mod proxy_tls;
mod socket;

use crate::error::*;
use http::{Response, StatusCode};
use http_body_util::{combinators, BodyExt, Either, Empty};
use hyper::body::{Bytes, Incoming};

pub use proxy_main::{Proxy, ProxyBuilder, ProxyBuilderError};

/// Type for synthetic boxed body
type BoxBody = combinators::BoxBody<Bytes, hyper::Error>;
/// Type for either passthrough body or synthetic body
type EitherBody = Either<Incoming, BoxBody>;

/// helper function to build http response with passthrough body
fn passthrough_response(response: Response<Incoming>) -> Result<Response<EitherBody>> {
  Ok(response.map(EitherBody::Left))
}

/// build http response with status code of 4xx and 5xx
fn synthetic_error_response(status_code: StatusCode) -> Result<Response<EitherBody>> {
  let res = Response::builder()
    .status(status_code)
    .body(EitherBody::Right(BoxBody::new(empty())))
    .unwrap();
  Ok(res)
}

/// helper function to build a empty body
fn empty() -> BoxBody {
  Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}
