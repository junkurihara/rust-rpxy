use crate::{error::*, log::*, utils::*};
use hyper::{header, Request};
use std::fmt::Display;

////////////////////////////////////////////////////
// Functions of utils for request messages
pub trait MsgLog {
  fn log<T: Display + ToCanonical>(self, src: &T, extra: Option<&str>);
}
impl<B> MsgLog for &Request<B> {
  fn log<T: Display + ToCanonical>(self, src: &T, extra: Option<&str>) {
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
    info!(
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
    );
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
          println!("v6 bracket");
          // v6 address with bracket case. if port is specified, always it is in this case.
          let mut iter = m.split(|ptr| ptr == &b'[' || ptr == &b']');
          iter.next().ok_or_else(|| anyhow!("Invalid Host"))?; // first item is always blank
          iter.next().ok_or_else(|| anyhow!("Invalid Host"))
        } else if m.len() - m.split(|v| v == &b':').fold(0, |acc, s| acc + s.len()) >= 2 {
          println!("v6 non-bracket");
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

// pub(super) fn parse_host_port<B: core::fmt::Debug>(
//   req: &Request<B>,
// ) -> Result<(String, Option<u16>)> {
//   let headers_host = req.headers().get("host");
//   let uri_host = req.uri().host();
//   let uri_port = req.uri().port_u16();

//   ensure!(
//     !(headers_host.is_none() && uri_host.is_none()),
//     "No host in request header"
//   );

//   // prioritize server_name in uri
//   if let Some(v) = uri_host {
//     Ok((v.to_string(), uri_port))
//   } else {
//     let uri_from_host = headers_host.unwrap().to_str()?.parse::<Uri>()?;
//     Ok((
//       uri_from_host
//         .host()
//         .ok_or_else(|| anyhow!("Failed to parse host"))?
//         .to_string(),
//       uri_from_host.port_u16(),
//     ))
//   }
// }
