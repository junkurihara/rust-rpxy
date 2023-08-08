use crate::error::RpxyError;
use async_trait::async_trait;
use derive_builder::Builder;
use hyper::{
  body::{Body, HttpBody},
  client::{connect::Connect, HttpConnector},
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
  // TODO: need `h2c` or http/2-only client separately
  inner: Client<C, B>,
  // TODO: maybe this forwarder definition is suitable for cache handling.
}

#[async_trait]
impl<C, B> ForwardRequest<B> for Forwarder<C, B>
where
  B: HttpBody + Send + 'static,
  B::Data: Send,
  B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
  C: Connect + Clone + Sync + Send + 'static,
{
  type Error = RpxyError;
  async fn request(&self, req: Request<B>) -> Result<Response<Body>, Self::Error> {
    // TODO:
    // TODO: Implement here a client that handles `h2c` requests
    // TODO:
    self.inner.request(req).await.map_err(RpxyError::Hyper)
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

    let inner = Client::builder().build::<_, Body>(connector);
    Self { inner }
  }
}
