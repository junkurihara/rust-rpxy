mod proxy_main;
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
use rustc_hash::FxHashMap as HashMap;
use rustls::ServerConfig;
use std::sync::Arc;

/// SNI to ServerConfig map type
pub type SniServerCryptoMap = HashMap<ServerName, Arc<ServerConfig>>;

pub(crate) use proxy_main::Proxy;

/// build connection builder shared with proxy instances
pub(crate) fn connection_builder(globals: &Arc<Globals>) -> Arc<ConnectionBuilder<LocalExecutor>> {
  let executor = LocalExecutor::new(globals.runtime_handle.clone());
  let mut http_server = server::conn::auto::Builder::new(executor);
  http_server
    .http1()
    .keep_alive(globals.proxy_config.keepalive)
    .header_read_timeout(globals.proxy_config.proxy_idle_timeout)
    .timer(TokioTimer)
    .pipeline_flush(true);
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
