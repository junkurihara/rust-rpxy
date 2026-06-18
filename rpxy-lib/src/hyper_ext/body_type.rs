use super::body::IncomingLike;
use crate::{error::RpxyError, log::*};
use futures::channel::mpsc::Receiver;
use http_body_util::{BodyExt, Empty, Full, StreamBody, combinators};
use hyper::body::{Body, Bytes, Frame, Incoming};
use std::{
  pin::Pin,
  task::{Context, Poll},
};

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

/* ------------------------------------ */
/// Generic length-limited body wrapper. Counts the data bytes seen via `poll_frame` and
/// returns `RpxyError::RequestBodyTooLarge` (with a single `error!` log line) when the
/// running total crosses the configured limit.
///
/// After overrun the wrapper enters a tripped state: subsequent polls do **not** drain
/// the inner body further and the error is returned exactly once, with `Poll::Ready(None)`
/// signalling end-of-stream thereafter. This guarantees that trailer-bearing or
/// non-data frames from the inner body cannot slip through after the limit is exceeded.
///
/// `limit = None` means unlimited and the wrapper short-circuits per `poll_frame` without
/// updating the counter, so unconfigured deployments pay no per-frame cost. The wrapper is
/// generic so it can be instantiated over `Full<Bytes>` or `StreamBody<_>` for unit tests
/// without needing to construct a real `hyper::body::Incoming`.
pub struct LimitedBody<B> {
  inner: B,
  limit: Option<usize>,
  received: usize,
  /// Overrun latch. `false` (initial) = forward inner frames normally. `true` = overrun
  /// has occurred and the `RequestBodyTooLarge` error has been surfaced once in the
  /// poll that crossed the limit; subsequent polls return `Poll::Ready(None)` instead
  /// of draining the inner body further (so trailers / non-data frames cannot slip
  /// through after the limit is exceeded).
  tripped: bool,
}

impl<B> LimitedBody<B> {
  pub fn new(inner: B, limit: Option<usize>) -> Self {
    Self {
      inner,
      limit,
      received: 0,
      tripped: false,
    }
  }
}

impl<B> Body for LimitedBody<B>
where
  B: Body<Data = bytes::Bytes> + Unpin,
  B::Error: Into<RpxyError>,
{
  type Data = bytes::Bytes;
  type Error = RpxyError;

  fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
    let this = self.get_mut();
    // Tripped latch: the error has already been surfaced once; subsequent polls return
    // end-of-stream without ever polling the inner body again, so pending trailers /
    // non-data frames cannot slip through after the limit is exceeded.
    if this.tripped {
      return Poll::Ready(None);
    }
    let Some(limit) = this.limit else {
      // Unlimited fast path: no counting, just forward.
      return Pin::new(&mut this.inner).poll_frame(cx).map_err(Into::into);
    };
    match Pin::new(&mut this.inner).poll_frame(cx) {
      Poll::Pending => Poll::Pending,
      Poll::Ready(None) => Poll::Ready(None),
      Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e.into()))),
      Poll::Ready(Some(Ok(frame))) => {
        if let Some(data) = frame.data_ref() {
          this.received = this.received.saturating_add(data.len());
          if this.received > limit {
            error!(
              "Request body exceeded limit: received {} bytes, maximum allowed {}",
              this.received, limit
            );
            // Surface the error in this poll; the latch makes subsequent polls
            // signal end-of-stream so no further frames (trailers etc.) leak out.
            this.tripped = true;
            return Poll::Ready(Some(Err(RpxyError::RequestBodyTooLarge {
              received: this.received,
              limit,
            })));
          }
        }
        Poll::Ready(Some(Ok(frame)))
      }
    }
  }

  fn is_end_stream(&self) -> bool {
    self.tripped || self.inner.is_end_stream()
  }

  fn size_hint(&self) -> hyper::body::SizeHint {
    self.inner.size_hint()
  }
}

/// Concrete request-body wrapper used by the h1/h2 listener.
pub type LimitedIncoming = LimitedBody<Incoming>;

#[allow(unused)]
/* ------------------------------------ */
/// Request body used in this project
/// - Incoming: client-facing inbound body (h1/h2). Wrapped in `LimitedIncoming` so the
///   configured `request_max_body_size` is enforced as the body streams; an
///   `Option<usize>` limit of `None` disables the check.
/// - IncomingLike: a Incoming-like type in which channel is used (h3 path; the h3 body
///   forwarder enforces the limit before the channel is fed)
pub enum RequestBody {
  Incoming(LimitedIncoming),
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
      RequestBody::Incoming(limited) => Pin::new(limited).poll_frame(cx),
      RequestBody::IncomingLike(incoming_like) => Pin::new(incoming_like).poll_frame(cx),
    }
  }
}

/* ------------------------------------ */
/// Body streamed over a bounded mpsc channel. The producer side awaits when the channel is full,
/// so a slow consumer applies backpressure to the producer instead of letting frames queue in
/// memory without bound.
pub type BoundedStreamBody = StreamBody<Receiver<Result<Frame<bytes::Bytes>, hyper::Error>>>;

#[allow(unused)]
/// Response body use in this project
/// - Incoming: just a type that only forwards the upstream response body to downstream.
/// - Boxed: a type that is generated from cache or synthetic response body, e.g.,, small byte object.
/// - Streamed: another type that is generated from stream, e.g., large byte object.
pub enum ResponseBody {
  Incoming(Incoming),
  Boxed(BoxBody),
  Streamed(BoundedStreamBody),
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

#[cfg(test)]
mod tests {
  use super::*;
  use http_body_util::{BodyExt, Full};

  /// Generic-over-Body test path. `LimitedBody<Full<Bytes>>` lets us hit the wrapper
  /// without constructing a `hyper::body::Incoming` (which has no public constructor).
  /// Production type `LimitedIncoming = LimitedBody<Incoming>` shares this exact code.

  /// Under-limit body passes through; counters do not raise an error.
  #[tokio::test]
  async fn limited_body_under_limit_passes_through() {
    let inner = Full::new(Bytes::from_static(b"hello world"));
    let limited = LimitedBody::new(inner, Some(64));
    let collected = limited.collect().await.unwrap().to_bytes();
    assert_eq!(collected.as_ref(), b"hello world");
  }

  /// Equal-to-limit boundary: 11 bytes with limit 11 passes (the check is strict `>`).
  #[tokio::test]
  async fn limited_body_equal_to_limit_passes() {
    let inner = Full::new(Bytes::from_static(b"hello world"));
    let limited = LimitedBody::new(inner, Some(11));
    let collected = limited.collect().await.unwrap().to_bytes();
    assert_eq!(collected.len(), 11);
  }

  /// Strictly over the limit yields `RpxyError::RequestBodyTooLarge` with the running
  /// received count and the configured limit reported.
  #[tokio::test]
  async fn limited_body_over_limit_errors_with_counts() {
    let inner = Full::new(Bytes::from_static(b"hello world"));
    let limited = LimitedBody::new(inner, Some(5));
    let err = limited.collect().await.unwrap_err();
    match err {
      RpxyError::RequestBodyTooLarge { received, limit } => {
        assert_eq!(limit, 5);
        assert!(received > 5, "received={received} should exceed limit=5");
      }
      other => panic!("expected RequestBodyTooLarge, got {other:?}"),
    }
  }

  /// `None` limit short-circuits the count and never errors, even for a body larger
  /// than any practical limit would allow.
  #[tokio::test]
  async fn limited_body_unlimited_when_none() {
    let big = Bytes::from(vec![0u8; 4 * 1024 * 1024]);
    let inner = Full::new(big);
    let limited = LimitedBody::new(inner, None);
    let collected = limited.collect().await.unwrap().to_bytes();
    assert_eq!(collected.len(), 4 * 1024 * 1024);
  }

  /// Empty body (no frames) is fine under any limit, including `Some(0)`.
  #[tokio::test]
  async fn limited_body_empty_under_zero_limit_passes() {
    let inner = Full::new(Bytes::new());
    let limited = LimitedBody::new(inner, Some(0));
    let collected = limited.collect().await.unwrap().to_bytes();
    assert!(collected.is_empty());
  }

  /// `Some(0)` rejects any non-empty body (preserves the previous h3 `= 0` semantics).
  #[tokio::test]
  async fn limited_body_zero_limit_rejects_non_empty() {
    let inner = Full::new(Bytes::from_static(b"x"));
    let limited = LimitedBody::new(inner, Some(0));
    let err = limited.collect().await.unwrap_err();
    assert!(matches!(err, RpxyError::RequestBodyTooLarge { received: 1, limit: 0 }));
  }

  /// After overrun, subsequent polls return `None` (end-of-stream) rather than draining
  /// the inner body further. This guarantees trailer / non-data frames cannot slip
  /// through after the limit is exceeded, even if a caller keeps polling past the
  /// error frame.
  #[tokio::test]
  async fn limited_body_tripped_state_returns_none_after_error() {
    use std::{
      future::poll_fn,
      pin::Pin,
      task::{Context, Poll, Waker},
    };

    let inner = Full::new(Bytes::from_static(b"hello world"));
    let mut limited = LimitedBody::new(inner, Some(5));
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);

    // First poll: overrun -> error frame.
    let first = poll_fn(|cx| match Pin::new(&mut limited).poll_frame(cx) {
      Poll::Ready(v) => Poll::Ready(v),
      Poll::Pending => Poll::Ready(None),
    })
    .await;
    match first {
      Some(Err(RpxyError::RequestBodyTooLarge { limit: 5, .. })) => {}
      other => panic!("expected RequestBodyTooLarge on first poll, got {other:?}"),
    }

    // Second poll: must be None (tripped-exhausted), NOT another frame from inner.
    let second = match Pin::new(&mut limited).poll_frame(&mut cx) {
      Poll::Ready(v) => v,
      Poll::Pending => panic!("expected Ready"),
    };
    assert!(second.is_none(), "post-overrun poll must return None, got {second:?}");

    // is_end_stream reflects the tripped state too.
    assert!(limited.is_end_stream());
  }
}
