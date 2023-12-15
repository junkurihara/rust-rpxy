use super::body::IncomingLike;
use crate::error::RpxyError;
use futures::channel::mpsc::UnboundedReceiver;
use http_body_util::{combinators, BodyExt, Empty, Full, StreamBody};
use hyper::body::{Body, Bytes, Frame, Incoming};
use std::pin::Pin;

/// Type for synthetic boxed body
pub type BoxBody = combinators::BoxBody<Bytes, hyper::Error>;

/// helper function to build a empty body
pub(crate) fn empty() -> BoxBody {
  Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

/// helper function to build a full body
pub(crate) fn full(body: Bytes) -> BoxBody {
  Full::new(body).map_err(|never| match never {}).boxed()
}

#[allow(unused)]
/* ------------------------------------ */
/// Request body used in this project
/// - Incoming: just a type that only forwards the downstream request body to upstream.
/// - IncomingLike: a Incoming-like type in which channel is used
pub enum RequestBody {
  Incoming(Incoming),
  IncomingLike(IncomingLike),
}

impl Body for RequestBody {
  type Data = bytes::Bytes;
  type Error = RpxyError;

  fn poll_frame(
    self: Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
    match self.get_mut() {
      RequestBody::Incoming(incoming) => Pin::new(incoming).poll_frame(cx).map_err(RpxyError::HyperBodyError),
      RequestBody::IncomingLike(incoming_like) => Pin::new(incoming_like).poll_frame(cx),
    }
  }
}

/* ------------------------------------ */
pub type UnboundedStreamBody = StreamBody<UnboundedReceiver<Result<Frame<bytes::Bytes>, hyper::Error>>>;

#[allow(unused)]
/// Response body use in this project
/// - Incoming: just a type that only forwards the upstream response body to downstream.
/// - Boxed: a type that is generated from cache or synthetic response body, e.g.,, small byte object.
/// - Streamed: another type that is generated from stream, e.g., large byte object.
pub enum ResponseBody {
  Incoming(Incoming),
  Boxed(BoxBody),
  Streamed(UnboundedStreamBody),
}

impl Body for ResponseBody {
  type Data = bytes::Bytes;
  type Error = RpxyError;

  fn poll_frame(
    self: Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
    match self.get_mut() {
      ResponseBody::Incoming(incoming) => Pin::new(incoming).poll_frame(cx),
      ResponseBody::Boxed(boxed) => Pin::new(boxed).poll_frame(cx),
      ResponseBody::Streamed(streamed) => Pin::new(streamed).poll_frame(cx),
    }
    .map_err(RpxyError::HyperBodyError)
  }
}
