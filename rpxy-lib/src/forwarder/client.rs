use crate::{
  error::{RpxyError, RpxyResult},
  globals::Globals,
  hyper_ext::{body::ResponseBody, rt::LocalExecutor},
  log::*,
};
use async_trait::async_trait;
use http::{Request, Response, Version};
use hyper::body::{Body, Incoming};
use hyper_util::client::legacy::{
  Client,
  connect::{Connect, HttpConnector},
};
use std::sync::Arc;

#[cfg(feature = "cache")]
use super::cache::{RpxyCache, get_policy_if_cacheable};

#[async_trait]
/// Definition of the forwarder that simply forward requests from downstream client to upstream app servers.
pub trait ForwardRequest<B1, B2> {
  type Error;
  async fn request(&self, req: Request<B1>) -> Result<Response<B2>, Self::Error>;
}

/// Forwarder http client struct responsible to cache handling
pub struct Forwarder<C, B> {
  #[cfg(feature = "cache")]
  cache: Option<RpxyCache>,
  inner: Client<C, B>,
  inner_h2: Client<C, B>, // `h2c` or http/2-only client is defined separately
}

#[async_trait]
impl<C, B1> ForwardRequest<B1, ResponseBody> for Forwarder<C, B1>
where
  C: Send + Sync + Connect + Clone + 'static,
  B1: Body + Send + Sync + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
  type Error = RpxyError;

  async fn request(&self, req: Request<B1>) -> Result<Response<ResponseBody>, Self::Error> {
    #[cfg(feature = "cache")]
    {
      // No cache configured: forward directly and return the upstream response uncached.
      let Some(cache) = self.cache.as_ref() else {
        return self
          .request_directly(req)
          .await
          .map(|inner| inner.map(ResponseBody::Incoming));
      };

      // The cache is keyed on the request URI seen here, i.e. the upstream-facing URI after the
      // handler's rewrite. Host and `Vary` matching are delegated to `http-cache-semantics`.
      // try reading from cache
      if let Some(cached_response) = cache.get(&req).await {
        // if found, return it as response.
        info!("Cache hit - Return from cache");
        return Ok(cached_response);
      }

      // Synthetic request copy used just for caching (cannot clone request object...); built only
      // on the miss path, before the live request is moved into the upstream client below.
      let synth_req = build_synth_req_for_cache(&req);
      let res = self.request_directly(req).await;

      // check cacheability and store it if cacheable
      let Ok(Some(cache_policy)) = get_policy_if_cacheable(Some(&synth_req), res.as_ref().ok()) else {
        return res.map(|inner| inner.map(ResponseBody::Incoming));
      };
      let (parts, body) = res.unwrap().into_parts();

      // Get streamed body without waiting for the arrival of the body,
      // which is done simultaneously with caching.
      let stream_body = cache.put(synth_req.uri(), body, &cache_policy).await?;

      // response with body being cached in background
      let new_res = Response::from_parts(parts, ResponseBody::Streamed(stream_body));
      Ok(new_res)
    }

    // No cache handling
    #[cfg(not(feature = "cache"))]
    {
      self
        .request_directly(req)
        .await
        .map(|inner| inner.map(ResponseBody::Incoming))
    }
  }
}

impl<C, B1> Forwarder<C, B1>
where
  C: Send + Sync + Connect + Clone + 'static,
  B1: Body + Send + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
  async fn request_directly(&self, req: Request<B1>) -> RpxyResult<Response<Incoming>> {
    // TODO: Revisit this per-request HTTP version dispatch if hyper-util exposes
    // a setup-time h1/h2 client selection path. See https://github.com/hyperium/hyper/issues/2417.
    match req.version() {
      Version::HTTP_2 => self.inner_h2.request(req).await, // handles `h2c` requests
      _ => self.inner.request(req).await,
    }
    .map_err(|e| RpxyError::FailedToFetchFromUpstream(e.to_string()))
  }
}

#[cfg(not(any(feature = "native-tls-backend", feature = "rustls-backend")))]
impl<B> Forwarder<HttpConnector, B>
where
  B: Body + Send + Unpin + 'static,
  <B as Body>::Data: Send,
  <B as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
  /// Build inner client with http
  pub async fn try_new(_globals: &Arc<Globals>) -> RpxyResult<Self> {
    warn!(
      "
--------------------------------------------------------------------------------------------------
Request forwarder is working without TLS support!
This mode is intended for testing only.
Enable 'native-tls-backend' or 'rustls-backend' feature for TLS support.
--------------------------------------------------------------------------------------------------"
    );
    let executor = LocalExecutor::new(_globals.runtime_handle.clone());
    let mut http = HttpConnector::new();
    http.enforce_http(true);
    http.set_reuse_address(true);
    http.set_keepalive(Some(_globals.proxy_config.upstream_idle_timeout));
    // Disable Nagle's algorithm: rpxy relays many small request/response writes upstream.
    http.set_nodelay(true);
    let inner = Client::builder(executor).build::<_, B>(http);
    let inner_h2 = inner.clone();

    Ok(Self {
      inner,
      inner_h2,
      #[cfg(feature = "cache")]
      cache: RpxyCache::new(_globals).await,
    })
  }
}

#[cfg(all(feature = "native-tls-backend", not(feature = "rustls-backend")))]
/// Build forwarder with hyper-tls (native-tls)
impl<B1> Forwarder<hyper_tls::HttpsConnector<HttpConnector>, B1>
where
  B1: Body + Send + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
  /// Build forwarder
  pub async fn try_new(_globals: &Arc<Globals>) -> RpxyResult<Self> {
    // build hyper client with hyper-tls
    info!("Native TLS support enabled for backend connections (native-tls)");
    let executor = LocalExecutor::new(_globals.runtime_handle.clone());

    let try_build_connector = |alpns: &[&str]| {
      hyper_tls::native_tls::TlsConnector::builder()
        .request_alpns(alpns)
        .build()
        .map_err(|e| RpxyError::FailedToBuildForwarder(e.to_string()))
        .map(|tls| {
          let mut http = HttpConnector::new();
          http.enforce_http(false);
          http.set_reuse_address(true);
          http.set_keepalive(Some(_globals.proxy_config.upstream_idle_timeout));
          // Disable Nagle's algorithm: rpxy relays many small request/response writes upstream.
          http.set_nodelay(true);
          hyper_tls::HttpsConnector::from((http, tls.into()))
        })
    };

    let connector = try_build_connector(&["h2", "http/1.1"])?;
    let inner = Client::builder(executor.clone()).build::<_, B1>(connector);

    let connector_h2 = try_build_connector(&["h2"])?;
    let inner_h2 = Client::builder(executor.clone())
      .http2_only(true)
      .build::<_, B1>(connector_h2);

    Ok(Self {
      inner,
      inner_h2,
      #[cfg(feature = "cache")]
      cache: RpxyCache::new(_globals).await,
    })
  }
}

#[cfg(feature = "rustls-backend")]
/// Build forwarder with hyper-rustls (rustls)
impl<B1> Forwarder<hyper_rustls::HttpsConnector<HttpConnector>, B1>
where
  B1: Body + Send + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
  /// Build forwarder
  pub async fn try_new(_globals: &Arc<Globals>) -> RpxyResult<Self> {
    // build hyper client with rustls and webpki, only https is allowed
    #[cfg(feature = "webpki-roots")]
    let builder = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots();
    #[cfg(feature = "webpki-roots")]
    let builder_h2 = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots();
    #[cfg(feature = "webpki-roots")]
    info!("Rustls backend: Mozilla WebPKI root certs used for backend connections");

    #[cfg(not(feature = "webpki-roots"))]
    let builder = hyper_rustls::HttpsConnectorBuilder::new().with_platform_verifier();
    #[cfg(not(feature = "webpki-roots"))]
    let builder_h2 = hyper_rustls::HttpsConnectorBuilder::new().with_platform_verifier();
    #[cfg(not(feature = "webpki-roots"))]
    info!("Rustls backend: Platform verifier used for backend connections");

    let mut http = HttpConnector::new();
    http.enforce_http(false);
    http.set_reuse_address(true);
    http.set_keepalive(Some(_globals.proxy_config.upstream_idle_timeout));
    // Disable Nagle's algorithm: rpxy relays many small request/response writes upstream.
    http.set_nodelay(true);

    let connector = builder.https_or_http().enable_all_versions().wrap_connector(http.clone());
    let connector_h2 = builder_h2.https_or_http().enable_http2().wrap_connector(http);
    let inner = Client::builder(LocalExecutor::new(_globals.runtime_handle.clone())).build::<_, B1>(connector);
    let inner_h2 = Client::builder(LocalExecutor::new(_globals.runtime_handle.clone()))
      .http2_only(true)
      .build::<_, B1>(connector_h2);

    Ok(Self {
      inner,
      inner_h2,
      #[cfg(feature = "cache")]
      cache: RpxyCache::new(_globals).await,
    })
  }
}

#[cfg(feature = "cache")]
/// Build synthetic request to cache (cannot clone request object). The live request is moved
/// into the upstream client on the miss path, so this copy carries the URI, method, version,
/// and headers the cache layer needs for the `CachePolicy` and the store.
fn build_synth_req_for_cache<T>(req: &Request<T>) -> Request<()> {
  let mut builder = Request::builder().method(req.method()).uri(req.uri()).version(req.version());
  // TODO: Include request extensions only if a future cache policy needs them.
  for (header_key, header_value) in req.headers() {
    builder = builder.header(header_key, header_value);
  }
  builder.body(()).unwrap()
}

#[cfg(all(test, feature = "cache"))]
mod tests {
  use super::*;
  use http::{HeaderValue, Method, Uri, header};

  #[test]
  fn synth_req_copies_live_request_uri_method_version_and_headers() {
    // The synthetic cache request must mirror the live request (URI included): the cache is
    // keyed on the request URI seen by the forwarder, not on any separately carried URI.
    let mut req = Request::builder()
      .method(Method::POST)
      .uri(Uri::from_static("http://upstream.internal:9000/u"))
      .version(http::Version::HTTP_2)
      .body(())
      .unwrap();
    req
      .headers_mut()
      .insert(header::ACCEPT_ENCODING, HeaderValue::from_static("gzip"));

    let synth = build_synth_req_for_cache(&req);

    assert_eq!(synth.uri(), &Uri::from_static("http://upstream.internal:9000/u"));
    assert_eq!(synth.method(), Method::POST);
    assert_eq!(synth.version(), http::Version::HTTP_2);
    assert_eq!(synth.headers().get(header::ACCEPT_ENCODING).unwrap(), "gzip");
  }
}
