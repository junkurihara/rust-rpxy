use crate::utils::ToCanonical;
use std::net::SocketAddr;
pub use tracing::{debug, error, info, warn};

#[derive(Debug, Clone)]
pub struct MessageLog {
  // pub tls_server_name: String,
  pub client_addr: String,
  pub method: String,
  pub host: String,
  pub p_and_q: String,
  pub version: hyper::Version,
  pub uri_scheme: String,
  pub uri_host: String,
  pub ua: String,
  pub xff: String,
  pub status: String,
  pub upstream: String,
}

impl<T> From<&hyper::Request<T>> for MessageLog {
  fn from(req: &hyper::Request<T>) -> Self {
    let header_mapper = |v: hyper::header::HeaderName| {
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
      host: header_mapper(hyper::header::HOST),
      p_and_q: req
        .uri()
        .path_and_query()
        .map_or_else(|| "", |v| v.as_str())
        .to_string(),
      version: req.version(),
      uri_scheme: req.uri().scheme_str().unwrap_or("").to_string(),
      uri_host: req.uri().host().unwrap_or("").to_string(),
      ua: header_mapper(hyper::header::USER_AGENT),
      xff: header_mapper(hyper::header::HeaderName::from_static("x-forwarded-for")),
      status: "".to_string(),
      upstream: "".to_string(),
    }
  }
}

impl MessageLog {
  pub fn client_addr(&mut self, client_addr: &SocketAddr) -> &mut Self {
    self.client_addr = client_addr.to_canonical().to_string();
    self
  }
  // pub fn tls_server_name(&mut self, tls_server_name: &str) -> &mut Self {
  //   self.tls_server_name = tls_server_name.to_string();
  //   self
  // }
  pub fn status_code(&mut self, status_code: &hyper::StatusCode) -> &mut Self {
    self.status = status_code.to_string();
    self
  }
  pub fn xff(&mut self, xff: &Option<&hyper::header::HeaderValue>) -> &mut Self {
    self.xff = xff.map_or_else(|| "", |v| v.to_str().unwrap_or("")).to_string();
    self
  }
  pub fn upstream(&mut self, upstream: &hyper::Uri) -> &mut Self {
    self.upstream = upstream.to_string();
    self
  }

  pub fn output(&self) {
    info!(
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
      self.upstream,
      // self.tls_server_name
    );
  }
}
