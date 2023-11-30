use crate::{
  error::{RpxyError, RpxyResult},
  globals::Globals,
  hyper_ext::{
    body::{wrap_incoming_body_response, IncomingOr},
    rt::LocalExecutor,
  },
};
use async_trait::async_trait;
use http::{Request, Response, Version};
use hyper::body::Body;
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::{
  connect::{Connect, HttpConnector},
  Client,
};
use std::sync::Arc;

#[async_trait]
/// Definition of the forwarder that simply forward requests from downstream client to upstream app servers.
pub trait ForwardRequest<B1, B2> {
  type Error;
  async fn request(&self, req: Request<B1>) -> Result<Response<B2>, Self::Error>;
}

/// Forwarder http client struct responsible to cache handling
pub struct Forwarder<C, B> {
  // #[cfg(feature = "cache")]
  // cache: Option<RpxyCache>,
  inner: Client<C, B>,
  inner_h2: Client<C, B>, // `h2c` or http/2-only client is defined separately
}

#[async_trait]
impl<C, B1, B2> ForwardRequest<B1, IncomingOr<B2>> for Forwarder<C, B1>
where
  C: Send + Sync + Connect + Clone + 'static,
  B1: Body + Send + Sync + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<(dyn std::error::Error + Send + Sync + 'static)>>,
  B2: Body,
{
  type Error = RpxyError;

  async fn request(&self, req: Request<B1>) -> Result<Response<IncomingOr<B2>>, Self::Error> {
    // TODO: cache handling

    self.request_directly(req).await
  }
}

impl<C, B1> Forwarder<C, B1>
where
  C: Send + Sync + Connect + Clone + 'static,
  B1: Body + Send + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<(dyn std::error::Error + Send + Sync + 'static)>>,
{
  async fn request_directly<B2: Body>(&self, req: Request<B1>) -> RpxyResult<Response<IncomingOr<B2>>> {
    match req.version() {
      Version::HTTP_2 => self.inner_h2.request(req).await, // handles `h2c` requests
      _ => self.inner.request(req).await,
    }
    .map_err(|e| RpxyError::FailedToFetchFromUpstream(e.to_string()))
    .map(wrap_incoming_body_response::<B2>)
  }
}

/// Build forwarder with hyper-tls (native-tls)
impl<B1> Forwarder<HttpsConnector<HttpConnector>, B1>
where
  B1: Body + Send + Unpin + 'static,
  <B1 as Body>::Data: Send,
  <B1 as Body>::Error: Into<Box<(dyn std::error::Error + Send + Sync + 'static)>>,
{
  /// Build forwarder
  pub async fn try_new(_globals: &Arc<Globals>) -> RpxyResult<Self> {
    // build hyper client with hyper-tls
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
          HttpsConnector::from((http, tls.into()))
        })
    };

    let connector = try_build_connector(&["h2", "http/1.1"])?;
    let inner = Client::builder(executor.clone()).build::<_, B1>(connector);

    let connector_h2 = try_build_connector(&["h2"])?;
    let inner_h2 = Client::builder(executor.clone())
      .http2_only(true)
      .build::<_, B1>(connector_h2);

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

    // #[cfg(feature = "cache")]
    // {
    //   let cache = RpxyCache::new(_globals).await;
    //   Self { inner, inner_h2, cache }
    // }
    // #[cfg(not(feature = "cache"))]
    Ok(Self { inner, inner_h2 })
  }
}
