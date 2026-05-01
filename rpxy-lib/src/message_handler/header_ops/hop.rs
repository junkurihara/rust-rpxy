use http::{HeaderMap, header};

use crate::log::*;

/// Remove connection header
pub(in crate::message_handler) fn remove_connection_header(headers: &mut HeaderMap) {
  let Some(values) = headers.get(header::CONNECTION).cloned() else {
    return;
  };

  let Ok(v) = values.to_str() else {
    return;
  };

  let keys = v.split(',').map(|m| m.trim()).filter(|m| !m.is_empty());
  for m in keys {
    headers.remove(m);
  }
}

/// Hop header values which are removed at proxy
const HOP_HEADERS: &[&str] = &[
  "connection",
  "te",
  "trailer",
  "keep-alive",
  "proxy-connection",
  "proxy-authenticate",
  "proxy-authorization",
  "transfer-encoding",
  "upgrade",
];

/// Remove hop headers
pub(in crate::message_handler) fn remove_hop_header(headers: &mut HeaderMap) {
  HOP_HEADERS.iter().for_each(|key| {
    headers.remove(*key);
  });
}

/// Extract upgrade header value if exist
pub(in crate::message_handler) fn extract_upgrade(headers: &HeaderMap) -> Option<String> {
  let Ok(connection_header) = headers.get(header::CONNECTION)?.to_str() else {
      return None;
  };

  let connection_header_contains_upgrade = connection_header.split(',')
    .any(|w| w.trim().eq_ignore_ascii_case(header::UPGRADE.as_str()));
  if !connection_header_contains_upgrade {
      return None;
  }

  if let Ok(m) = headers.get(header::UPGRADE)?.to_str() {
    debug!("Upgrade in request header: {}", m);
    return Some(m.to_owned());
  }

  None
}
