use crate::{
  backend::{UpstreamGroup, UpstreamOption},
  error::*,
};
use hyper::{header, Request};

////////////////////////////////////////////////////
// Functions to manipulate request line

/// Apply upstream options in request line, specified in the configuration
pub(super) fn apply_upstream_options_to_request_line<B>(req: &mut Request<B>, upstream: &UpstreamGroup) -> Result<()> {
  for opt in upstream.opts.iter() {
    match opt {
      UpstreamOption::ForceHttp11Upstream => *req.version_mut() = hyper::Version::HTTP_11,
      UpstreamOption::ForceHttp2Upstream => {
        // case: h2c -> https://www.rfc-editor.org/rfc/rfc9113.txt
        // Upgrade from HTTP/1.1 to HTTP/2 is deprecated. So, http-2 prior knowledge is required.
        *req.version_mut() = hyper::Version::HTTP_2;
      }
      _ => (),
    }
  }

  Ok(())
}

/// Trait defining parser of hostname
pub trait ParseHost {
  fn parse_host(&self) -> Result<&[u8]>;
}
impl<B> ParseHost for Request<B> {
  /// Extract hostname from either the request HOST header or request line
  fn parse_host(&self) -> Result<&[u8]> {
    let headers_host = self.headers().get(header::HOST);
    let uri_host = self.uri().host();
    // let uri_port = self.uri().port_u16();

    if !(!(headers_host.is_none() && uri_host.is_none())) {
      return Err(RpxyError::Request("No host in request header"));
    }

    // prioritize server_name in uri
    uri_host.map_or_else(
      || {
        let m = headers_host.unwrap().as_bytes();
        if m.starts_with(&[b'[']) {
          // v6 address with bracket case. if port is specified, always it is in this case.
          let mut iter = m.split(|ptr| ptr == &b'[' || ptr == &b']');
          iter.next().ok_or(RpxyError::Request("Invalid Host"))?; // first item is always blank
          iter.next().ok_or(RpxyError::Request("Invalid Host"))
        } else if m.len() - m.split(|v| v == &b':').fold(0, |acc, s| acc + s.len()) >= 2 {
          // v6 address case, if 2 or more ':' is contained
          Ok(m)
        } else {
          // v4 address or hostname
          m.split(|colon| colon == &b':')
            .next()
            .ok_or(RpxyError::Request("Invalid Host"))
        }
      },
      |v| Ok(v.as_bytes()),
    )
  }
}
