use http::{HeaderMap, HeaderName, header};

/// Request headers whose values are masked in DEBUG logs.
/// These are the IANA standard credential-bearing headers; they are provided as
/// `http::header` constants, so the denylist stays closed and typo-free.
const SENSITIVE_HEADERS: [HeaderName; 3] = [header::AUTHORIZATION, header::PROXY_AUTHORIZATION, header::COOKIE];

/// Wraps a `&HeaderMap` for DEBUG logging of requests to be forwarded.
///
/// When `redact` is true (the default), the values of credential-bearing
/// headers (`Authorization` / `Cookie` / `Proxy-Authorization`) are replaced
/// with a `<redacted>` placeholder while the header names stay visible for
/// diagnostics. When `redact` is false (operator opted in via the env var
/// `RPXY_UNSAFE_DEBUG_HEADERS`), all values are printed verbatim.
pub(crate) struct DebugHeaders<'a> {
  headers: &'a HeaderMap,
  redact: bool,
}

impl<'a> DebugHeaders<'a> {
  /// Build a debug view of `headers`. `unsafe_debug_headers` is the
  /// operator-facing opt-out (env `RPXY_UNSAFE_DEBUG_HEADERS`); when it is
  /// `false` (the default) credential headers are redacted. The inversion is
  /// localized here so the call site reads as "log these headers, honoring the
  /// unsafe-debug flag" without a bare `!` at the use point.
  pub(crate) fn new(headers: &'a HeaderMap, unsafe_debug_headers: bool) -> Self {
    Self {
      headers,
      redact: !unsafe_debug_headers,
    }
  }
}

impl std::fmt::Debug for DebugHeaders<'_> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let mut m = f.debug_map();
    for (name, value) in self.headers.iter() {
      // `HeaderMap::iter()` repeats the name for multi-value headers, so each
      // value of a repeated sensitive header is masked individually.
      if self.redact && SENSITIVE_HEADERS.contains(name) {
        m.entry(&name.as_str(), &"<redacted>");
      } else {
        m.entry(&name.as_str(), &value);
      }
    }
    m.finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use http::HeaderValue;

  fn build_headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer supersecret"));
    h.insert(header::PROXY_AUTHORIZATION, HeaderValue::from_static("Basic dXNlcjpwdw=="));
    h.append(header::COOKIE, HeaderValue::from_static("sid=abc"));
    h.append(header::COOKIE, HeaderValue::from_static("pref=dark"));
    h.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.1"));
    h
  }

  #[test]
  fn redacts_sensitive_values_and_keeps_benign() {
    let headers = build_headers();
    let rendered = format!("{:?}", DebugHeaders::new(&headers, false));

    // sensitive values are masked
    assert!(!rendered.contains("supersecret"), "authorization value leaked: {rendered}");
    assert!(!rendered.contains("dXNlcjpwdw=="), "proxy-authorization value leaked: {rendered}");
    assert!(!rendered.contains("sid=abc"), "cookie value leaked: {rendered}");
    assert!(!rendered.contains("pref=dark"), "second cookie value leaked: {rendered}");
    assert!(rendered.contains("<redacted>"), "expected redaction placeholder: {rendered}");

    // header names remain visible for diagnostics
    assert!(rendered.contains("authorization"), "authorization name missing: {rendered}");
    assert!(rendered.contains("cookie"), "cookie name missing: {rendered}");

    // benign header is untouched
    assert!(rendered.contains("203.0.113.1"), "benign header was masked: {rendered}");
  }

  #[test]
  fn multi_value_cookie_masks_every_entry() {
    let headers = build_headers();
    let rendered = format!("{:?}", DebugHeaders::new(&headers, false));
    // both cookie entries replaced; placeholder appears at least twice for cookie
    let redacted_count = rendered.matches("<redacted>").count();
    // authorization + proxy-authorization + 2 cookie entries = 4 masked values
    assert_eq!(redacted_count, 4, "expected 4 masked values: {rendered}");
  }

  #[test]
  fn unsafe_opt_out_prints_values_verbatim() {
    let headers = build_headers();
    let rendered = format!("{:?}", DebugHeaders::new(&headers, true));
    assert!(rendered.contains("supersecret"), "opt-out must print authorization: {rendered}");
    assert!(rendered.contains("sid=abc"), "opt-out must print cookie: {rendered}");
    assert!(!rendered.contains("<redacted>"), "opt-out must not redact: {rendered}");
  }
}
