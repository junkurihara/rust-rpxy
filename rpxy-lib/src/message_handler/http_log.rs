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

/// Request URI captured for the access log.
///
/// `http::Uri` (and any `Scheme` / `Authority` / `PathAndQuery` extracted from it) shares the
/// original `Bytes` buffer, so holding one keeps the raw query alive in memory. To honor the
/// redaction guarantee, redaction-on captures only freshly-allocated query-free / redacted strings
/// and never retains the `Uri`.
#[derive(Debug, Clone)]
enum LoggedUri {
  /// Redaction disabled: keep the cheap `Uri` handle and render lazily in `Display`.
  Verbatim(http::Uri),
  /// Redaction enabled: query masked at capture time; no raw query bytes retained.
  Redacted { host: String, p_and_q: String, target: String },
}

/// Upstream URI captured for the access log; same Verbatim/Redacted split as `LoggedUri`.
#[derive(Debug, Clone)]
enum LoggedUpstream {
  Verbatim(http::Uri),
  Redacted(String),
}

/// Render an upstream URI with query values masked. For the common absolute form, rebuild from the
/// URI parts so the raw query is never copied verbatim; fall back to redacting the rendered URI for
/// other forms. The returned `String` does not alias the source `Uri` buffer.
fn redact_upstream(upstream: &http::Uri) -> String {
  match (upstream.scheme_str(), upstream.authority()) {
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
  }
}

/// Struct to log HTTP messages.
///
/// Fields hold cheap-to-clone source types: `Uri` / `HeaderValue` are `Bytes`-backed (clone is a
/// refcount bump), and `Method` is allocation-free for standard methods. String rendering is
/// deferred to `Display`, so a request whose access-log line is never emitted pays no formatting
/// cost. Query-bearing fields use `LoggedUri` / `LoggedUpstream` so that redaction-on never retains
/// raw query bytes.
#[derive(Debug, Clone)]
pub struct HttpMessageLog {
  client_addr: Option<SocketAddr>,
  method: http::Method,
  version: http::Version,
  // `Host` header, used as a fallback when the URI carries no authority (Verbatim case only).
  host_header: Option<header::HeaderValue>,
  ua: Option<header::HeaderValue>,
  xff: Option<header::HeaderValue>,
  forwarded: Option<header::HeaderValue>,
  status: Option<http::StatusCode>,
  uri: LoggedUri,
  upstream: Option<LoggedUpstream>,
  /// Whether query-string values are masked. Set at construction; consulted by the `upstream`
  /// setter (which runs after `new()`).
  redact_query: bool,
}

impl HttpMessageLog {
  /// Build an access-log record from a request. Source data is captured as cheap-clone handles;
  /// formatting happens lazily in `Display`, when (and only when) the log line is emitted. With
  /// redaction enabled, query values are masked here so no raw query bytes are retained.
  pub fn new<T>(req: &http::Request<T>, redact_query: bool) -> Self {
    let uri = if redact_query {
      // into_owned: redaction-on must never retain borrows of the request buffers.
      let host = header_ops::host_from_uri_or_host_header(req.uri(), req.headers().get(header::HOST))
        .map(Cow::into_owned)
        .unwrap_or_default();
      let p_and_q_raw = req.uri().path_and_query().map_or("", |v| v.as_str());
      let p_and_q = redact_query_values(p_and_q_raw).into_owned();
      let scheme = req.uri().scheme_str().unwrap_or("");
      let path = req.uri().path();
      let target = if !scheme.is_empty() && !host.is_empty() {
        format!("{scheme}://{host}{path}")
      } else {
        path.to_string()
      };
      LoggedUri::Redacted { host, p_and_q, target }
    } else {
      LoggedUri::Verbatim(req.uri().clone())
    };

    Self {
      client_addr: None,
      method: req.method().clone(),
      version: req.version(),
      host_header: req.headers().get(header::HOST).cloned(),
      ua: req.headers().get(header::USER_AGENT).cloned(),
      xff: req.headers().get(header_ops::header_defs::X_FORWARDED_FOR).cloned(),
      forwarded: req.headers().get(header::FORWARDED).cloned(),
      status: None,
      uri,
      upstream: None,
      redact_query,
    }
  }

  /// Derive `(host, path-and-query, target)` for the log line from the captured request URI.
  /// For `Verbatim`, host falls back to the `Host` header and the values are computed on demand;
  /// for `Redacted`, the precomputed (already-masked) strings are borrowed.
  fn render_request_uri(&self) -> (Cow<'_, str>, Cow<'_, str>, Cow<'_, str>) {
    match &self.uri {
      LoggedUri::Verbatim(uri) => {
        // The host Cow borrows from the captured Uri/HeaderValue in the common no-port case.
        let host = header_ops::host_from_uri_or_host_header(uri, self.host_header.as_ref()).unwrap_or_default();
        let p_and_q = uri.path_and_query().map_or("", |v| v.as_str());
        let scheme = uri.scheme_str().unwrap_or("");
        let path = uri.path();
        let target = if !scheme.is_empty() && !host.is_empty() {
          format!("{scheme}://{host}{path}")
        } else {
          path.to_string()
        };
        (host, Cow::Borrowed(p_and_q), Cow::Owned(target))
      }
      LoggedUri::Redacted { host, p_and_q, target } => (Cow::Borrowed(host), Cow::Borrowed(p_and_q), Cow::Borrowed(target)),
    }
  }
}

impl std::fmt::Display for HttpMessageLog {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let (host, p_and_q, target) = self.render_request_uri();

    let ua = self.ua.as_ref().and_then(|h| h.to_str().ok()).unwrap_or("");
    let xff = self.xff.as_ref().and_then(|h| h.to_str().ok()).unwrap_or("");
    let forwarded = self.forwarded.as_ref().and_then(|h| h.to_str().ok()).unwrap_or("");
    let forwarded_part = if !forwarded.is_empty() {
      format!(" \"{forwarded}\"")
    } else {
      String::new()
    };

    let client_addr = self.client_addr.map(|a| a.to_string()).unwrap_or_default();
    let status = self.status.map(|s| s.to_string()).unwrap_or_default();
    let upstream: Cow<'_, str> = match &self.upstream {
      None => Cow::Borrowed(""),
      Some(LoggedUpstream::Verbatim(u)) => Cow::Owned(u.to_string()),
      Some(LoggedUpstream::Redacted(s)) => Cow::Borrowed(s),
    };

    write!(
      f,
      "{} <- {} -- {} {} {:?} -- {} -- {} \"{}\", \"{}\"{} \"{}\"",
      host, client_addr, self.method, p_and_q, self.version, status, target, ua, xff, forwarded_part, upstream
    )
  }
}

impl HttpMessageLog {
  pub fn client_addr(&mut self, client_addr: &SocketAddr) -> &mut Self {
    self.client_addr = Some(client_addr.to_canonical());
    self
  }
  pub fn status_code(&mut self, status_code: &http::StatusCode) -> &mut Self {
    self.status = Some(*status_code);
    self
  }
  pub fn xff(&mut self, xff: &Option<&header::HeaderValue>) -> &mut Self {
    self.xff = (*xff).cloned();
    self
  }
  pub fn upstream(&mut self, upstream: &http::Uri) -> &mut Self {
    self.upstream = Some(if self.redact_query {
      LoggedUpstream::Redacted(redact_upstream(upstream))
    } else {
      LoggedUpstream::Verbatim(upstream.clone())
    });
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
  use http::{HeaderValue, Method, StatusCode, Version};

  // Build a log record with the same field values the two format tests share. `forwarded` and the
  // trailing setters are left to the caller.
  fn sample_log() -> HttpMessageLog {
    HttpMessageLog {
      client_addr: Some("192.168.1.1:8080".parse().unwrap()),
      method: Method::GET,
      version: Version::HTTP_11,
      host_header: None,
      ua: Some(HeaderValue::from_static("Mozilla/5.0")),
      xff: Some(HeaderValue::from_static("10.0.0.1")),
      forwarded: None,
      status: Some(StatusCode::OK),
      uri: LoggedUri::Verbatim("https://example.com/path?query=value".parse().unwrap()),
      // Production upstreams always carry a path; the explicit path also avoids `Uri::to_string`
      // normalizing an authority-only URI to a trailing "/".
      upstream: Some(LoggedUpstream::Verbatim("https://backend.example.com/api".parse().unwrap())),
      redact_query: false,
    }
  }

  #[test]
  fn test_log_format_without_forwarded() {
    let log = sample_log();

    let formatted = format!("{}", log);
    assert!(!formatted.contains(" \"\""));
    assert!(formatted.contains("\"Mozilla/5.0\", \"10.0.0.1\" \"https://backend.example.com/api\""));
  }

  #[test]
  fn test_log_format_with_forwarded() {
    let log = HttpMessageLog {
      forwarded: Some(HeaderValue::from_static("for=192.0.2.60;proto=http;by=203.0.113.43")),
      ..sample_log()
    };

    let formatted = format!("{}", log);
    assert!(formatted.contains(" \"for=192.0.2.60;proto=http;by=203.0.113.43\""));
    assert!(formatted.contains(
      "\"Mozilla/5.0\", \"10.0.0.1\" \"for=192.0.2.60;proto=http;by=203.0.113.43\" \"https://backend.example.com/api\""
    ));
  }

  // Pin the entire access-log line, built through the production path (`new()` + setters), so the
  // byte-exact format - including the `status` segment, which renders via `StatusCode::Display`
  // (e.g. "200 OK") - is guarded against future drift.
  #[test]
  fn full_line_equivalence_via_new_and_setters() {
    let req = http::Request::builder()
      .method(Method::GET)
      .uri("https://example.com/path?query=value")
      .header(http::header::USER_AGENT, "Mozilla/5.0")
      .body(())
      .unwrap();
    let mut log = HttpMessageLog::new(&req, false);
    log.client_addr(&"192.168.1.1:8080".parse().unwrap());
    log.xff(&Some(&HeaderValue::from_static("10.0.0.1")));
    log.upstream(&"https://backend.example.com/path?query=value".parse().unwrap());
    log.status_code(&StatusCode::OK);

    assert_eq!(
      format!("{log}"),
      "example.com <- 192.168.1.1:8080 -- GET /path?query=value HTTP/1.1 -- 200 OK -- https://example.com/path \"Mozilla/5.0\", \"10.0.0.1\" \"https://backend.example.com/path?query=value\""
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
    let formatted = format!("{log}");
    assert!(
      formatted.contains("/reset?token=<redacted>&email=<redacted>"),
      "redacted path-and-query missing: {formatted}"
    );
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
    assert!(format!("{log}").contains("/reset?token=abc123"));
  }

  #[test]
  fn upstream_redacted_when_enabled() {
    let req = http::Request::builder().uri("https://example.com/p").body(()).unwrap();
    let mut log = HttpMessageLog::new(&req, true);
    log.upstream(&"https://backend.local/api?key=s3cret".parse::<http::Uri>().unwrap());
    let formatted = format!("{log}");
    assert!(
      formatted.contains("https://backend.local/api?key=<redacted>"),
      "redacted upstream missing: {formatted}"
    );
    assert!(!formatted.contains("s3cret"), "upstream query value leaked: {formatted}");
  }

  #[test]
  fn upstream_verbatim_when_disabled() {
    let req = http::Request::builder().uri("https://example.com/p").body(()).unwrap();
    let mut log = HttpMessageLog::new(&req, false);
    log.upstream(&"https://backend.local/api?key=s3cret".parse::<http::Uri>().unwrap());
    assert!(format!("{log}").contains("https://backend.local/api?key=s3cret"));
  }

  #[test]
  fn upstream_redacted_without_query_is_unchanged() {
    // The rebuilt-from-parts path must reproduce the plain URL when there is no query.
    let req = http::Request::builder().uri("https://example.com/p").body(()).unwrap();
    let mut log = HttpMessageLog::new(&req, true);
    log.upstream(&"https://backend.local/api".parse::<http::Uri>().unwrap());
    assert!(format!("{log}").contains("https://backend.local/api"));
  }
}
