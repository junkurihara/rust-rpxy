use super::watch;
use crate::error::*;
use futures_channel::{mpsc, oneshot};
use futures_util::{stream::FusedStream, Future, Stream};
use http::HeaderMap;
use hyper::body::{Body, Bytes, Frame, SizeHint};
use std::{
  pin::Pin,
  task::{Context, Poll},
};

////////////////////////////////////////////////////////////
/// Incoming like body to handle incoming request body
pub struct IncomingLike {
  content_length: DecodedLength,
  want_tx: watch::Sender,
  data_rx: mpsc::Receiver<Result<Bytes, hyper::Error>>,
  trailers_rx: oneshot::Receiver<HeaderMap>,
}

macro_rules! ready {
  ($e:expr) => {
    match $e {
      Poll::Ready(v) => v,
      Poll::Pending => return Poll::Pending,
    }
  };
}

type BodySender = mpsc::Sender<Result<Bytes, hyper::Error>>;
type TrailersSender = oneshot::Sender<HeaderMap>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodedLength(u64);
impl DecodedLength {
  pub(crate) const CLOSE_DELIMITED: DecodedLength = DecodedLength(::std::u64::MAX);
  pub(crate) const CHUNKED: DecodedLength = DecodedLength(::std::u64::MAX - 1);
  pub(crate) const ZERO: DecodedLength = DecodedLength(0);

  pub(crate) fn sub_if(&mut self, amt: u64) {
    match *self {
      DecodedLength::CHUNKED | DecodedLength::CLOSE_DELIMITED => (),
      DecodedLength(ref mut known) => {
        *known -= amt;
      }
    }
  }
  /// Converts to an Option<u64> representing a Known or Unknown length.
  pub(crate) fn into_opt(self) -> Option<u64> {
    match self {
      DecodedLength::CHUNKED | DecodedLength::CLOSE_DELIMITED => None,
      DecodedLength(known) => Some(known),
    }
  }
}
pub(crate) struct Sender {
  want_rx: watch::Receiver,
  data_tx: BodySender,
  trailers_tx: Option<TrailersSender>,
}

const WANT_PENDING: usize = 1;
const WANT_READY: usize = 2;

impl IncomingLike {
  /// Create a `Body` stream with an associated sender half.
  ///
  /// Useful when wanting to stream chunks from another thread.
  #[inline]
  #[allow(unused)]
  pub(crate) fn channel() -> (Sender, IncomingLike) {
    Self::new_channel(DecodedLength::CHUNKED, /*wanter =*/ false)
  }

  pub(crate) fn new_channel(content_length: DecodedLength, wanter: bool) -> (Sender, IncomingLike) {
    let (data_tx, data_rx) = mpsc::channel(0);
    let (trailers_tx, trailers_rx) = oneshot::channel();

    // If wanter is true, `Sender::poll_ready()` won't becoming ready
    // until the `Body` has been polled for data once.
    let want = if wanter { WANT_PENDING } else { WANT_READY };

    let (want_tx, want_rx) = watch::channel(want);

    let tx = Sender {
      want_rx,
      data_tx,
      trailers_tx: Some(trailers_tx),
    };
    let rx = IncomingLike {
      content_length,
      want_tx,
      data_rx,
      trailers_rx,
    };

    (tx, rx)
  }
}

impl Body for IncomingLike {
  type Data = Bytes;
  type Error = hyper::Error;

  fn poll_frame(
    mut self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
    self.want_tx.send(WANT_READY);

    if !self.data_rx.is_terminated() {
      if let Some(chunk) = ready!(Pin::new(&mut self.data_rx).poll_next(cx)?) {
        self.content_length.sub_if(chunk.len() as u64);
        return Poll::Ready(Some(Ok(Frame::data(chunk))));
      }
    }

    // check trailers after data is terminated
    match ready!(Pin::new(&mut self.trailers_rx).poll(cx)) {
      Ok(t) => Poll::Ready(Some(Ok(Frame::trailers(t)))),
      Err(_) => Poll::Ready(None),
    }
  }

  fn is_end_stream(&self) -> bool {
    self.content_length == DecodedLength::ZERO
  }

  fn size_hint(&self) -> SizeHint {
    macro_rules! opt_len {
      ($content_length:expr) => {{
        let mut hint = SizeHint::default();

        if let Some(content_length) = $content_length.into_opt() {
          hint.set_exact(content_length);
        }

        hint
      }};
    }

    opt_len!(self.content_length)
  }
}

impl Sender {
  /// Check to see if this `Sender` can send more data.
  pub(crate) fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<RpxyResult<()>> {
    // Check if the receiver end has tried polling for the body yet
    ready!(self.poll_want(cx)?);
    self
      .data_tx
      .poll_ready(cx)
      .map_err(|_| RpxyError::HyperIncomingLikeNewClosed)
  }

  fn poll_want(&mut self, cx: &mut Context<'_>) -> Poll<RpxyResult<()>> {
    match self.want_rx.load(cx) {
      WANT_READY => Poll::Ready(Ok(())),
      WANT_PENDING => Poll::Pending,
      watch::CLOSED => Poll::Ready(Err(RpxyError::HyperIncomingLikeNewClosed)),
      unexpected => unreachable!("want_rx value: {}", unexpected),
    }
  }

  async fn ready(&mut self) -> RpxyResult<()> {
    futures_util::future::poll_fn(|cx| self.poll_ready(cx)).await
  }

  /// Send data on data channel when it is ready.
  #[allow(unused)]
  pub(crate) async fn send_data(&mut self, chunk: Bytes) -> RpxyResult<()> {
    self.ready().await?;
    self
      .data_tx
      .try_send(Ok(chunk))
      .map_err(|_| RpxyError::HyperIncomingLikeNewClosed)
  }

  /// Send trailers on trailers channel.
  #[allow(unused)]
  pub(crate) async fn send_trailers(&mut self, trailers: HeaderMap) -> RpxyResult<()> {
    let tx = match self.trailers_tx.take() {
      Some(tx) => tx,
      None => return Err(RpxyError::HyperIncomingLikeNewClosed),
    };
    tx.send(trailers).map_err(|_| RpxyError::HyperIncomingLikeNewClosed)
  }
}
