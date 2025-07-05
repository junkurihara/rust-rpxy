use super::canonical_address::ToCanonical;
use crate::{log::*, message_handler::utils_headers};
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
  pub scheme: String,
  pub path: String,
  pub ua: String,
  pub xff: String,
  pub forwarded: String,
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
    let host =
      utils_headers::host_from_uri_or_host_header(req.uri(), req.headers().get(header::HOST).cloned()).unwrap_or_default();

    Self {
      // tls_server_name: "".to_string(),
      client_addr: "".to_string(),
      method: req.method().to_string(),
      host,
      p_and_q: req.uri().path_and_query().map_or_else(|| "", |v| v.as_str()).to_string(),
      version: req.version(),
      scheme: req.uri().scheme_str().unwrap_or("").to_string(),
      path: req.uri().path().to_string(),
      ua: header_mapper(header::USER_AGENT),
      xff: header_mapper(header::HeaderName::from_static("x-forwarded-for")),
      forwarded: header_mapper(header::FORWARDED),
      status: "".to_string(),
      upstream: "".to_string(),
    }
  }
}

impl std::fmt::Display for HttpMessageLog {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let forwarded_part = if !self.forwarded.is_empty() {
      format!(" \"{}\"", self.forwarded)
    } else {
      "".to_string()
    };

    write!(
      f,
      "{} <- {} -- {} {} {:?} -- {} -- {} \"{}\", \"{}\"{} \"{}\"",
      self.host,
      self.client_addr,
      self.method,
      self.p_and_q,
      self.version,
      self.status,
      if !self.scheme.is_empty() && !self.host.is_empty() {
        format!("{}://{}{}", self.scheme, self.host, self.path)
      } else {
        self.path.clone()
      },
      self.ua,
      self.xff,
      forwarded_part,
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

#[cfg(test)]
mod tests {
  use super::*;
  use http::{Method, Version};

  #[test]
  fn test_log_format_without_forwarded() {
    let log = HttpMessageLog {
      client_addr: "192.168.1.1:8080".to_string(),
      method: Method::GET.to_string(),
      host: "example.com".to_string(),
      p_and_q: "/path?query=value".to_string(),
      version: Version::HTTP_11,
      scheme: "https".to_string(),
      path: "/path".to_string(),
      ua: "Mozilla/5.0".to_string(),
      xff: "10.0.0.1".to_string(),
      forwarded: "".to_string(),
      status: "200".to_string(),
      upstream: "https://backend.example.com".to_string(),
    };

    let formatted = format!("{}", log);
    assert!(!formatted.contains(" \"\""));
    assert!(formatted.contains("\"Mozilla/5.0\", \"10.0.0.1\" \"https://backend.example.com\""));
  }

  #[test]
  fn test_log_format_with_forwarded() {
    let log = HttpMessageLog {
      client_addr: "192.168.1.1:8080".to_string(),
      method: Method::GET.to_string(),
      host: "example.com".to_string(),
      p_and_q: "/path?query=value".to_string(),
      version: Version::HTTP_11,
      scheme: "https".to_string(),
      path: "/path".to_string(),
      ua: "Mozilla/5.0".to_string(),
      xff: "10.0.0.1".to_string(),
      forwarded: "for=192.0.2.60;proto=http;by=203.0.113.43".to_string(),
      status: "200".to_string(),
      upstream: "https://backend.example.com".to_string(),
    };

    let formatted = format!("{}", log);
    assert!(formatted.contains(" \"for=192.0.2.60;proto=http;by=203.0.113.43\""));
    assert!(
      formatted
        .contains("\"Mozilla/5.0\", \"10.0.0.1\" \"for=192.0.2.60;proto=http;by=203.0.113.43\" \"https://backend.example.com\"")
    );
  }
}
