use super::canonical_address::ToCanonical;
use crate::log::*;
use http::header;
use std::net::SocketAddr;

/// Struct to log HTTP messages
#[derive(Debug, Clone)]
pub struct HttpMessageLog {
  // pub tls_server_name: String,
  pub client_addr: String,
  pub method: String,
  pub host: String,
  pub p_and_q: String,
  pub version: http::Version,
  pub uri_scheme: String,
  pub uri_host: String,
  pub ua: String,
  pub xff: String,
  pub status: String,
  pub upstream: String,
}

impl<T> From<&http::Request<T>> for HttpMessageLog {
  fn from(req: &http::Request<T>) -> Self {
    let header_mapper = |v: header::HeaderName| {
      req
        .headers()
        .get(v)
        .map_or_else(|| "", |s| s.to_str().unwrap_or(""))
        .to_string()
    };
    Self {
      // tls_server_name: "".to_string(),
      client_addr: "".to_string(),
      method: req.method().to_string(),
      host: header_mapper(header::HOST),
      p_and_q: req.uri().path_and_query().map_or_else(|| "", |v| v.as_str()).to_string(),
      version: req.version(),
      uri_scheme: req.uri().scheme_str().unwrap_or("").to_string(),
      uri_host: req.uri().host().unwrap_or("").to_string(),
      ua: header_mapper(header::USER_AGENT),
      xff: header_mapper(header::HeaderName::from_static("x-forwarded-for")),
      status: "".to_string(),
      upstream: "".to_string(),
    }
  }
}

impl std::fmt::Display for HttpMessageLog {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    write!(
      f,
      "{} <- {} -- {} {} {:?} -- {} -- {} \"{}\", \"{}\" \"{}\"",
      if !self.host.is_empty() {
        self.host.as_str()
      } else {
        self.uri_host.as_str()
      },
      self.client_addr,
      self.method,
      self.p_and_q,
      self.version,
      self.status,
      if !self.uri_scheme.is_empty() && !self.uri_host.is_empty() {
        format!("{}://{}", self.uri_scheme, self.uri_host)
      } else {
        "".to_string()
      },
      self.ua,
      self.xff,
      self.upstream
    )
  }
}

impl HttpMessageLog {
  pub fn client_addr(&mut self, client_addr: &SocketAddr) -> &mut Self {
    self.client_addr = client_addr.to_canonical().to_string();
    self
  }
  // pub fn tls_server_name(&mut self, tls_server_name: &str) -> &mut Self {
  //   self.tls_server_name = tls_server_name.to_string();
  //   self
  // }
  pub fn status_code(&mut self, status_code: &http::StatusCode) -> &mut Self {
    self.status = status_code.to_string();
    self
  }
  pub fn xff(&mut self, xff: &Option<&header::HeaderValue>) -> &mut Self {
    self.xff = xff.map_or_else(|| "", |v| v.to_str().unwrap_or("")).to_string();
    self
  }
  pub fn upstream(&mut self, upstream: &http::Uri) -> &mut Self {
    self.upstream = upstream.to_string();
    self
  }

  pub fn output(&self) {
    info!(
      name: crate::constants::log_event_names::ACCESS_LOG,
      "{}", self
    );
  }
}
