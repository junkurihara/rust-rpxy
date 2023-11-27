use super::http_result::*;
use http::{header, Request};

/// Trait defining parser of hostname
pub trait ParseHost {
  type Error;
  fn parse_host(&self) -> Result<&[u8], Self::Error>;
}
impl<B> ParseHost for Request<B> {
  type Error = HttpError;
  /// Extract hostname from either the request HOST header or request line
  fn parse_host(&self) -> HttpResult<&[u8]> {
    let headers_host = self.headers().get(header::HOST);
    let uri_host = self.uri().host();
    // let uri_port = self.uri().port_u16();

    if !(!(headers_host.is_none() && uri_host.is_none())) {
      return Err(HttpError::NoHostInRequestHeader);
    }

    // prioritize server_name in uri
    uri_host.map_or_else(
      || {
        let m = headers_host.unwrap().as_bytes();
        if m.starts_with(&[b'[']) {
          // v6 address with bracket case. if port is specified, always it is in this case.
          let mut iter = m.split(|ptr| ptr == &b'[' || ptr == &b']');
          iter.next().ok_or(HttpError::InvalidHostInRequestHeader)?; // first item is always blank
          iter.next().ok_or(HttpError::InvalidHostInRequestHeader)
        } else if m.len() - m.split(|v| v == &b':').fold(0, |acc, s| acc + s.len()) >= 2 {
          // v6 address case, if 2 or more ':' is contained
          Ok(m)
        } else {
          // v4 address or hostname
          m.split(|colon| colon == &b':')
            .next()
            .ok_or(HttpError::InvalidHostInRequestHeader)
        }
      },
      |v| Ok(v.as_bytes()),
    )
  }
}
