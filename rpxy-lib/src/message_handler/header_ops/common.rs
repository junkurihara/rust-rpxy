use anyhow::{Result, anyhow};
use http::{HeaderMap, HeaderName, HeaderValue, Uri, header};
use std::borrow::Cow;

/// Overwrite header entry if exist
pub(in crate::message_handler) fn add_header_entry_overwrite_if_exist(
  headers: &mut HeaderMap,
  key: impl Into<Cow<'static, str>>,
  value: impl Into<Cow<'static, str>>,
) -> Result<()> {
  match headers.entry(HeaderName::from_bytes(key.into().as_bytes())?) {
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
/// pass `.as_ref()` to avoid cloning on the hot path.
pub(in crate::message_handler) fn host_from_uri_or_host_header(
  uri: &Uri,
  host_header_value: Option<&header::HeaderValue>,
) -> Result<String> {
  // Prioritize uri host over host header
  let uri_host = uri.host().map(|host| {
    if let Some(port) = uri.port_u16() {
      format!("{}:{}", host, port)
    } else {
      host.to_string()
    }
  });
  if let Some(host) = uri_host {
    return Ok(host);
  }
  // If uri host is not available, use host header
  host_header_value
    .map(|h| h.to_str().map(|s| s.to_string()))
    .transpose()?
    .ok_or_else(|| anyhow!("No host found in URI or Host header"))
}
