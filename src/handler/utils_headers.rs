use crate::{
  backend::{UpstreamGroup, UpstreamOption},
  error::*,
  log::*,
  utils::*,
};
use bytes::BufMut;
use hyper::{
  header::{self, HeaderMap, HeaderName, HeaderValue},
  Uri,
};
use std::net::SocketAddr;

////////////////////////////////////////////////////
// Functions to manipulate headers

pub(super) fn apply_upstream_options_to_header(
  headers: &mut HeaderMap,
  _client_addr: &SocketAddr,
  upstream: &UpstreamGroup,
  upstream_base_uri: &Uri,
) -> Result<()> {
  for opt in upstream.opts.iter() {
    match opt {
      UpstreamOption::OverrideHost => {
        // overwrite HOST value with upstream hostname (like 192.168.xx.x seen from rpxy)
        let upstream_host = upstream_base_uri.host().ok_or_else(|| anyhow!("none"))?;
        headers
          .insert(header::HOST, HeaderValue::from_str(upstream_host)?)
          .ok_or_else(|| anyhow!("none"))?;
      }
      UpstreamOption::UpgradeInsecureRequests => {
        // add upgrade-insecure-requests in request header if not exist
        headers
          .entry(header::UPGRADE_INSECURE_REQUESTS)
          .or_insert(HeaderValue::from_bytes(&[b'1']).unwrap());
      }
    }
  }

  Ok(())
}

// https://datatracker.ietf.org/doc/html/rfc9110
pub(super) fn append_header_entry_with_comma(headers: &mut HeaderMap, key: &str, value: &str) -> Result<()> {
  match headers.entry(HeaderName::from_bytes(key.as_bytes())?) {
    header::Entry::Vacant(entry) => {
      entry.insert(value.parse::<HeaderValue>()?);
    }
    header::Entry::Occupied(mut entry) => {
      // entry.append(value.parse::<HeaderValue>()?);
      let mut new_value = Vec::<u8>::with_capacity(entry.get().as_bytes().len() + 2 + value.len());
      new_value.put_slice(entry.get().as_bytes());
      new_value.put_slice(&[b',', b' ']);
      new_value.put_slice(value.as_bytes());
      entry.insert(HeaderValue::from_bytes(&new_value)?);
    }
  }

  Ok(())
}

pub(super) fn add_header_entry_if_not_exist(
  headers: &mut HeaderMap,
  key: impl Into<std::borrow::Cow<'static, str>>,
  value: impl Into<std::borrow::Cow<'static, str>>,
) -> Result<()> {
  match headers.entry(HeaderName::from_bytes(key.into().as_bytes())?) {
    header::Entry::Vacant(entry) => {
      entry.insert(value.into().parse::<HeaderValue>()?);
    }
    header::Entry::Occupied(_) => (),
  };

  Ok(())
}

pub(super) fn add_header_entry_overwrite_if_exist(
  headers: &mut HeaderMap,
  key: impl Into<std::borrow::Cow<'static, str>>,
  value: impl Into<std::borrow::Cow<'static, str>>,
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

pub(super) fn remove_hop_header(headers: &mut HeaderMap) {
  HOP_HEADERS.iter().for_each(|key| {
    headers.remove(*key);
  });
}

pub(super) fn extract_upgrade(headers: &HeaderMap) -> Option<String> {
  if let Some(c) = headers.get(header::CONNECTION) {
    if c
      .to_str()
      .unwrap_or("")
      .split(',')
      .into_iter()
      .any(|w| w.trim().to_ascii_lowercase() == header::UPGRADE.as_str().to_ascii_lowercase())
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
