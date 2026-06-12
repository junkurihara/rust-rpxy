use http::{HeaderMap, header, header::HeaderName};

use crate::log::*;

/// Remove connection header
pub(in crate::message_handler) fn remove_connection_header(headers: &mut HeaderMap) {
  if let Some(values) = headers.get(header::CONNECTION) {
    if let Ok(v) = values.clone().to_str() {
      let keys = v.split(',').map(|m| m.trim()).filter(|m| !m.is_empty());
      for m in keys {
        headers.remove(m);
      }
    }
  }
}

/// Hop header values which are removed at proxy.
/// Pre-built as `HeaderName`s so that `HeaderMap::remove` does not have to
/// parse and hash a string key on every request.
static HOP_HEADERS: [HeaderName; 9] = [
  header::CONNECTION,
  header::TE,
  header::TRAILER,
  HeaderName::from_static("keep-alive"),
  HeaderName::from_static("proxy-connection"),
  header::PROXY_AUTHENTICATE,
  header::PROXY_AUTHORIZATION,
  header::TRANSFER_ENCODING,
  header::UPGRADE,
];

/// Remove hop headers
pub(in crate::message_handler) fn remove_hop_header(headers: &mut HeaderMap) {
  HOP_HEADERS.iter().for_each(|key| {
    headers.remove(key);
  });
}

/// Extract upgrade header value if exist
pub(in crate::message_handler) fn extract_upgrade(headers: &HeaderMap) -> Option<String> {
  if let Some(c) = headers.get(header::CONNECTION) {
    if c
      .to_str()
      .unwrap_or("")
      .split(',')
      .any(|w| w.trim().eq_ignore_ascii_case(header::UPGRADE.as_str()))
    {
      if let Some(Ok(m)) = headers.get(header::UPGRADE).map(|u| u.to_str()) {
        debug!("Upgrade in request header: {}", m);
        return Some(m.to_owned());
      }
    }
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;
  use http::HeaderValue;

  #[test]
  fn removes_all_hop_headers_case_insensitively() {
    let mut h = HeaderMap::new();
    // mixed-case names: the http crate normalizes header names to lowercase at
    // parse time, so removal via `HeaderName` keys must match these as well
    h.insert("Connection", HeaderValue::from_static("keep-alive"));
    h.insert("TE", HeaderValue::from_static("trailers"));
    h.insert("Trailer", HeaderValue::from_static("expires"));
    h.insert("KEEP-ALIVE", HeaderValue::from_static("timeout=5"));
    h.insert("Proxy-Connection", HeaderValue::from_static("keep-alive"));
    h.insert("Proxy-Authenticate", HeaderValue::from_static("Basic"));
    h.insert("PROXY-AUTHORIZATION", HeaderValue::from_static("Basic dXNlcjpwdw=="));
    h.insert("Transfer-Encoding", HeaderValue::from_static("chunked"));
    h.insert("Upgrade", HeaderValue::from_static("websocket"));

    remove_hop_header(&mut h);

    assert!(h.is_empty(), "all hop headers should be removed: {h:?}");
  }

  #[test]
  fn removes_from_static_built_names() {
    // regression guard for the two names without standard constants in http::header
    let mut h = HeaderMap::new();
    h.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    h.insert("proxy-connection", HeaderValue::from_static("keep-alive"));

    remove_hop_header(&mut h);

    assert!(h.is_empty(), "from_static-built hop headers should be removed: {h:?}");
  }

  #[test]
  fn keeps_unrelated_headers() {
    let mut h = HeaderMap::new();
    h.insert(header::HOST, HeaderValue::from_static("example.com"));
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    h.insert(header::CONNECTION, HeaderValue::from_static("close"));

    remove_hop_header(&mut h);

    assert_eq!(h.len(), 2, "unrelated headers should survive: {h:?}");
    assert!(h.contains_key(header::HOST));
    assert!(h.contains_key(header::CONTENT_TYPE));
    assert!(!h.contains_key(header::CONNECTION));
  }
}
