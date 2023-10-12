#[cfg(feature = "cache")]
use super::cache::{get_policy_if_cacheable, RpxyCache};
use crate::{error::RpxyError, globals::Globals, log::*, CryptoSource};
use async_trait::async_trait;
#[cfg(feature = "cache")]
use bytes::Buf;
use hyper::{
  body::{Body, HttpBody},
  client::{connect::Connect, HttpConnector},
  http::Version,
  Client, Request, Response,
};
use hyper_rustls::HttpsConnector;

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

#[async_trait]
/// Definition of the forwarder that simply forward requests from downstream client to upstream app servers.
pub trait ForwardRequest<B> {
  type Error;
  async fn request(&self, req: Request<B>) -> Result<Response<Body>, Self::Error>;
}

/// Forwarder struct responsible to cache handling
pub struct Forwarder<C, B = Body>
where
  C: Connect + Clone + Sync + Send + 'static,
{
  #[cfg(feature = "cache")]
  cache: Option<RpxyCache>,
  inner: Client<C, B>,
  inner_h2: Client<C, B>, // `h2c` or http/2-only client is defined separately
}

#[async_trait]
impl<C, B> ForwardRequest<B> for Forwarder<C, B>
where
  B: HttpBody + Send + Sync + 'static,
  B::Data: Send,
  B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
  C: Connect + Clone + Sync + Send + 'static,
{
  type Error = RpxyError;

  #[cfg(feature = "cache")]
  async fn request(&self, req: Request<B>) -> Result<Response<Body>, Self::Error> {
    let mut synth_req = None;
    if self.cache.is_some() {
      if let Some(cached_response) = self.cache.as_ref().unwrap().get(&req).await {
        // if found, return it as response.
        info!("Cache hit - Return from cache");
        return Ok(cached_response);
      };

      // Synthetic request copy used just for caching (cannot clone request object...)
      synth_req = Some(build_synth_req_for_cache(&req));
    }

    // TODO: This 'match' condition is always evaluated at every 'request' invocation. So, it is inefficient.
    // Needs to be reconsidered. Currently, this is a kind of work around.
    // This possibly relates to https://github.com/hyperium/hyper/issues/2417.
    let res = match req.version() {
      Version::HTTP_2 => self.inner_h2.request(req).await.map_err(RpxyError::Hyper), // handles `h2c` requests
      _ => self.inner.request(req).await.map_err(RpxyError::Hyper),
    };

    if self.cache.is_none() {
      return res;
    }

    // check cacheability and store it if cacheable
    let Ok(Some(cache_policy)) = get_policy_if_cacheable(synth_req.as_ref(), res.as_ref().ok()) else {
      return res;
    };
    let (parts, body) = res.unwrap().into_parts();
    let Ok(mut bytes) = hyper::body::aggregate(body).await else {
      return Err(RpxyError::Cache("Failed to write byte buffer"));
    };
    let aggregated = bytes.copy_to_bytes(bytes.remaining());

    if let Err(cache_err) = self
      .cache
      .as_ref()
      .unwrap()
      .put(synth_req.unwrap().uri(), &aggregated, &cache_policy)
      .await
    {
      error!("{:?}", cache_err);
    };

    // res
    Ok(Response::from_parts(parts, Body::from(aggregated)))
  }

  #[cfg(not(feature = "cache"))]
  async fn request(&self, req: Request<B>) -> Result<Response<Body>, Self::Error> {
    match req.version() {
      Version::HTTP_2 => self.inner_h2.request(req).await.map_err(RpxyError::Hyper), // handles `h2c` requests
      _ => self.inner.request(req).await.map_err(RpxyError::Hyper),
    }
  }
}

impl Forwarder<HttpsConnector<HttpConnector>, Body> {
  /// Build forwarder
  pub async fn new<T: CryptoSource>(_globals: &std::sync::Arc<Globals<T>>) -> Self {
    #[cfg(feature = "native-roots")]
    let builder = hyper_rustls::HttpsConnectorBuilder::new().with_native_roots();
    #[cfg(feature = "native-roots")]
    let builder_h2 = hyper_rustls::HttpsConnectorBuilder::new().with_native_roots();
    #[cfg(feature = "native-roots")]
    info!("Native cert store is used for the connection to backend applications");

    #[cfg(not(feature = "native-roots"))]
    let builder = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots();
    #[cfg(not(feature = "native-roots"))]
    let builder_h2 = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots();
    #[cfg(not(feature = "native-roots"))]
    info!("Mozilla WebPKI root certs is used for the connection to backend applications");

    let connector = builder.https_or_http().enable_http1().enable_http2().build();
    let connector_h2 = builder_h2.https_or_http().enable_http2().build();

    let inner = Client::builder().build::<_, Body>(connector);
    let inner_h2 = Client::builder().http2_only(true).build::<_, Body>(connector_h2);

    #[cfg(feature = "cache")]
    {
      let cache = RpxyCache::new(_globals).await;
      Self { inner, inner_h2, cache }
    }
    #[cfg(not(feature = "cache"))]
    Self { inner, inner_h2 }
  }
}
