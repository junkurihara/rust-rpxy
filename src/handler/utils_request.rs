use crate::{error::*, log::*, utils::*};
use hyper::{header, Request};
use std::fmt::Display;

////////////////////////////////////////////////////
// Functions of utils for request messages
pub trait ReqLog {
  fn log<T: Display + ToCanonical>(self, src: &T, extra: Option<&str>);
  fn log_debug<T: Display + ToCanonical>(self, src: &T, extra: Option<&str>);
  fn build_message<T: Display + ToCanonical>(self, src: &T, extra: Option<&str>) -> String;
}
impl<B> ReqLog for &Request<B> {
  fn log<T: Display + ToCanonical>(self, src: &T, extra: Option<&str>) {
    info!("{}", &self.build_message(src, extra));
  }
  fn log_debug<T: Display + ToCanonical>(self, src: &T, extra: Option<&str>) {
    debug!("{}", &self.build_message(src, extra));
  }
  fn build_message<T: Display + ToCanonical>(self, src: &T, extra: Option<&str>) -> String {
    let canonical_src = src.to_canonical();

    let host = self
      .headers()
      .get(header::HOST)
      .map_or_else(|| "", |v| v.to_str().unwrap_or(""));
    let uri_scheme = self
      .uri()
      .scheme_str()
      .map_or_else(|| "".to_string(), |v| format!("{}://", v));
    let uri_host = self.uri().host().unwrap_or("");
    let uri_pq = self.uri().path_and_query().map_or_else(|| "", |v| v.as_str());
    let ua = self
      .headers()
      .get(header::USER_AGENT)
      .map_or_else(|| "", |v| v.to_str().unwrap_or(""));
    let xff = self
      .headers()
      .get("x-forwarded-for")
      .map_or_else(|| "", |v| v.to_str().unwrap_or(""));

    format!(
      "{} <- {} -- {} {} {:?} -- ({}{}) \"{}\" \"{}\" {}",
      host,
      canonical_src,
      self.method(),
      uri_pq,
      self.version(),
      uri_scheme,
      uri_host,
      ua,
      xff,
      extra.unwrap_or("")
    )
  }
}

pub trait ParseHost {
  fn parse_host(&self) -> Result<&[u8]>;
}
impl<B> ParseHost for Request<B> {
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
            .into_iter()
            .next()
            .ok_or(RpxyError::Request("Invalid Host"))
        }
      },
      |v| Ok(v.as_bytes()),
    )
  }
}
