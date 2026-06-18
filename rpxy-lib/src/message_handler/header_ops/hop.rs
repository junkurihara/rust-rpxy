use http::{HeaderMap, header, header::HeaderName};

use crate::log::*;

/// Remove the `Connection` header and every hop-by-hop header it lists.
///
/// RFC 9110 §7.6.1: the `Connection` field-value enumerates header names that apply only to
/// the immediate hop and must not be forwarded. The `Connection` header itself is also
/// hop-by-hop. We remove it from `headers` up front - this yields an owned `HeaderValue`
/// whose borrow is independent of `headers`, so we can mutate the map freely while iterating
/// the listed names, without cloning the value. `remove_hop_header` is always called by
/// callers right after this function and lists `header::CONNECTION` in `HOP_HEADERS` as
/// defensive redundancy.
pub(in crate::message_handler) fn remove_connection_header(headers: &mut HeaderMap) {
  let Some(values) = headers.remove(header::CONNECTION) else {
    return;
  };
  let Ok(v) = values.to_str() else { return };
  for m in v.split(',').map(|m| m.trim()).filter(|m| !m.is_empty()) {
    headers.remove(m);
  }
}

/// Hop-by-hop header names which are removed at proxy.
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

/// Extract the `Upgrade` header value when `Connection` lists `upgrade`.
pub(in crate::message_handler) fn extract_upgrade(headers: &HeaderMap) -> Option<String> {
  let c = headers.get(header::CONNECTION)?;
  let has_upgrade_token = c
    .to_str()
    .unwrap_or("")
    .split(',')
    .any(|w| w.trim().eq_ignore_ascii_case(header::UPGRADE.as_str()));
  if !has_upgrade_token {
    return None;
  }
  let m = headers.get(header::UPGRADE)?.to_str().ok()?;
  debug!("Upgrade in request header: {}", m);
  Some(m.to_owned())
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

  /// Names listed in `Connection` are removed; unrelated headers (e.g. `Content-Type`) are
  /// kept. Pins the core listed-name removal behaviour.
  #[test]
  fn remove_connection_header_drops_listed_names_and_keeps_others() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive, X-Custom"));
    h.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    h.insert("x-custom", HeaderValue::from_static("v"));
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));

    remove_connection_header(&mut h);

    assert!(!h.contains_key("keep-alive"), "listed name `keep-alive` must be removed");
    assert!(!h.contains_key("x-custom"), "listed name `x-custom` must be removed");
    assert!(h.contains_key(header::CONTENT_TYPE), "unrelated header must survive");
  }

  /// Mixed-case names in `Connection` match the (always-lowercased) `HeaderName`s in the
  /// map. Pins reliance on the `http` crate's case-insensitive name normalisation.
  #[test]
  fn remove_connection_header_is_case_insensitive() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_static("Keep-Alive"));
    h.insert("keep-alive", HeaderValue::from_static("timeout=5"));

    remove_connection_header(&mut h);

    assert!(!h.contains_key("keep-alive"), "mixed-case listed name must still be removed");
  }

  /// Extra whitespace and empty segments between commas are tolerated; both real targets
  /// are removed, and the function does not panic.
  #[test]
  fn remove_connection_header_handles_whitespace_and_empties() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive ,  , x-custom"));
    h.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    h.insert("x-custom", HeaderValue::from_static("v"));
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));

    remove_connection_header(&mut h);

    assert!(!h.contains_key("keep-alive"));
    assert!(!h.contains_key("x-custom"));
    assert!(h.contains_key(header::CONTENT_TYPE));
  }

  /// Absent `Connection` header: early return, no panic, no mutation.
  #[test]
  fn remove_connection_header_does_nothing_on_absent_header() {
    let mut h = HeaderMap::new();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));

    remove_connection_header(&mut h);

    assert_eq!(h.len(), 1);
    assert!(h.contains_key(header::CONTENT_TYPE));
  }

  /// An unparseable name in the `Connection` list (e.g. one containing a space, which is
  /// not a valid header-name token) is silently skipped; valid names alongside it are still
  /// removed. Matches the previous `headers.remove(&str)` behaviour, which no-ops on parse
  /// failure.
  #[test]
  fn remove_connection_header_ignores_unparseable_names() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive, has space"));
    h.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));

    remove_connection_header(&mut h);

    assert!(!h.contains_key("keep-alive"), "valid listed name must be removed");
    assert!(h.contains_key(header::CONTENT_TYPE), "unrelated header must survive");
  }

  /// Contract widening (vs. the previous shape that only removed listed names): the
  /// `Connection` header itself is now also removed by this function. Today both call
  /// sites pair this function with `remove_hop_header` (which also lists `CONNECTION`), so
  /// end-to-end transfer is unchanged - but pin the new contract so a future direct caller
  /// reading this in isolation gets the same guarantee.
  #[test]
  fn remove_connection_header_also_removes_connection_itself() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_static("close"));

    remove_connection_header(&mut h);

    assert!(!h.contains_key(header::CONNECTION), "Connection itself must be removed");
  }

  #[test]
  fn extract_upgrade_returns_value_when_connection_lists_upgrade() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive, Upgrade"));
    h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    assert_eq!(extract_upgrade(&h).as_deref(), Some("websocket"));
  }

  #[test]
  fn extract_upgrade_returns_none_when_connection_absent() {
    let mut h = HeaderMap::new();
    h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    assert_eq!(extract_upgrade(&h), None);
  }

  #[test]
  fn extract_upgrade_returns_none_when_no_upgrade_token() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    assert_eq!(extract_upgrade(&h), None);
  }

  #[test]
  fn extract_upgrade_returns_none_when_upgrade_header_absent() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    assert_eq!(extract_upgrade(&h), None);
  }

  #[test]
  fn extract_upgrade_returns_none_for_non_utf8_connection() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_bytes(b"Upgrade, \xff").unwrap());
    h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    assert_eq!(extract_upgrade(&h), None);
  }

  #[test]
  fn extract_upgrade_returns_none_for_non_utf8_upgrade() {
    let mut h = HeaderMap::new();
    h.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    h.insert(header::UPGRADE, HeaderValue::from_bytes(b"\xff").unwrap());
    assert_eq!(extract_upgrade(&h), None);
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
