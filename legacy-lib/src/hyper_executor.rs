use std::sync::Arc;

use hyper_util::server::{self, conn::auto::Builder as ConnectionBuilder};
use tokio::runtime::Handle;

use crate::{globals::Globals, CryptoSource};

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

/// build connection builder shared with proxy instances
pub(crate) fn build_http_server<T>(globals: &Arc<Globals<T>>) -> ConnectionBuilder<LocalExecutor>
where
  T: CryptoSource,
{
  let executor = LocalExecutor::new(globals.runtime_handle.clone());
  let mut http_server = server::conn::auto::Builder::new(executor);
  http_server
    .http1()
    .keep_alive(globals.proxy_config.keepalive)
    .pipeline_flush(true);
  http_server
    .http2()
    .max_concurrent_streams(globals.proxy_config.max_concurrent_streams);
  http_server
}
