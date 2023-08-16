use super::cache::RpxyCache;
use crate::{error::RpxyError, globals::Globals, log::*, CryptoSource};
use async_trait::async_trait;
use bytes::Buf;
use derive_builder::Builder;
use hyper::{
  body::{Body, HttpBody},
  client::{connect::Connect, HttpConnector},
  http::Version,
  Client, Request, Response,
};
use hyper_rustls::HttpsConnector;

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

#[derive(Builder, Clone)]
/// Forwarder struct responsible to cache handling
pub struct Forwarder<C, B = Body>
where
  C: Connect + Clone + Sync + Send + 'static,
{
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
  async fn request(&self, req: Request<B>) -> Result<Response<Body>, Self::Error> {
    let mut synth_req = None;
    if self.cache.is_some() {
      if let Some(cached_response) = self.cache.as_ref().unwrap().get(&req).await {
        // if found, return it as response.
        debug!("Cache hit - Return from cache");
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
    let Ok(Some(cache_policy)) = self
      .cache
      .as_ref()
      .unwrap()
      .is_cacheable(synth_req.as_ref(), res.as_ref().ok()) else {
        return res;
      };
    let (parts, body) = res.unwrap().into_parts();
    // TODO: Inefficient?
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
}

impl Forwarder<HttpsConnector<HttpConnector>, Body> {
  pub async fn new<T: CryptoSource>(globals: &std::sync::Arc<Globals<T>>) -> Self {
    // let connector = TrustDnsResolver::default().into_rustls_webpki_https_connector();
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
      .with_webpki_roots()
      .https_or_http()
      .enable_http1()
      .enable_http2()
      .build();
    let connector_h2 = hyper_rustls::HttpsConnectorBuilder::new()
      .with_webpki_roots()
      .https_or_http()
      .enable_http2()
      .build();

    let inner = Client::builder().build::<_, Body>(connector);
    let inner_h2 = Client::builder().http2_only(true).build::<_, Body>(connector_h2);

    let cache = RpxyCache::new(globals).await;
    Self { inner, inner_h2, cache }
  }
}
