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
/// ported from https://github.com/hyperium/hyper/blob/master/src/body/incoming.rs
pub struct IncomingLike {
  content_length: DecodedLength,
  want_tx: watch::Sender,
  data_rx: mpsc::Receiver<Result<Bytes, RpxyError>>,
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

type BodySender = mpsc::Sender<Result<Bytes, RpxyError>>;
type TrailersSender = oneshot::Sender<HeaderMap>;

const MAX_LEN: u64 = std::u64::MAX - 2;
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct DecodedLength(u64);
impl DecodedLength {
  pub(crate) const CLOSE_DELIMITED: DecodedLength = DecodedLength(::std::u64::MAX);
  pub(crate) const CHUNKED: DecodedLength = DecodedLength(::std::u64::MAX - 1);
  pub(crate) const ZERO: DecodedLength = DecodedLength(0);

  #[allow(dead_code)]
  pub(crate) fn new(len: u64) -> Self {
    debug_assert!(len <= MAX_LEN);
    DecodedLength(len)
  }

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
  type Error = RpxyError;

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

  /// Try to send data on this channel.
  ///
  /// # Errors
  ///
  /// Returns `Err(Bytes)` if the channel could not (currently) accept
  /// another `Bytes`.
  ///
  /// # Note
  ///
  /// This is mostly useful for when trying to send from some other thread
  /// that doesn't have an async context. If in an async context, prefer
  /// `send_data()` instead.
  #[allow(unused)]
  pub(crate) fn try_send_data(&mut self, chunk: Bytes) -> Result<(), Bytes> {
    self
      .data_tx
      .try_send(Ok(chunk))
      .map_err(|err| err.into_inner().expect("just sent Ok"))
  }

  #[allow(unused)]
  pub(crate) fn abort(mut self) {
    self.send_error(RpxyError::HyperNewBodyWriteAborted);
  }

  pub(crate) fn send_error(&mut self, err: RpxyError) {
    let _ = self
      .data_tx
      // clone so the send works even if buffer is full
      .clone()
      .try_send(Err(err));
  }
}

#[cfg(test)]
mod tests {
  use std::mem;
  use std::task::Poll;

  use super::{Body, DecodedLength, IncomingLike, Sender, SizeHint};
  use crate::error::RpxyError;
  use http_body_util::BodyExt;

  #[test]
  fn test_size_of() {
    // These are mostly to help catch *accidentally* increasing
    // the size by too much.

    let body_size = mem::size_of::<IncomingLike>();
    let body_expected_size = mem::size_of::<u64>() * 5;
    assert!(
      body_size <= body_expected_size,
      "Body size = {} <= {}",
      body_size,
      body_expected_size,
    );

    //assert_eq!(body_size, mem::size_of::<Option<Incoming>>(), "Option<Incoming>");

    assert_eq!(mem::size_of::<Sender>(), mem::size_of::<usize>() * 5, "Sender");

    assert_eq!(
      mem::size_of::<Sender>(),
      mem::size_of::<Option<Sender>>(),
      "Option<Sender>"
    );
  }
  #[test]
  fn size_hint() {
    fn eq(body: IncomingLike, b: SizeHint, note: &str) {
      let a = body.size_hint();
      assert_eq!(a.lower(), b.lower(), "lower for {:?}", note);
      assert_eq!(a.upper(), b.upper(), "upper for {:?}", note);
    }

    eq(IncomingLike::channel().1, SizeHint::new(), "channel");

    eq(
      IncomingLike::new_channel(DecodedLength::new(4), /*wanter =*/ false).1,
      SizeHint::with_exact(4),
      "channel with length",
    );
  }

  #[tokio::test]
  async fn channel_abort() {
    let (tx, mut rx) = IncomingLike::channel();

    tx.abort();

    match rx.frame().await.unwrap() {
      Err(RpxyError::HyperNewBodyWriteAborted) => true,
      unexpected => panic!("unexpected: {:?}", unexpected),
    };
  }

  #[tokio::test]
  async fn channel_abort_when_buffer_is_full() {
    let (mut tx, mut rx) = IncomingLike::channel();

    tx.try_send_data("chunk 1".into()).expect("send 1");
    // buffer is full, but can still send abort
    tx.abort();

    let chunk1 = rx.frame().await.expect("item 1").expect("chunk 1").into_data().unwrap();
    assert_eq!(chunk1, "chunk 1");

    match rx.frame().await.unwrap() {
      Err(RpxyError::HyperNewBodyWriteAborted) => true,
      unexpected => panic!("unexpected: {:?}", unexpected),
    };
  }

  #[test]
  fn channel_buffers_one() {
    let (mut tx, _rx) = IncomingLike::channel();

    tx.try_send_data("chunk 1".into()).expect("send 1");

    // buffer is now full
    let chunk2 = tx.try_send_data("chunk 2".into()).expect_err("send 2");
    assert_eq!(chunk2, "chunk 2");
  }

  #[tokio::test]
  async fn channel_empty() {
    let (_, mut rx) = IncomingLike::channel();

    assert!(rx.frame().await.is_none());
  }

  #[test]
  fn channel_ready() {
    let (mut tx, _rx) = IncomingLike::new_channel(DecodedLength::CHUNKED, /*wanter = */ false);

    let mut tx_ready = tokio_test::task::spawn(tx.ready());

    assert!(tx_ready.poll().is_ready(), "tx is ready immediately");
  }

  #[test]
  fn channel_wanter() {
    let (mut tx, mut rx) = IncomingLike::new_channel(DecodedLength::CHUNKED, /*wanter = */ true);

    let mut tx_ready = tokio_test::task::spawn(tx.ready());
    let mut rx_data = tokio_test::task::spawn(rx.frame());

    assert!(tx_ready.poll().is_pending(), "tx isn't ready before rx has been polled");

    assert!(rx_data.poll().is_pending(), "poll rx.data");
    assert!(tx_ready.is_woken(), "rx poll wakes tx");

    assert!(tx_ready.poll().is_ready(), "tx is ready after rx has been polled");
  }

  #[test]

  fn channel_notices_closure() {
    let (mut tx, rx) = IncomingLike::new_channel(DecodedLength::CHUNKED, /*wanter = */ true);

    let mut tx_ready = tokio_test::task::spawn(tx.ready());

    assert!(tx_ready.poll().is_pending(), "tx isn't ready before rx has been polled");

    drop(rx);
    assert!(tx_ready.is_woken(), "dropping rx wakes tx");

    match tx_ready.poll() {
      Poll::Ready(Err(RpxyError::HyperIncomingLikeNewClosed)) => (),
      unexpected => panic!("tx poll ready unexpected: {:?}", unexpected),
    }
  }
}
