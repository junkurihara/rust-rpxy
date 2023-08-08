use crate::error::RpxyError;
use async_trait::async_trait;
use derive_builder::Builder;
use hyper::{
  body::{Body, HttpBody},
  client::{connect::Connect, HttpConnector},
  http::Version,
  Client, Request, Response,
};
use hyper_rustls::HttpsConnector;

#[async_trait]
/// Definition of the forwarder that simply forward requests from downstream client to upstream app servers.
pub trait ForwardRequest<B> {
  type Error;
  async fn request(&self, req: Request<B>) -> Result<Response<Body>, Self::Error>;
}

#[derive(Builder, Clone)]
/// Forwarder struct
pub struct Forwarder<C, B = Body>
where
  C: Connect + Clone + Sync + Send + 'static,
{
  // TODO: maybe this forwarder definition is suitable for cache handling.
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
    // TODO: This 'match' condition is always evaluated at every 'request' invocation. So, it is inefficient.
    // Needs to be reconsidered. Currently, this is a kind of work around.
    // This possibly relates to https://github.com/hyperium/hyper/issues/2417.
    match req.version() {
      Version::HTTP_2 => self.inner_h2.request(req).await.map_err(RpxyError::Hyper), // handles `h2c` requests
      _ => self.inner.request(req).await.map_err(RpxyError::Hyper),
    }
  }
}

impl Forwarder<HttpsConnector<HttpConnector>, Body> {
  pub async fn new() -> Self {
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
      .enable_http1()
      .build();

    let inner = Client::builder().build::<_, Body>(connector);
    let inner_h2 = Client::builder().http2_only(true).build::<_, Body>(connector_h2);
    Self { inner, inner_h2 }
  }
}
