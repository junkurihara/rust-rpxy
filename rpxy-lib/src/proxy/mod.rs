mod proxy_main;
#[cfg(feature = "proxy-protocol")]
mod proxy_protocol;
mod socket;

#[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
mod proxy_h3;
#[cfg(feature = "http3-quinn")]
mod proxy_quic_quinn;
#[cfg(all(feature = "http3-s2n", not(feature = "http3-quinn")))]
mod proxy_quic_s2n;

use crate::{
  globals::Globals,
  hyper_ext::rt::{LocalExecutor, TokioTimer},
  name_exp::ServerName,
};
use hyper_util::server::{self, conn::auto::Builder as ConnectionBuilder};
use rpxy_certs::ServerCryptoForSni;
use std::sync::Arc;

/// SNI to per-SNI server crypto map type (carries the config and whether the vhost enforces mTLS)
pub type SniServerCryptoMap = std::collections::HashMap<ServerName, ServerCryptoForSni, ahash::RandomState>;

pub use proxy_main::{ListenerKind, ListenerSpecBuilder, ListenerSpecBuilderError, ProxyBuilder, ProxyBuilderError};

/// build connection builder shared with proxy instances
pub(crate) fn connection_builder(globals: &Arc<Globals>) -> Arc<ConnectionBuilder<LocalExecutor>> {
  let executor = LocalExecutor::new(globals.runtime_handle.clone());
  let mut http_server = server::conn::auto::Builder::new(executor);
  // Do NOT enable hyper's experimental `pipeline_flush` here. Besides aggregating flushes for
  // pipelined requests, it bypasses the per-connection write-buffer cap (`max_buf_size`) in
  // hyper's h1 `can_buffer()`, so a client reading slower than the body is produced makes hyper
  // buffer the entire response in memory - an unbounded-memory exposure for any large response.
  // Leaving it at the default keeps the cap, so client-socket backpressure propagates to the
  // response body (upstream relay / cache reads).
  http_server
    .http1()
    .keep_alive(globals.proxy_config.keepalive)
    .header_read_timeout(globals.proxy_config.proxy_idle_timeout)
    .timer(TokioTimer);
  http_server
    .http2()
    .max_concurrent_streams(globals.proxy_config.max_concurrent_streams);

  if globals.proxy_config.keepalive {
    http_server
      .http2()
      .keep_alive_interval(Some(globals.proxy_config.proxy_idle_timeout))
      .keep_alive_timeout(globals.proxy_config.proxy_idle_timeout + std::time::Duration::from_secs(1))
      .timer(TokioTimer);
  }
  Arc::new(http_server)
}
