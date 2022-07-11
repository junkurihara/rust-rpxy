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

    let server_name = self.headers().get(header::HOST).map_or_else(
      || {
        self
          .uri()
          .authority()
          .map_or_else(|| "<none>", |au| au.as_str())
      },
      |h| h.to_str().unwrap_or("<none>"),
    );
    format!(
      "{} <- {} -- {} {:?} {:?} {:?} {}",
      server_name,
      canonical_src,
      self.method(),
      self.version(),
      self
        .uri()
        .path_and_query()
        .map_or_else(|| "", |v| v.as_str()),
      self.headers(),
      extra.map_or_else(|| "", |v| v)
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

    ensure!(
      !(headers_host.is_none() && uri_host.is_none()),
      "No host in request header"
    );

    // prioritize server_name in uri
    uri_host.map_or_else(
      || {
        let m = headers_host.unwrap().as_bytes();
        if m.starts_with(&[b'[']) {
          // v6 address with bracket case. if port is specified, always it is in this case.
          let mut iter = m.split(|ptr| ptr == &b'[' || ptr == &b']');
          iter.next().ok_or_else(|| anyhow!("Invalid Host"))?; // first item is always blank
          iter.next().ok_or_else(|| anyhow!("Invalid Host"))
        } else if m.len() - m.split(|v| v == &b':').fold(0, |acc, s| acc + s.len()) >= 2 {
          // v6 address case, if 2 or more ':' is contained
          Ok(m)
        } else {
          // v4 address or hostname
          m.split(|colon| colon == &b':')
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Invalid Host"))
        }
      },
      |v| Ok(v.as_bytes()),
    )
  }
}
