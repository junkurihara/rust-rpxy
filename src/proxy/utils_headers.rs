use super::{Upstream, UpstreamOption};
use crate::{error::*, log::*, utils::*};
use hyper::{
  header::{self, HeaderMap, HeaderValue},
  Uri,
};
use std::net::SocketAddr;

////////////////////////////////////////////////////
// Functions to manipulate headers

pub(super) fn apply_upstream_options_to_header(
  headers: &mut HeaderMap,
  _client_addr: SocketAddr,
  upstream_scheme_host: &Uri,
  upstream: &Upstream,
) -> Result<()> {
  for opt in upstream.opts.iter() {
    match opt {
      UpstreamOption::OverrideHost => {
        let upstream_host = upstream_scheme_host.host().ok_or_else(|| anyhow!("none"))?;
        headers
          .insert(header::HOST, HeaderValue::from_str(upstream_host)?)
          .ok_or_else(|| anyhow!("none"))?;
      }
    }
  }

  Ok(())
}

pub(super) fn append_header_entry(
  headers: &mut HeaderMap,
  key: &'static str,
  value: &str,
) -> Result<()> {
  match headers.entry(key) {
    header::Entry::Vacant(entry) => {
      entry.insert(value.parse::<HeaderValue>()?);
    }
    header::Entry::Occupied(mut entry) => {
      entry.append(value.parse::<HeaderValue>()?);
    }
  }

  Ok(())
}

pub(super) fn add_forwarding_header(
  headers: &mut HeaderMap,
  client_addr: SocketAddr,
) -> Result<()> {
  // default process
  // optional process defined by upstream_option is applied in fn apply_upstream_options
  append_header_entry(
    headers,
    "x-forwarded-for",
    &client_addr.to_canonical().ip().to_string(),
  )?;

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
