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
  let default_port = if uri.scheme_str() == Some("https") { 443 } else { 80 };
  let port = authority.port_u16().unwrap_or(default_port);
  let addr = format!("{}:{}", authority.host(), port);

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
