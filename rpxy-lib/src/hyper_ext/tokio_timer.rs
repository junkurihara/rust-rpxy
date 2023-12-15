use std::{
  future::Future,
  pin::Pin,
  task::{Context, Poll},
  time::{Duration, Instant},
};

use hyper::rt::{Sleep, Timer};
use pin_project_lite::pin_project;

#[derive(Clone, Debug)]
pub struct TokioTimer;

impl Timer for TokioTimer {
  fn sleep(&self, duration: Duration) -> Pin<Box<dyn Sleep>> {
    Box::pin(TokioSleep {
      inner: tokio::time::sleep(duration),
    })
  }

  fn sleep_until(&self, deadline: Instant) -> Pin<Box<dyn Sleep>> {
    Box::pin(TokioSleep {
      inner: tokio::time::sleep_until(deadline.into()),
    })
  }

  fn reset(&self, sleep: &mut Pin<Box<dyn Sleep>>, new_deadline: Instant) {
    if let Some(sleep) = sleep.as_mut().downcast_mut_pin::<TokioSleep>() {
      sleep.reset(new_deadline)
    }
  }
}

pin_project! {
    pub(crate) struct TokioSleep {
        #[pin]
        pub(crate) inner: tokio::time::Sleep,
    }
}

impl Future for TokioSleep {
  type Output = ();

  fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
    self.project().inner.poll(cx)
  }
}

impl Sleep for TokioSleep {}

impl TokioSleep {
  pub fn reset(self: Pin<&mut Self>, deadline: Instant) {
    self.project().inner.as_mut().reset(deadline.into());
  }
}
