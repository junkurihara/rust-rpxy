mod proxy_h3;
mod proxy_main;
#[cfg(feature = "http3-quinn")]
mod proxy_quic_quinn;
#[cfg(feature = "http3-s2n")]
mod proxy_quic_s2n;
mod socket;

use crate::{globals::Globals, hyper_executor::LocalExecutor};
use hyper_util::server::{self, conn::auto::Builder as ConnectionBuilder};
use std::sync::Arc;

pub(crate) use proxy_main::Proxy;

/// build connection builder shared with proxy instances
pub(crate) fn connection_builder(globals: &Arc<Globals>) -> Arc<ConnectionBuilder<LocalExecutor>> {
  let executor = LocalExecutor::new(globals.runtime_handle.clone());
  let mut http_server = server::conn::auto::Builder::new(executor);
  http_server
    .http1()
    .keep_alive(globals.proxy_config.keepalive)
    .pipeline_flush(true);
  http_server
    .http2()
    .max_concurrent_streams(globals.proxy_config.max_concurrent_streams);
  Arc::new(http_server)
}
