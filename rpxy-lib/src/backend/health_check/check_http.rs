use crate::{error::RpxyResult, hyper_ext::rt::LocalExecutor, log::*};
use http_body_util::Empty;
use hyper::body::Bytes;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use std::time::Duration;

/// Lightweight HTTP client for health check probes.
/// Uses the same connector construction pattern as the main Forwarder
/// to ensure transport-level consistency (TLS settings, ALPN, etc.).
pub(super) struct HealthCheckHttpClient {
  inner: InnerClient,
}

// Type aliases for each TLS backend variant
#[cfg(feature = "rustls-backend")]
type InnerClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Empty<Bytes>>;

#[cfg(all(feature = "native-tls-backend", not(feature = "rustls-backend")))]
type InnerClient = Client<hyper_tls::HttpsConnector<HttpConnector>, Empty<Bytes>>;

#[cfg(not(any(feature = "native-tls-backend", feature = "rustls-backend")))]
type InnerClient = Client<HttpConnector, Empty<Bytes>>;

impl HealthCheckHttpClient {
  /// Build the health check HTTP client using the same connector settings as the Forwarder.
  pub fn try_new(runtime_handle: &tokio::runtime::Handle) -> RpxyResult<Self> {
    let executor = LocalExecutor::new(runtime_handle.clone());

    #[cfg(feature = "rustls-backend")]
    let inner = {
      let mut http = HttpConnector::new();
      http.enforce_http(false);

      #[cfg(feature = "webpki-roots")]
      let builder = hyper_rustls::HttpsConnectorBuilder::new().with_webpki_roots();
      #[cfg(not(feature = "webpki-roots"))]
      let builder = hyper_rustls::HttpsConnectorBuilder::new().with_platform_verifier();

      let connector = builder.https_or_http().enable_all_versions().wrap_connector(http);
      Client::builder(executor)
        .pool_max_idle_per_host(1)
        .build::<_, Empty<Bytes>>(connector)
    };

    #[cfg(all(feature = "native-tls-backend", not(feature = "rustls-backend")))]
    let inner = {
      use crate::error::RpxyError;
      let tls = hyper_tls::native_tls::TlsConnector::builder()
        .request_alpns(&["h2", "http/1.1"])
        .build()
        .map_err(|e| RpxyError::FailedToBuildForwarder(e.to_string()))?;
      let mut http = HttpConnector::new();
      http.enforce_http(false);
      let connector = hyper_tls::HttpsConnector::from((http, tls.into()));
      Client::builder(executor)
        .pool_max_idle_per_host(1)
        .build::<_, Empty<Bytes>>(connector)
    };

    #[cfg(not(any(feature = "native-tls-backend", feature = "rustls-backend")))]
    let inner = {
      let mut http = HttpConnector::new();
      http.enforce_http(true);
      Client::builder(executor)
        .pool_max_idle_per_host(1)
        .build::<_, Empty<Bytes>>(http)
    };

    debug!("Health check HTTP client built");
    Ok(Self { inner })
  }

  /// Perform an HTTP health check: GET `uri + path`, check response status.
  pub async fn check(&self, uri: &hyper::Uri, path: &str, expected_status: u16, timeout: Duration) -> bool {
    let target_uri = match build_health_check_uri(uri, path) {
      Some(u) => u,
      None => {
        debug!("Failed to build health check URI for {}{}", uri, path);
        return false;
      }
    };

    let req = match http::Request::builder()
      .method(http::Method::GET)
      .uri(&target_uri)
      .body(Empty::<Bytes>::new())
    {
      Ok(r) => r,
      Err(e) => {
        debug!("Failed to build health check request for {}: {}", target_uri, e);
        return false;
      }
    };

    match tokio::time::timeout(timeout, self.inner.request(req)).await {
      Ok(Ok(resp)) => {
        let status = resp.status().as_u16();
        if status == expected_status {
          true
        } else {
          debug!("Health check HTTP status mismatch for {target_uri}: got {status}, expected {expected_status}");
          false
        }
      }
      Ok(Err(e)) => {
        debug!("Health check HTTP request failed for {target_uri}: {e}");
        false
      }
      Err(_) => {
        debug!("Health check HTTP request timed out for {target_uri}");
        false
      }
    }
  }
}

/// Build health check URI by combining upstream base URI with health check path.
fn build_health_check_uri(base: &hyper::Uri, path: &str) -> Option<hyper::Uri> {
  let authority = base.authority()?;
  let scheme = base.scheme_str().unwrap_or("http");
  format!("{}://{}{}", scheme, authority, path).parse().ok()
}
