use anyhow::{Result, anyhow};
use http::{HeaderMap, HeaderName, HeaderValue, Uri, header};
use std::borrow::Cow;

/// Overwrite header entry if exist, taking a pre-built header name.
pub(in crate::message_handler) fn add_header_entry_overwrite_if_exist(
  headers: &mut HeaderMap,
  key: HeaderName,
  value: impl Into<Cow<'static, str>>,
) -> Result<()> {
  match headers.entry(key) {
    header::Entry::Vacant(entry) => {
      entry.insert(value.into().parse::<HeaderValue>()?);
    }
    header::Entry::Occupied(mut entry) => {
      entry.insert(HeaderValue::from_bytes(value.into().as_bytes())?);
    }
  }

  Ok(())
}

/// Align cookie values in single line
/// Sometimes violates [RFC6265](https://www.rfc-editor.org/rfc/rfc6265#section-5.4) (for http/1.1).
/// This is allowed in RFC7540 (for http/2) as mentioned [here](https://stackoverflow.com/questions/4843556/in-http-specification-what-is-the-string-that-separates-cookies).
pub(super) fn make_cookie_single_line(headers: &mut HeaderMap) -> Result<()> {
  // Zero or one Cookie line needs no merging (a single line is already single-line), which is the
  // overwhelmingly common case; handle it without scanning the whole header map or allocating.
  // Only when there are >= 2 lines do we build the joined buffer (sized exactly up front).
  let mut count = 0usize;
  let mut total = 0usize;
  for v in headers.get_all(header::COOKIE) {
    count += 1;
    total += v.len();
  }
  if count < 2 {
    return Ok(());
  }
  let mut joined = String::with_capacity(total + 2 * (count - 1));
  for (i, v) in headers.get_all(header::COOKIE).iter().enumerate() {
    if i > 0 {
      joined.push_str("; ");
    }
    joined.push_str(v.to_str().unwrap_or(""));
  }
  headers.remove(header::COOKIE);
  headers.insert(header::COOKIE, HeaderValue::from_bytes(joined.as_bytes())?);
  Ok(())
}

/// Extract host from URI, falling back to the `Host` header.
///
/// Takes the `Host` header by reference; callers that already own an `Option<HeaderValue>` can
/// pass `.as_ref()` to avoid cloning on the hot path. Returns a `Cow` borrowing from the inputs
/// so the common cases (URI host without port, Host-header fallback) do not allocate; only a
/// URI host carrying an explicit port is formatted into an owned `host:port` string.
pub(in crate::message_handler) fn host_from_uri_or_host_header<'a>(
  uri: &'a Uri,
  host_header_value: Option<&'a header::HeaderValue>,
) -> Result<Cow<'a, str>> {
  // Prioritize uri host over host header
  if let Some(host) = uri.host() {
    return Ok(match uri.port_u16() {
      Some(port) => Cow::Owned(format!("{}:{}", host, port)),
      None => Cow::Borrowed(host),
    });
  }
  // If uri host is not available, use host header
  host_header_value
    .map(|h| h.to_str().map(Cow::Borrowed))
    .transpose()?
    .ok_or_else(|| anyhow!("No host found in URI or Host header"))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn cookie_zero_lines_is_noop() {
    let mut headers = HeaderMap::new();
    make_cookie_single_line(&mut headers).unwrap();
    assert_eq!(headers.get_all(header::COOKIE).iter().count(), 0);
  }

  #[test]
  fn cookie_single_line_left_untouched() {
    let mut headers = HeaderMap::new();
    headers.append(header::COOKIE, HeaderValue::from_static("a=1; b=2"));
    make_cookie_single_line(&mut headers).unwrap();
    let values: Vec<_> = headers.get_all(header::COOKIE).iter().collect();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0], "a=1; b=2");
  }

  #[test]
  fn cookie_multiple_lines_merged_in_order() {
    let mut headers = HeaderMap::new();
    headers.append(header::COOKIE, HeaderValue::from_static("a=1"));
    headers.append(header::COOKIE, HeaderValue::from_static("b=2"));
    headers.append(header::COOKIE, HeaderValue::from_static("c=3"));
    make_cookie_single_line(&mut headers).unwrap();
    let values: Vec<_> = headers.get_all(header::COOKIE).iter().collect();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0], "a=1; b=2; c=3");
  }

  #[test]
  fn host_from_uri_without_port_borrows() {
    let uri: Uri = "https://example.com/path".parse().unwrap();
    let host = host_from_uri_or_host_header(&uri, None).unwrap();
    assert!(matches!(host, Cow::Borrowed(_)), "no-port URI host must borrow");
    assert_eq!(host, "example.com");
  }

  #[test]
  fn host_from_uri_with_port_owns() {
    let uri: Uri = "https://example.com:8443/path".parse().unwrap();
    let host = host_from_uri_or_host_header(&uri, None).unwrap();
    assert!(matches!(host, Cow::Owned(_)), "URI host with port must be owned");
    assert_eq!(host, "example.com:8443");
  }

  #[test]
  fn host_falls_back_to_host_header_borrowed() {
    let uri: Uri = "/path".parse().unwrap();
    let header_value = HeaderValue::from_static("fallback.example:8080");
    let host = host_from_uri_or_host_header(&uri, Some(&header_value)).unwrap();
    assert!(matches!(host, Cow::Borrowed(_)), "Host-header fallback must borrow");
    assert_eq!(host, "fallback.example:8080");
  }

  #[test]
  fn host_prefers_uri_over_host_header() {
    let uri: Uri = "https://uri.example/path".parse().unwrap();
    let header_value = HeaderValue::from_static("header.example");
    assert_eq!(host_from_uri_or_host_header(&uri, Some(&header_value)).unwrap(), "uri.example");
  }

  #[test]
  fn host_missing_everywhere_errors() {
    let uri: Uri = "/path".parse().unwrap();
    assert!(host_from_uri_or_host_header(&uri, None).is_err());
  }
}
