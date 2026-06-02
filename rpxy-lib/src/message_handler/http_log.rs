use super::canonical_address::ToCanonical;
use crate::{log::*, message_handler::header_ops};
use http::header;
use std::{borrow::Cow, net::SocketAddr};

/// Placeholder substituted for redacted query-string values in the access log.
const REDACTED: &str = "<redacted>";

/// Redact the values of query-string parameters in a path-and-query or full-URI string.
///
/// Everything up to and including the first `?` (the path) is kept. Each `&`-separated query
/// segment is masked: `key=value` becomes `key=<redacted>` (the key is kept), and a non-empty
/// segment without `=` is replaced entirely with `<redacted>`. Empty segments and inputs with no
/// query are left unchanged; the no-query case borrows without allocating. No URL-decoding or
/// parameter denylist is applied.
fn redact_query_values(path_and_query: &str) -> Cow<'_, str> {
  let Some(query_at) = path_and_query.find('?') else {
    return Cow::Borrowed(path_and_query);
  };
  let (prefix, query) = path_and_query.split_at(query_at + 1);
  if query.is_empty() {
    return Cow::Borrowed(path_and_query);
  }
  let mut out = String::with_capacity(path_and_query.len());
  out.push_str(prefix);
  for (i, segment) in query.split('&').enumerate() {
    if i > 0 {
      out.push('&');
    }
    if segment.is_empty() {
      continue;
    }
    match segment.split_once('=') {
      Some((key, _value)) => {
        out.push_str(key);
        out.push('=');
        out.push_str(REDACTED);
      }
      None => out.push_str(REDACTED),
    }
  }
  Cow::Owned(out)
}

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
  /// When set, query-string values in `p_and_q` and `upstream` are stored already redacted.
  redact_query: bool,
}

impl HttpMessageLog {
  /// Build an access-log record from a request. When `redact_query` is set, query-string values
  /// in `p_and_q` are masked at construction (and in `upstream` via its setter), so the struct
  /// never retains raw query values once redaction is enabled.
  pub fn new<T>(req: &http::Request<T>, redact_query: bool) -> Self {
    let header_mapper = |v: header::HeaderName| {
      req
        .headers()
        .get(v)
        .map_or_else(|| "", |s| s.to_str().unwrap_or(""))
        .to_string()
    };
    let host = header_ops::host_from_uri_or_host_header(req.uri(), req.headers().get(header::HOST)).unwrap_or_default();
    let p_and_q_raw = req.uri().path_and_query().map_or_else(|| "", |v| v.as_str());
    let p_and_q = if redact_query {
      redact_query_values(p_and_q_raw).into_owned()
    } else {
      p_and_q_raw.to_string()
    };

    Self {
      // tls_server_name: "".to_string(),
      client_addr: "".to_string(),
      method: req.method().to_string(),
      host,
      p_and_q,
      version: req.version(),
      scheme: req.uri().scheme_str().unwrap_or("").to_string(),
      path: req.uri().path().to_string(),
      ua: header_mapper(header::USER_AGENT),
      xff: header_mapper(header_ops::header_defs::X_FORWARDED_FOR),
      forwarded: header_mapper(header::FORWARDED),
      status: "".to_string(),
      upstream: "".to_string(),
      redact_query,
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
    if !self.redact_query {
      self.upstream = upstream.to_string();
      return self;
    }
    // Redaction on. For the common absolute form, rebuild from the URI parts so the raw query is
    // never copied into an owned string; fall back to redacting the rendered URI for other forms.
    self.upstream = match (upstream.scheme_str(), upstream.authority()) {
      (Some(scheme), Some(authority)) => {
        let mut s = String::new();
        s.push_str(scheme);
        s.push_str("://");
        s.push_str(authority.as_str());
        if let Some(p_and_q) = upstream.path_and_query() {
          s.push_str(&redact_query_values(p_and_q.as_str()));
        }
        s
      }
      _ => redact_query_values(&upstream.to_string()).into_owned(),
    };
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
      redact_query: false,
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
      redact_query: false,
    };

    let formatted = format!("{}", log);
    assert!(formatted.contains(" \"for=192.0.2.60;proto=http;by=203.0.113.43\""));
    assert!(
      formatted
        .contains("\"Mozilla/5.0\", \"10.0.0.1\" \"for=192.0.2.60;proto=http;by=203.0.113.43\" \"https://backend.example.com\"")
    );
  }

  #[test]
  fn redact_query_values_no_query_borrows() {
    let out = redact_query_values("/path/to/resource");
    assert!(matches!(out, Cow::Borrowed(_)), "no-query input must borrow");
    assert_eq!(out, "/path/to/resource");
  }

  #[test]
  fn redact_query_values_masks_values_keeps_keys() {
    let out = redact_query_values("/reset?token=abc123&email=a@b.com");
    assert_eq!(out, "/reset?token=<redacted>&email=<redacted>");
  }

  #[test]
  fn redact_query_values_empty_value_and_repeated_keys() {
    let out = redact_query_values("/p?a=&a=2&b=x");
    assert_eq!(out, "/p?a=<redacted>&a=<redacted>&b=<redacted>");
  }

  #[test]
  fn redact_query_values_non_empty_bare_segment_masked() {
    let out = redact_query_values("/p?a=1&flag&b=2");
    assert_eq!(out, "/p?a=<redacted>&<redacted>&b=<redacted>");
  }

  #[test]
  fn redact_query_values_question_only_unchanged() {
    let out = redact_query_values("/p?");
    assert!(matches!(out, Cow::Borrowed(_)), "trailing '?' with no query must borrow");
    assert_eq!(out, "/p?");
  }

  #[test]
  fn redact_query_values_leading_empty_segment_preserved() {
    let out = redact_query_values("/p?&a=1");
    assert_eq!(out, "/p?&a=<redacted>");
  }

  #[test]
  fn new_redacts_query_when_enabled() {
    let req = http::Request::builder()
      .method(Method::GET)
      .uri("https://example.com/reset?token=abc123&email=a@b.com")
      .body(())
      .unwrap();
    let log = HttpMessageLog::new(&req, true);
    assert_eq!(log.p_and_q, "/reset?token=<redacted>&email=<redacted>");
    let formatted = format!("{log}");
    assert!(!formatted.contains("abc123"), "token value leaked: {formatted}");
    assert!(!formatted.contains("a@b.com"), "email value leaked: {formatted}");
  }

  #[test]
  fn new_keeps_query_when_disabled() {
    let req = http::Request::builder()
      .uri("https://example.com/reset?token=abc123")
      .body(())
      .unwrap();
    let log = HttpMessageLog::new(&req, false);
    assert_eq!(log.p_and_q, "/reset?token=abc123");
  }

  #[test]
  fn upstream_redacted_when_enabled() {
    let req = http::Request::builder().uri("https://example.com/p").body(()).unwrap();
    let mut log = HttpMessageLog::new(&req, true);
    log.upstream(&"https://backend.local/api?key=s3cret".parse::<http::Uri>().unwrap());
    assert_eq!(log.upstream, "https://backend.local/api?key=<redacted>");
  }

  #[test]
  fn upstream_verbatim_when_disabled() {
    let req = http::Request::builder().uri("https://example.com/p").body(()).unwrap();
    let mut log = HttpMessageLog::new(&req, false);
    log.upstream(&"https://backend.local/api?key=s3cret".parse::<http::Uri>().unwrap());
    assert_eq!(log.upstream, "https://backend.local/api?key=s3cret");
  }

  #[test]
  fn upstream_redacted_without_query_is_unchanged() {
    // The rebuilt-from-parts path must reproduce the plain URL when there is no query.
    let req = http::Request::builder().uri("https://example.com/p").body(()).unwrap();
    let mut log = HttpMessageLog::new(&req, true);
    log.upstream(&"https://backend.local/api".parse::<http::Uri>().unwrap());
    assert_eq!(log.upstream, "https://backend.local/api");
  }
}
