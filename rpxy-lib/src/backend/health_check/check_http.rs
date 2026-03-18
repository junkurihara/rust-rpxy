use crate::{error::RpxyResult, hyper_ext::rt::LocalExecutor, log::*};
use http_body_util::Empty;
use hyper::body::Bytes;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use std::time::Duration;

/// Lightweight HTTP client for health check probes.
/// Shares the same TLS backend and ALPN configuration as the main Forwarder,
/// but omits connection tuning (keepalive, reuse_address) since health checks
/// are infrequent, short-lived probes.
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
  /// Build the health check HTTP client with the same TLS backend and ALPN as the Forwarder.
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
        .map_err(|e| RpxyError::FailedToBuildHealthCheckClient(e.to_string()))?;
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
  pub async fn check(&self, server_name: &str, uri: &hyper::Uri, path: &str, expected_status: u16, timeout: Duration) -> bool {
    let target_uri = match build_health_check_uri(uri, path) {
      Some(u) => u,
      None => {
        debug!("[{server_name}] Failed to build health check URI for {uri}{path}");
        return false;
      }
    };

    let host = target_uri.authority().map(|a| a.as_str()).unwrap_or_default();
    let req = match http::Request::builder()
      .method(http::Method::GET)
      .uri(&target_uri)
      .header(http::header::HOST, host)
      .body(Empty::<Bytes>::new())
    {
      Ok(r) => r,
      Err(e) => {
        debug!("[{server_name}] Failed to build health check request for {target_uri}: {e}");
        return false;
      }
    };

    match tokio::time::timeout(timeout, self.inner.request(req)).await {
      Ok(Ok(resp)) => {
        let status = resp.status().as_u16();
        trace!("[{server_name}] Health check HTTP response for {target_uri}: {status}");
        if status == expected_status {
          true
        } else {
          debug!("[{server_name}] Health check HTTP status mismatch for {target_uri}: got {status}, expected {expected_status}");
          false
        }
      }
      Ok(Err(e)) => {
        debug!("[{server_name}] Health check HTTP request failed for {target_uri}: {e}");
        false
      }
      Err(_) => {
        debug!("[{server_name}] Health check HTTP request timed out for {target_uri}");
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

#[cfg(test)]
mod tests {
  use super::*;

  fn build_uri(base: &str, path: &str) -> Option<String> {
    let base_uri: hyper::Uri = base.parse().unwrap();
    build_health_check_uri(&base_uri, path).map(|u| u.to_string())
  }

  #[test]
  fn http_scheme_preserved() {
    assert_eq!(
      build_uri("http://backend:8080", "/healthz"),
      Some("http://backend:8080/healthz".to_string())
    );
  }

  #[test]
  fn https_scheme_preserved() {
    assert_eq!(
      build_uri("https://backend:443", "/health"),
      Some("https://backend:443/health".to_string())
    );
  }

  #[test]
  fn authority_with_port() {
    assert_eq!(
      build_uri("http://10.0.0.1:9090", "/status"),
      Some("http://10.0.0.1:9090/status".to_string())
    );
  }

  #[test]
  fn root_path() {
    assert_eq!(build_uri("http://backend:80", "/"), Some("http://backend:80/".to_string()));
  }

  #[test]
  fn nested_path() {
    assert_eq!(
      build_uri("http://backend:80", "/api/health"),
      Some("http://backend:80/api/health".to_string())
    );
  }

  #[test]
  fn no_authority_returns_none() {
    let uri: hyper::Uri = "/relative-only".parse().unwrap();
    assert!(build_health_check_uri(&uri, "/healthz").is_none());
  }

  #[test]
  fn ipv6_authority() {
    assert_eq!(
      build_uri("http://[::1]:8080", "/healthz"),
      Some("http://[::1]:8080/healthz".to_string())
    );
  }
}
