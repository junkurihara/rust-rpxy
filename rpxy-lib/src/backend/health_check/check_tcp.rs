use crate::log::trace;
use hyper::Uri;
use std::time::Duration;
use tokio::net::TcpStream;

/// Perform a TCP health check by attempting to connect to the upstream's host:port.
///
/// - DNS resolution is handled internally by `TcpStream::connect` (uses host:port string).
/// - For HTTPS upstreams, only TCP connectivity is verified (no TLS handshake).
/// - Returns `true` if TCP 3-way handshake completes within `timeout`.
pub(super) async fn check_tcp(server_name: &str, uri: &Uri, timeout: Duration) -> bool {
  let Some(authority) = uri.authority() else {
    return false;
  };
  let addr = build_tcp_addr(uri, authority);

  let res = tokio::time::timeout(timeout, TcpStream::connect(&addr))
    .await
    .is_ok_and(|r| r.is_ok());

  trace!(
    "[{server_name}] TCP health check for {}: {}",
    addr,
    if res { "healthy" } else { "unhealthy" }
  );

  res
}

/// Build TCP address string from URI and authority.
/// IPv6 hosts from `Authority::host()` already include brackets (e.g. `[::1]`),
/// so we use them directly without wrapping again.
fn build_tcp_addr(uri: &Uri, authority: &hyper::http::uri::Authority) -> String {
  let default_port = if uri.scheme_str() == Some("https") { 443 } else { 80 };
  let port = authority.port_u16().unwrap_or(default_port);
  let host = authority.host();
  format!("{}:{}", host, port)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn addr_from(uri_str: &str) -> String {
    let uri: Uri = uri_str.parse().unwrap();
    let authority = uri.authority().unwrap().clone();
    build_tcp_addr(&uri, &authority)
  }

  #[test]
  fn ipv4_with_explicit_port() {
    assert_eq!(addr_from("http://192.168.1.1:8080"), "192.168.1.1:8080");
  }

  #[test]
  fn ipv4_default_http_port() {
    assert_eq!(addr_from("http://192.168.1.1"), "192.168.1.1:80");
  }

  #[test]
  fn ipv4_default_https_port() {
    assert_eq!(addr_from("https://192.168.1.1"), "192.168.1.1:443");
  }

  #[test]
  fn hostname_with_port() {
    assert_eq!(addr_from("http://backend.local:3000"), "backend.local:3000");
  }

  #[test]
  fn hostname_default_port() {
    assert_eq!(addr_from("http://backend.local"), "backend.local:80");
  }

  #[test]
  fn ipv6_with_explicit_port() {
    assert_eq!(addr_from("http://[::1]:8080"), "[::1]:8080");
  }

  #[test]
  fn ipv6_default_http_port() {
    assert_eq!(addr_from("http://[::1]"), "[::1]:80");
  }

  #[test]
  fn ipv6_default_https_port() {
    assert_eq!(addr_from("https://[::1]"), "[::1]:443");
  }

  #[test]
  fn ipv6_full_address() {
    assert_eq!(addr_from("http://[2001:db8::1]:9090"), "[2001:db8::1]:9090");
  }

  #[tokio::test]
  async fn check_tcp_returns_false_for_no_authority() {
    let uri: Uri = "/no-authority".parse().unwrap();
    assert!(!check_tcp("test", &uri, Duration::from_millis(100)).await);
  }

  #[tokio::test]
  async fn check_tcp_returns_false_for_unreachable() {
    // Port 1 is almost certainly not listening
    let uri: Uri = "http://127.0.0.1:1".parse().unwrap();
    assert!(!check_tcp("test", &uri, Duration::from_millis(200)).await);
  }
}
