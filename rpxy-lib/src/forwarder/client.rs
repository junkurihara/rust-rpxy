use crate::{
  error::{RpxyError, RpxyResult},
  globals::Globals,
  hyper_ext::{
    body::{wrap_incoming_body_response, BoxBody, IncomingOr},
    rt::LocalExecutor,
  },
  log::*,
};
use async_trait::async_trait;
use http::{Request, Response, Version};
use hyper::body::{Body, Incoming};
use hyper_util::client::legacy::{
  connect::{Connect, HttpConnector},
  Client,
};
use std::sync::Arc;

#[cfg(feature = "cache")]
use super::cache::{get_policy_if_cacheable, RpxyCache};
#[cfg(feature = "cache")]
use crate::hyper_ext::body::{full, wrap_synthetic_body_response};
#[cfg(feature = "cache")]
use http_body_util::BodyExt;

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
impl<C, B1> ForwardRequest<B1, IncomingOr<BoxBody>> for Forwarder<C, B1>
where
  C: Send + Sync + Connect + Clone + 'static,
  B1: Body + Send + Sync + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<(dyn std::error::Error + Send + Sync + 'static)>>,
{
  type Error = RpxyError;

  async fn request(&self, req: Request<B1>) -> Result<Response<IncomingOr<BoxBody>>, Self::Error> {
    // TODO: cache handling
    #[cfg(feature = "cache")]
    {
      let mut synth_req = None;
      if self.cache.is_some() {
        // if let Some(cached_response) = self.cache.as_ref().unwrap().get(&req).await {
        //   // if found, return it as response.
        //   info!("Cache hit - Return from cache");
        //   return Ok(cached_response);
        // };

        // Synthetic request copy used just for caching (cannot clone request object...)
        synth_req = Some(build_synth_req_for_cache(&req));
      }
      let res = self.request_directly(req).await;

      if self.cache.is_none() {
        return res.map(wrap_incoming_body_response::<BoxBody>);
      }

      // check cacheability and store it if cacheable
      let Ok(Some(cache_policy)) = get_policy_if_cacheable(synth_req.as_ref(), res.as_ref().ok()) else {
        return res.map(wrap_incoming_body_response::<BoxBody>);
      };
      let (parts, body) = res.unwrap().into_parts();

      let Ok(bytes) = body.collect().await.map(|v| v.to_bytes()) else {
        return Err(RpxyError::FailedToWriteByteBufferForCache);
      };

      // TODO: this is inefficient. needs to be reconsidered to avoid unnecessary copy and should spawn async task to store cache.
      // We may need to use the same logic as h3.
      // Is bytes.clone() enough?

      // if let Err(cache_err) = self
      //   .cache
      //   .as_ref()
      //   .unwrap()
      //   .put(synth_req.unwrap().uri(), &bytes, &cache_policy)
      //   .await
      // {
      //   error!("{:?}", cache_err);
      // };

      // response with cached body
      Ok(wrap_synthetic_body_response(Response::from_parts(parts, full(bytes))))
    }

    // No cache handling
    #[cfg(not(feature = "cache"))]
    {
      self
        .request_directly(req)
        .await
        .map(wrap_incoming_body_response::<BoxBody>)
    }
  }
}

impl<C, B1> Forwarder<C, B1>
where
  C: Send + Sync + Connect + Clone + 'static,
  B1: Body + Send + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<(dyn std::error::Error + Send + Sync + 'static)>>,
{
  async fn request_directly(&self, req: Request<B1>) -> RpxyResult<Response<Incoming>> {
    // TODO: This 'match' condition is always evaluated at every 'request' invocation. So, it is inefficient.
    // Needs to be reconsidered. Currently, this is a kind of work around.
    // This possibly relates to https://github.com/hyperium/hyper/issues/2417.
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
  <B as Body>::Error: Into<Box<(dyn std::error::Error + Send + Sync + 'static)>>,
{
  /// Build inner client with http
  pub fn try_new(_globals: &Arc<Globals>) -> RpxyResult<Self> {
    warn!(
      "
--------------------------------------------------------------------------------------------------
Request forwarder is working without TLS support!!!
We recommend to use this just for testing.
Please enable native-tls-backend or rustls-backend feature to enable TLS support.
--------------------------------------------------------------------------------------------------"
    );
    let executor = LocalExecutor::new(_globals.runtime_handle.clone());
    let mut http = HttpConnector::new();
    http.set_reuse_address(true);
    let inner = Client::builder(executor).build::<_, B>(http);

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
  <B1 as Body>::Error: Into<Box<(dyn std::error::Error + Send + Sync + 'static)>>,
{
  /// Build forwarder
  pub async fn try_new(_globals: &Arc<Globals>) -> RpxyResult<Self> {
    // build hyper client with hyper-tls
    info!("Native TLS support is enabled for the connection to backend applications");
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
impl<B1> Forwarder<hyper_tls::HttpsConnector<HttpConnector>, B1>
where
  B1: Body + Send + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<(dyn std::error::Error + Send + Sync + 'static)>>,
{
  /// Build forwarder
  pub async fn try_new(_globals: &Arc<Globals>) -> RpxyResult<Self> {
    todo!("Not implemented yet. Please use native-tls-backend feature for now.");
    // #[cfg(feature = "native-roots")]
    // let builder = hyper_rustls::HttpsConnectorBuilder::new().with_native_roots();
    // #[cfg(feature = "native-roots")]
    // let builder_h2 = hyper_rustls::HttpsConnectorBuilder::new().with_native_roots();
    // #[cfg(feature = "native-roots")]
    // info!("Native cert store is used for the connection to backend applications");

    // #[cfg(not(feature = "native-roots"))]
    // let builder = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots();
    // #[cfg(not(feature = "native-roots"))]
    // let builder_h2 = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots();
    // #[cfg(not(feature = "native-roots"))]
    // info!("Mozilla WebPKI root certs is used for the connection to backend applications");

    // let connector = builder.https_or_http().enable_http1().enable_http2().build();
    // let connector_h2 = builder_h2.https_or_http().enable_http2().build();

    // let inner = Client::builder().build::<_, Body>(connector);
    // let inner_h2 = Client::builder().http2_only(true).build::<_, Body>(connector_h2);
  }
}

#[cfg(feature = "cache")]
/// Build synthetic request to cache
fn build_synth_req_for_cache<T>(req: &Request<T>) -> Request<()> {
  let mut builder = Request::builder()
    .method(req.method())
    .uri(req.uri())
    .version(req.version());
  // TODO: omit extensions. is this approach correct?
  for (header_key, header_value) in req.headers() {
    builder = builder.header(header_key, header_value);
  }
  builder.body(()).unwrap()
}
