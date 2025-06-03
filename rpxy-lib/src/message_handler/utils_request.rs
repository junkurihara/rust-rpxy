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
impl<B> InspectParseHost for Request<B> {
  type Error = anyhow::Error;
  /// Inspect and extract hostname from either the request HOST header or request line
  fn inspect_parse_host(&self) -> Result<Vec<u8>> {
    let drop_port = |v: &[u8]| {
      if v.starts_with(b"[") {
        // v6 address with bracket case. if port is specified, always it is in this case.
        let mut iter = v.split(|ptr| ptr == &b'[' || ptr == &b']');
        iter.next().ok_or(anyhow!("Invalid Host header"))?; // first item is always blank
        iter.next().ok_or(anyhow!("Invalid Host header")).map(|b| b.to_owned())
      } else if v.len() - v.split(|v| v == &b':').fold(0, |acc, s| acc + s.len()) >= 2 {
        // v6 address case, if 2 or more ':' is contained
        Ok(v.to_owned())
      } else {
        // v4 address or hostname
        v.split(|colon| colon == &b':')
          .next()
          .ok_or(anyhow!("Invalid Host header"))
          .map(|v| v.to_ascii_lowercase())
      }
    };

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
