use super::canonical_address::ToCanonical;
use crate::{
  backend::{UpstreamCandidates, UpstreamOption},
  log::*,
};
use anyhow::{Result, anyhow, ensure};
use bytes::BufMut;
use http::{HeaderMap, HeaderName, HeaderValue, Uri, header};
use std::{borrow::Cow, net::SocketAddr};

#[cfg(feature = "sticky-cookie")]
use crate::backend::{LoadBalanceContext, StickyCookie, StickyCookieValue};
// use crate::backend::{UpstreamGroup, UpstreamOption};

// ////////////////////////////////////////////////////
// // Functions to manipulate headers
#[cfg(feature = "sticky-cookie")]
/// Take sticky cookie header value from request header,
/// and returns LoadBalanceContext to be forwarded to LB if exist and if needed.
/// Removing sticky cookie is needed and it must not be passed to the upstream.
pub(super) fn takeout_sticky_cookie_lb_context(
  headers: &mut HeaderMap,
  expected_cookie_name: &str,
) -> Result<Option<LoadBalanceContext>> {
  let mut headers_clone = headers.clone();

  match headers_clone.entry(header::COOKIE) {
    header::Entry::Vacant(_) => Ok(None),
    header::Entry::Occupied(entry) => {
      let cookies_iter = entry
        .iter()
        .flat_map(|v| v.to_str().unwrap_or("").split(';').map(|v| v.trim()));
      let (sticky_cookies, without_sticky_cookies): (Vec<_>, Vec<_>) =
        cookies_iter.into_iter().partition(|v| v.starts_with(expected_cookie_name));
      if sticky_cookies.is_empty() {
        return Ok(None);
      }
      ensure!(sticky_cookies.len() == 1, "Invalid cookie: Multiple sticky cookie values");

      let cookies_passed_to_upstream = without_sticky_cookies.join("; ");
      let cookie_passed_to_lb = sticky_cookies.first().unwrap();
      headers.remove(header::COOKIE);
      headers.insert(header::COOKIE, cookies_passed_to_upstream.parse()?);

      let sticky_cookie = StickyCookie {
        value: StickyCookieValue::try_from(cookie_passed_to_lb, expected_cookie_name)?,
        info: None,
      };
      Ok(Some(LoadBalanceContext { sticky_cookie }))
    }
  }
}

#[cfg(feature = "sticky-cookie")]
/// Set-Cookie if LB Sticky is enabled and if cookie is newly created/updated.
/// Set-Cookie response header could be in multiple lines.
/// https://developer.mozilla.org/ja/docs/Web/HTTP/Headers/Set-Cookie
pub(super) fn set_sticky_cookie_lb_context(headers: &mut HeaderMap, context_from_lb: &LoadBalanceContext) -> Result<()> {
  let sticky_cookie_string: String = context_from_lb.sticky_cookie.clone().try_into()?;
  let new_header_val: HeaderValue = sticky_cookie_string.parse()?;
  let expected_cookie_name = &context_from_lb.sticky_cookie.value.name;
  match headers.entry(header::SET_COOKIE) {
    header::Entry::Vacant(entry) => {
      entry.insert(new_header_val);
    }
    header::Entry::Occupied(mut entry) => {
      let mut flag = false;
      for e in entry.iter_mut() {
        if e.to_str().unwrap_or("").starts_with(expected_cookie_name) {
          *e = new_header_val.clone();
          flag = true;
        }
      }
      if !flag {
        entry.append(new_header_val);
      }
    }
  };
  Ok(())
}

/// overwrite HOST value with upstream hostname (like 192.168.xx.x seen from rpxy)
fn override_host_header(headers: &mut HeaderMap, upstream_base_uri: &Uri) -> Result<()> {
  let mut upstream_host = upstream_base_uri
    .host()
    .ok_or_else(|| anyhow!("No hostname is given"))?
    .to_string();
  // add port if it is not default
  if let Some(port) = upstream_base_uri.port_u16() {
    upstream_host = format!("{}:{}", upstream_host, port);
  }

  // overwrite host header, this removes all the HOST header values
  headers.insert(header::HOST, HeaderValue::from_str(&upstream_host)?);
  Ok(())
}

/// Apply options to request header, which are specified in the configuration
pub(super) fn apply_upstream_options_to_header(
  headers: &mut HeaderMap,
  upstream_base_uri: &Uri,
  // _client_addr: &SocketAddr,
  upstream: &UpstreamCandidates,
) -> Result<()> {
  for opt in upstream.options.iter() {
    match opt {
      UpstreamOption::SetUpstreamHost => {
        // prioritize KeepOriginalHost
        if !upstream.options.contains(&UpstreamOption::KeepOriginalHost) {
          // overwrite host header, this removes all the HOST header values
          override_host_header(headers, upstream_base_uri)?;
        }
      }
      UpstreamOption::UpgradeInsecureRequests => {
        // add upgrade-insecure-requests in request header if not exist
        headers
          .entry(header::UPGRADE_INSECURE_REQUESTS)
          .or_insert(HeaderValue::from_bytes(b"1").unwrap());
      }
      _ => (),
    }
  }

  Ok(())
}

/// Append header entry with comma according to [RFC9110](https://datatracker.ietf.org/doc/html/rfc9110)
pub(super) fn append_header_entry_with_comma(headers: &mut HeaderMap, key: &str, value: &str) -> Result<()> {
  match headers.entry(HeaderName::from_bytes(key.as_bytes())?) {
    header::Entry::Vacant(entry) => {
      entry.insert(value.parse::<HeaderValue>()?);
    }
    header::Entry::Occupied(mut entry) => {
      // entry.append(value.parse::<HeaderValue>()?);
      let mut new_value = Vec::<u8>::with_capacity(entry.get().as_bytes().len() + 2 + value.len());
      new_value.put_slice(entry.get().as_bytes());
      new_value.put_slice(b", ");
      new_value.put_slice(value.as_bytes());
      entry.insert(HeaderValue::from_bytes(&new_value)?);
    }
  }

  Ok(())
}

/// Add header entry if not exist
pub(super) fn add_header_entry_if_not_exist(
  headers: &mut HeaderMap,
  key: impl Into<Cow<'static, str>>,
  value: impl Into<Cow<'static, str>>,
) -> Result<()> {
  match headers.entry(HeaderName::from_bytes(key.into().as_bytes())?) {
    header::Entry::Vacant(entry) => {
      entry.insert(value.into().parse::<HeaderValue>()?);
    }
    header::Entry::Occupied(_) => (),
  };

  Ok(())
}

/// Overwrite header entry if exist
pub(super) fn add_header_entry_overwrite_if_exist(
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

/// Add forwarding headers like `x-forwarded-for`.
pub(super) fn add_forwarding_header(
  headers: &mut HeaderMap,
  client_addr: &SocketAddr,
  listen_addr: &SocketAddr,
  tls: bool,
  uri_str: &str,
) -> Result<()> {
  // default process
  // optional process defined by upstream_option is applied in fn apply_upstream_options
  let canonical_client_addr = client_addr.to_canonical().ip().to_string();
  append_header_entry_with_comma(headers, "x-forwarded-for", &canonical_client_addr)?;

  // Single line cookie header
  // TODO: This should be only for HTTP/1.1. For 2+, this can be multi-lined.
  make_cookie_single_line(headers)?;

  /////////// As Nginx
  // If we receive X-Forwarded-Proto, pass it through; otherwise, pass along the
  // scheme used to connect to this server
  add_header_entry_if_not_exist(headers, "x-forwarded-proto", if tls { "https" } else { "http" })?;
  // If we receive X-Forwarded-Port, pass it through; otherwise, pass along the
  // server port the client connected to
  add_header_entry_if_not_exist(headers, "x-forwarded-port", listen_addr.port().to_string())?;

  /////////// As Nginx-Proxy
  // x-real-ip
  add_header_entry_overwrite_if_exist(headers, "x-real-ip", canonical_client_addr)?;
  // x-forwarded-ssl
  add_header_entry_overwrite_if_exist(headers, "x-forwarded-ssl", if tls { "on" } else { "off" })?;
  // x-original-uri
  add_header_entry_overwrite_if_exist(headers, "x-original-uri", uri_str.to_string())?;
  // proxy
  add_header_entry_overwrite_if_exist(headers, "proxy", "")?;

  Ok(())
}

/// Remove connection header
pub(super) fn remove_connection_header(headers: &mut HeaderMap) {
  if let Some(values) = headers.get(header::CONNECTION) {
    if let Ok(v) = values.clone().to_str() {
      for m in v.split(',') {
        if !m.is_empty() {
          headers.remove(m.trim());
        }
      }
    }
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
pub(super) fn remove_hop_header(headers: &mut HeaderMap) {
  HOP_HEADERS.iter().for_each(|key| {
    headers.remove(*key);
  });
}

/// Extract upgrade header value if exist
pub(super) fn extract_upgrade(headers: &HeaderMap) -> Option<String> {
  if let Some(c) = headers.get(header::CONNECTION) {
    if c
      .to_str()
      .unwrap_or("")
      .split(',')
      .any(|w| w.trim().eq_ignore_ascii_case(header::UPGRADE.as_str()))
    {
      if let Some(u) = headers.get(header::UPGRADE) {
        if let Ok(m) = u.to_str() {
          debug!("Upgrade in request header: {}", m);
          return Some(m.to_owned());
        }
      }
    }
  }
  None
}
