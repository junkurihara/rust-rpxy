use crate::{
  backend::{Upstream, UpstreamCandidates, UpstreamOption},
  log::*,
};
use anyhow::{Result, anyhow, ensure};
use http::{Request, Version, header, uri::Scheme};

/// Trait defining parser of hostname
/// Inspect and extract hostname from either the request HOST header or request line
pub trait InspectParseHost {
  type Error;
  fn inspect_parse_host(&self) -> Result<Vec<u8>, Self::Error>;
}

/// Strip the `:port` suffix from a Host-header / URI host value.
///
/// Promoted from a closure inside `inspect_parse_host` to a free function so its behavior can
/// be pinned by unit tests. Body is intentionally unchanged in this commit; the dead error
/// arms and the colon-count-by-subtraction trick are rewritten in a follow-up commit, guarded
/// by these tests.
fn drop_port(v: &[u8]) -> Result<Vec<u8>> {
  if v.starts_with(b"[") {
    // v6 address with bracket case. if port is specified, always it is in this case.
    let mut iter = v.split(|ptr| ptr == &b'[' || ptr == &b']');
    iter.next().ok_or_else(|| anyhow!("Invalid Host header"))?; // first item is always blank
    iter
      .next()
      .ok_or_else(|| anyhow!("Invalid Host header"))
      .map(|b| b.to_owned())
  } else if v.len() - v.split(|v| v == &b':').fold(0, |acc, s| acc + s.len()) >= 2 {
    // v6 address case, if 2 or more ':' is contained
    Ok(v.to_owned())
  } else {
    // v4 address or hostname
    v.split(|colon| colon == &b':')
      .next()
      .ok_or_else(|| anyhow!("Invalid Host header"))
      .map(|v| v.to_ascii_lowercase())
  }
}

impl<B> InspectParseHost for Request<B> {
  type Error = anyhow::Error;
  /// Inspect and extract hostname from either the request HOST header or request line
  fn inspect_parse_host(&self) -> Result<Vec<u8>> {
    let headers_host = self.headers().get(header::HOST).map(|v| drop_port(v.as_bytes()));
    let uri_host = self.uri().host().map(|v| drop_port(v.as_bytes()));
    // let uri_port = self.uri().port_u16();

    // prioritize server_name in uri
    match (headers_host, uri_host) {
      (Some(Ok(hh)), Some(Ok(hu))) => {
        ensure!(hh == hu, "Host header and uri host mismatch");
        Ok(hh)
      }
      (Some(Ok(hh)), None) => Ok(hh),
      (None, Some(Ok(hu))) => Ok(hu),
      _ => Err(anyhow!("Neither Host header nor uri host is valid")),
    }
  }
}

////////////////////////////////////////////////////
// Functions to manipulate request line

/// Update request line, e.g., version, and apply upstream options to request line, specified in the configuration
pub(super) fn update_request_line<B>(
  req: &mut Request<B>,
  upstream_chosen: &Upstream,
  upstream_candidates: &UpstreamCandidates,
) -> anyhow::Result<()> {
  // If request is grpc, HTTP/2 is required
  if req
    .headers()
    .get(header::CONTENT_TYPE)
    .map(|v| v.as_bytes().starts_with(b"application/grpc"))
    == Some(true)
  {
    debug!("Must be http/2 for gRPC request.");
    *req.version_mut() = Version::HTTP_2;
    return Ok(());
  }

  // If not specified (force_httpXX_upstream) and https, version is preserved except for http/3
  if upstream_chosen.uri.scheme() == Some(&Scheme::HTTP) {
    // Change version to http/1.1 when destination scheme is http
    debug!("Change version to http/1.1 when destination scheme is http unless upstream option enabled.");
    *req.version_mut() = Version::HTTP_11;
  } else if req.version() == Version::HTTP_3 {
    // HTTP/3 is always https
    debug!("HTTP/3 is currently unsupported for request to upstream.");
    *req.version_mut() = Version::HTTP_2;
  }

  for opt in upstream_candidates.options.iter() {
    match opt {
      UpstreamOption::ForceHttp11Upstream => *req.version_mut() = Version::HTTP_11,
      UpstreamOption::ForceHttp2Upstream => {
        // case: h2c -> https://www.rfc-editor.org/rfc/rfc9113.txt
        // Upgrade from HTTP/1.1 to HTTP/2 is deprecated. So, http-2 prior knowledge is required.
        *req.version_mut() = Version::HTTP_2;
      }
      _ => (),
    }
  }

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Pin the current observable behavior of `drop_port` across every input shape the request
  /// flow reaches it with: IPv4 / hostname (with and without port, mixed case), bracketed
  /// IPv6 (with and without port), bare IPv6, the lenient unterminated-bracket case, and the
  /// empty Host. The body refactor in the next commit must keep this table green unchanged.
  #[test]
  fn drop_port_strips_port_and_normalises_host() {
    let cases: &[(&[u8], &[u8])] = &[
      (b"127.0.0.1", b"127.0.0.1"),
      (b"127.0.0.1:8080", b"127.0.0.1"),
      (b"Example.COM", b"example.com"),
      (b"Example.COM:8080", b"example.com"),
      (b"2001:db8::1", b"2001:db8::1"),
      (b"::1", b"::1"),
      (b"[2001:db8::1]", b"2001:db8::1"),
      (b"[2001:db8::1]:8080", b"2001:db8::1"),
      (b"[::1]:443", b"::1"),
      (b"[2001:db8::1", b"2001:db8::1"),
      (b"", b""),
    ];
    for (input, expected) in cases {
      let got = drop_port(input).unwrap();
      assert_eq!(
        got.as_slice(),
        *expected,
        "drop_port({input:?}) -> {got:?}, expected {expected:?}"
      );
    }
  }
}
