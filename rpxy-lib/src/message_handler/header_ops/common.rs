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
  let cookies = headers
    .iter()
    .filter(|(k, _)| **k == header::COOKIE)
    .map(|(_, v)| v.to_str().unwrap_or(""))
    .collect::<Vec<_>>()
    .join("; ");
  if !cookies.is_empty() {
    headers.remove(header::COOKIE);
    headers.insert(header::COOKIE, HeaderValue::from_bytes(cookies.as_bytes())?);
  }
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
