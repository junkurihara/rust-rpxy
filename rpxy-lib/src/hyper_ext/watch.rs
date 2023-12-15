//! An SPSC broadcast channel.
//!
//! - The value can only be a `usize`.
//! - The consumer is only notified if the value is different.
//! - The value `0` is reserved for closed.
// from https://github.com/hyperium/hyper/blob/master/src/common/watch.rs

use futures_util::task::AtomicWaker;
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Arc,
};
use std::task;

type Value = usize;

pub(super) const CLOSED: usize = 0;

pub(super) fn channel(initial: Value) -> (Sender, Receiver) {
  debug_assert!(initial != CLOSED, "watch::channel initial state of 0 is reserved");

  let shared = Arc::new(Shared {
    value: AtomicUsize::new(initial),
    waker: AtomicWaker::new(),
  });

  (Sender { shared: shared.clone() }, Receiver { shared })
}

pub(super) struct Sender {
  shared: Arc<Shared>,
}

pub(super) struct Receiver {
  shared: Arc<Shared>,
}

struct Shared {
  value: AtomicUsize,
  waker: AtomicWaker,
}

impl Sender {
  pub(super) fn send(&mut self, value: Value) {
    if self.shared.value.swap(value, Ordering::SeqCst) != value {
      self.shared.waker.wake();
    }
  }
}

impl Drop for Sender {
  fn drop(&mut self) {
    self.send(CLOSED);
  }
}

impl Receiver {
  pub(crate) fn load(&mut self, cx: &mut task::Context<'_>) -> Value {
    self.shared.waker.register(cx.waker());
    self.shared.value.load(Ordering::SeqCst)
  }

  #[allow(dead_code)]
  pub(crate) fn peek(&self) -> Value {
    self.shared.value.load(Ordering::Relaxed)
  }
}
