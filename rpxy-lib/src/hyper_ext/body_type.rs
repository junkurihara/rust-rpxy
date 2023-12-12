// use http::Response;
use http_body_util::{combinators, BodyExt, Either, Empty, Full};
use hyper::body::{Body, Bytes, Incoming};
use std::pin::Pin;

/// Type for synthetic boxed body
pub(crate) type BoxBody = combinators::BoxBody<Bytes, hyper::Error>;
/// Type for either passthrough body or given body type, specifically synthetic boxed body
pub(crate) type IncomingOr<B> = Either<Incoming, B>;

// /// helper function to build http response with passthrough body
// pub(crate) fn wrap_incoming_body_response<B>(response: Response<Incoming>) -> Response<IncomingOr<B>>
// where
//   B: hyper::body::Body,
// {
//   response.map(IncomingOr::Left)
// }

// /// helper function to build http response with synthetic body
// pub(crate) fn wrap_synthetic_body_response<B>(response: Response<B>) -> Response<IncomingOr<B>> {
//   response.map(IncomingOr::Right)
// }

/// helper function to build a empty body
pub(crate) fn empty() -> BoxBody {
  Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

/// helper function to build a full body
pub(crate) fn full(body: Bytes) -> BoxBody {
  Full::new(body).map_err(|never| match never {}).boxed()
}

/* ------------------------------------ */
#[cfg(feature = "cache")]
use futures::channel::mpsc::UnboundedReceiver;
#[cfg(feature = "cache")]
use http_body_util::StreamBody;
#[cfg(feature = "cache")]
use hyper::body::Frame;

#[cfg(feature = "cache")]
pub(crate) type UnboundedStreamBody = StreamBody<UnboundedReceiver<Result<Frame<bytes::Bytes>, hyper::Error>>>;

/// Response body use in this project
/// - Incoming: just a type that only forwards the upstream response body to downstream.
/// - BoxedCache: a type that is generated from cache, e.g.,, small byte object.
/// - StreamedCache: another type that is generated from cache as stream, e.g., large byte object.
pub(crate) enum ResponseBody {
  Incoming(Incoming),
  Boxed(BoxBody),
  #[cfg(feature = "cache")]
  Streamed(UnboundedStreamBody),
}

impl Body for ResponseBody {
  type Data = bytes::Bytes;
  type Error = hyper::Error;

  fn poll_frame(
    self: Pin<&mut Self>,
    cx: &mut std::task::Context<'_>,
  ) -> std::task::Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
    match self.get_mut() {
      ResponseBody::Incoming(incoming) => Pin::new(incoming).poll_frame(cx),
      #[cfg(feature = "cache")]
      ResponseBody::Boxed(boxed) => Pin::new(boxed).poll_frame(cx),
      #[cfg(feature = "cache")]
      ResponseBody::Streamed(streamed) => Pin::new(streamed).poll_frame(cx),
    }
  }
}
