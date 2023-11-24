use tokio::runtime::Handle;

#[derive(Clone)]
/// Executor for hyper
pub struct LocalExecutor {
  runtime_handle: Handle,
}

impl LocalExecutor {
  pub fn new(runtime_handle: Handle) -> Self {
    LocalExecutor { runtime_handle }
  }
}

impl<F> hyper::rt::Executor<F> for LocalExecutor
where
  F: std::future::Future + Send + 'static,
  F::Output: Send,
{
  fn execute(&self, fut: F) {
    self.runtime_handle.spawn(fut);
  }
}
