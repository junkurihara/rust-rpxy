use crate::{
  backend::{
    Backend, BackendBuilder, Backends, ReverseProxy, Upstream, UpstreamGroup, UpstreamGroupBuilder, UpstreamOption,
  },
  certs::CryptoSource,
  constants::*,
  error::RpxyError,
  log::*,
  utils::{BytesName, PathNameBytesExp},
};
use rustc_hash::FxHashMap as HashMap;
use std::sync::{
  atomic::{AtomicUsize, Ordering},
  Arc,
};
use std::{net::SocketAddr, path::PathBuf};
use tokio::time::Duration;

/// Global object containing proxy configurations and shared object like counters.
/// But note that in Globals, we do not have Mutex and RwLock. It is indeed, the context shared among async tasks.
pub struct Globals<T>
where
  T: CryptoSource,
{
  /// Configuration parameters for proxy transport and request handlers
  pub proxy_config: ProxyConfig, // TODO: proxy configはarcに包んでこいつだけ使いまわせばいいように変えていく。backendsも？

  /// Backend application objects to which http request handler forward incoming requests
  pub backends: Backends<T>,

  /// Shared context - Counter for serving requests
  pub request_count: RequestCount,

  /// Shared context - Async task runtime handler
  pub runtime_handle: tokio::runtime::Handle,
}

/// Configuration parameters for proxy transport and request handlers
#[derive(PartialEq, Eq, Clone)]
pub struct ProxyConfig {
  pub listen_sockets: Vec<SocketAddr>, // when instantiate server
  pub http_port: Option<u16>,          // when instantiate server
  pub https_port: Option<u16>,         // when instantiate server
  pub tcp_listen_backlog: u32,         // when instantiate server

  pub proxy_timeout: Duration,    // when serving requests at Proxy
  pub upstream_timeout: Duration, // when serving requests at Handler

  pub max_clients: usize,          // when serving requests
  pub max_concurrent_streams: u32, // when instantiate server
  pub keepalive: bool,             // when instantiate server

  // experimentals
  pub sni_consistency: bool, // Handler

  pub cache_enabled: bool,
  pub cache_dir: Option<PathBuf>,
  pub cache_max_entry: Option<usize>,
  pub cache_max_each_size: Option<usize>,

  // All need to make packet acceptor
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub http3: bool,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_alt_svc_max_age: u32,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_request_max_body_size: usize,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_max_concurrent_bidistream: u32,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_max_concurrent_unistream: u32,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_max_concurrent_connections: u32,
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3_max_idle_timeout: Option<Duration>,
}

impl Default for ProxyConfig {
  fn default() -> Self {
    Self {
      listen_sockets: Vec::new(),
      http_port: None,
      https_port: None,
      tcp_listen_backlog: TCP_LISTEN_BACKLOG,

      // TODO: Reconsider each timeout values
      proxy_timeout: Duration::from_secs(PROXY_TIMEOUT_SEC),
      upstream_timeout: Duration::from_secs(UPSTREAM_TIMEOUT_SEC),

      max_clients: MAX_CLIENTS,
      max_concurrent_streams: MAX_CONCURRENT_STREAMS,
      keepalive: true,

      sni_consistency: true,

      cache_enabled: false,
      cache_dir: None,
      cache_max_entry: None,
      cache_max_each_size: None,

      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      http3: false,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_alt_svc_max_age: H3::ALT_SVC_MAX_AGE,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_request_max_body_size: H3::REQUEST_MAX_BODY_SIZE,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_max_concurrent_connections: H3::MAX_CONCURRENT_CONNECTIONS,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_max_concurrent_bidistream: H3::MAX_CONCURRENT_BIDISTREAM,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_max_concurrent_unistream: H3::MAX_CONCURRENT_UNISTREAM,
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
      h3_max_idle_timeout: Some(Duration::from_secs(H3::MAX_IDLE_TIMEOUT)),
    }
  }
}

/// Configuration parameters for backend applications
#[derive(PartialEq, Eq, Clone)]
pub struct AppConfigList<T>
where
  T: CryptoSource,
{
  pub inner: Vec<AppConfig<T>>,
  pub default_app: Option<String>,
}
impl<T> TryInto<Backends<T>> for AppConfigList<T>
where
  T: CryptoSource + Clone,
{
  type Error = RpxyError;

  fn try_into(self) -> Result<Backends<T>, Self::Error> {
    let mut backends = Backends::new();
    for app_config in self.inner.iter() {
      let backend = app_config.try_into()?;
      backends
        .apps
        .insert(app_config.server_name.clone().to_server_name_vec(), backend);
      info!(
        "Registering application {} ({})",
        &app_config.server_name, &app_config.app_name
      );
    }

    // default backend application for plaintext http requests
    if let Some(d) = self.default_app {
      let d_sn: Vec<&str> = backends
        .apps
        .iter()
        .filter(|(_k, v)| v.app_name == d)
        .map(|(_, v)| v.server_name.as_ref())
        .collect();
      if !d_sn.is_empty() {
        info!(
          "Serving plaintext http for requests to unconfigured server_name by app {} (server_name: {}).",
          d, d_sn[0]
        );
        backends.default_server_name_bytes = Some(d_sn[0].to_server_name_vec());
      }
    }
    Ok(backends)
  }
}

/// Configuration parameters for single backend application
#[derive(PartialEq, Eq, Clone)]
pub struct AppConfig<T>
where
  T: CryptoSource,
{
  pub app_name: String,
  pub server_name: String,
  pub reverse_proxy: Vec<ReverseProxyConfig>,
  pub tls: Option<TlsConfig<T>>,
}
impl<T> TryInto<Backend<T>> for &AppConfig<T>
where
  T: CryptoSource + Clone,
{
  type Error = RpxyError;

  fn try_into(self) -> Result<Backend<T>, Self::Error> {
    // backend builder
    let mut backend_builder = BackendBuilder::default();
    // reverse proxy settings
    let reverse_proxy = self.try_into()?;

    backend_builder
      .app_name(self.app_name.clone())
      .server_name(self.server_name.clone())
      .reverse_proxy(reverse_proxy);

    // TLS settings and build backend instance
    let backend = if self.tls.is_none() {
      backend_builder.build().map_err(RpxyError::BackendBuild)?
    } else {
      let tls = self.tls.as_ref().unwrap();

      backend_builder
        .https_redirection(Some(tls.https_redirection))
        .crypto_source(Some(tls.inner.clone()))
        .build()?
    };
    Ok(backend)
  }
}
impl<T> TryInto<ReverseProxy> for &AppConfig<T>
where
  T: CryptoSource + Clone,
{
  type Error = RpxyError;

  fn try_into(self) -> Result<ReverseProxy, Self::Error> {
    let mut upstream: HashMap<PathNameBytesExp, UpstreamGroup> = HashMap::default();

    self.reverse_proxy.iter().for_each(|rpo| {
      let upstream_vec: Vec<Upstream> = rpo.upstream.iter().map(|x| x.try_into().unwrap()).collect();
      // let upstream_iter = rpo.upstream.iter().map(|x| x.to_upstream().unwrap());
      // let lb_upstream_num = vec_upstream.len();
      let elem = UpstreamGroupBuilder::default()
        .upstream(&upstream_vec)
        .path(&rpo.path)
        .replace_path(&rpo.replace_path)
        .lb(&rpo.load_balance, &upstream_vec, &self.server_name, &rpo.path)
        .opts(&rpo.upstream_options)
        .build()
        .unwrap();

      upstream.insert(elem.path.clone(), elem);
    });
    if self.reverse_proxy.iter().filter(|rpo| rpo.path.is_none()).count() >= 2 {
      error!("Multiple default reverse proxy setting");
      return Err(RpxyError::ConfigBuild("Invalid reverse proxy setting"));
    }

    if !(upstream.iter().all(|(_, elem)| {
      !(elem.opts.contains(&UpstreamOption::ForceHttp11Upstream)
        && elem.opts.contains(&UpstreamOption::ForceHttp2Upstream))
    })) {
      error!("Either one of force_http11 or force_http2 can be enabled");
      return Err(RpxyError::ConfigBuild("Invalid upstream option setting"));
    }

    Ok(ReverseProxy { upstream })
  }
}

/// Configuration parameters for single reverse proxy corresponding to the path
#[derive(PartialEq, Eq, Clone)]
pub struct ReverseProxyConfig {
  pub path: Option<String>,
  pub replace_path: Option<String>,
  pub upstream: Vec<UpstreamUri>,
  pub upstream_options: Option<Vec<String>>,
  pub load_balance: Option<String>,
}

/// Configuration parameters for single upstream destination from a reverse proxy
#[derive(PartialEq, Eq, Clone)]
pub struct UpstreamUri {
  pub inner: hyper::Uri,
}
impl TryInto<Upstream> for &UpstreamUri {
  type Error = anyhow::Error;

  fn try_into(self) -> std::result::Result<Upstream, Self::Error> {
    Ok(Upstream {
      uri: self.inner.clone(),
    })
  }
}

/// Configuration parameters on TLS for a single backend application
#[derive(PartialEq, Eq, Clone)]
pub struct TlsConfig<T>
where
  T: CryptoSource,
{
  pub inner: T,
  pub https_redirection: bool,
}

#[derive(Debug, Clone, Default)]
/// Counter for serving requests
pub struct RequestCount(Arc<AtomicUsize>);

impl RequestCount {
  pub fn current(&self) -> usize {
    self.0.load(Ordering::Relaxed)
  }

  pub fn increment(&self) -> usize {
    self.0.fetch_add(1, Ordering::Relaxed)
  }

  pub fn decrement(&self) -> usize {
    let mut count;
    while {
      count = self.0.load(Ordering::Relaxed);
      count > 0
        && self
          .0
          .compare_exchange(count, count - 1, Ordering::Relaxed, Ordering::Relaxed)
          != Ok(count)
    } {}
    count
  }
}
