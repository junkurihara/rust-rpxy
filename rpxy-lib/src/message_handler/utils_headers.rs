use super::canonical_address::ToCanonical;
use crate::{
  backend::{UpstreamCandidates, UpstreamOption},
  log::*,
};
use anyhow::{Result, anyhow};
use http::{HeaderMap, HeaderName, HeaderValue, Uri, header, header::AsHeaderName};
use ipnet::IpNet;
use std::{
  borrow::Cow,
  net::{IpAddr, SocketAddr},
  str::FromStr,
};

#[cfg(feature = "sticky-cookie")]
use crate::backend::{LoadBalanceContext, StickyCookie, StickyCookieValue};
// use crate::backend::{UpstreamGroup, UpstreamOption};

const X_FORWARDED_FOR: &str = "x-forwarded-for";
const X_FORWARDED_PROTO: &str = "x-forwarded-proto";
const X_FORWARDED_PORT: &str = "x-forwarded-port";
const X_FORWARDED_SSL: &str = "x-forwarded-ssl";
const X_ORIGINAL_URI: &str = "x-original-uri";
const X_REAL_IP: &str = "x-real-ip";

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
      anyhow::ensure!(sticky_cookies.len() == 1, "Invalid cookie: Multiple sticky cookie values");

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
/// This function is called after almost all other headers has been set and updated.
pub(super) fn apply_upstream_options_to_header(
  headers: &mut HeaderMap,
  upstream_base_uri: &Uri,
  // _client_addr: &SocketAddr,
  upstream: &UpstreamCandidates,
  trusted_forwarded_proxies: &[IpNet],
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
      UpstreamOption::ForwardedHeader => {
        // This is called after X-Forwarded-For is added to generate RFC 7239 Forwarded header from it.
        // If Forwarded already exists, it has already been normalized by add_forwarding_header().
        if !headers.contains_key(header::FORWARDED) {
          let Some(forwarding_chain) = extract_forwarding_chain_from_headers(headers)? else {
            warn!("Failed to generate Forwarded header: no X-Forwarded-For information found in headers");
            continue;
          };
          let normalized_chain = reduce_trusted_proxy_chain(forwarding_chain, trusted_forwarded_proxies);
          match generate_forwarded_header(&normalized_chain) {
            Ok(forwarded_value) => {
              add_header_entry_overwrite_if_exist(headers, header::FORWARDED.as_str(), forwarded_value)?;
            }
            Err(e) => {
              // Log warning but don't fail the request if Forwarded generation fails
              warn!("Failed to generate Forwarded header: {}", e);
            }
          }
        }
      }
      _ => (),
    }
  }

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

/// An entry in Forwarded header with only the parameters relevant for forwarding chain normalization and consistency check.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ForwardedNode {
  Ip(IpAddr, Option<u16>),
  Unknown(Option<String>),
  Obfuscated(String, Option<String>),
}

impl ForwardedNode {
  fn ip_addr(&self) -> Option<IpAddr> {
    match self {
      Self::Ip(ip, _) => Some(*ip),
      Self::Unknown(_) | Self::Obfuscated(_, _) => None,
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ForwardedEntry {
  /// for=:
  /// For Forwarded Header: this is optional in the RFC, but we require it for each entry to be able to build the forwarding chain. If missing, the entry is considered invalid.
  for_node: ForwardedNode,
  /// proto=, When generated from incoming X-Forwarded-For, this is always None except for the last entry which was generated by the previous hop proxy. This is used for consistency check against X-Forwarded-Proto.
  proto: Option<String>,
  /// host=, When generated from incoming X-Forwarded-For, this is always None
  host: Option<String>,
  /// by=, When generated from incoming X-Forwarded-For, this is always None.
  by: Option<String>,
}

/// Add or update forwarding headers like `x-forwarded-for`.
/// If only `forwarded` header exists, it will update `x-forwarded-for` with the proxy chain.
/// If both `x-forwarded-for` and `forwarded` headers exist, it will update `x-forwarded-for` first and then add `forwarded` header.
/// If the immediate peer is in trusted_forwarded_proxies,
/// it will recursively trust the incoming forwarding headers and append the immediate peer IP to the end of the chain.
/// Otherwise, it will ignore incoming forwarding headers and start a new chain with the immediate peer IP.
pub(super) fn add_forwarding_header(
  headers: &mut HeaderMap,
  client_addr: &SocketAddr,
  listen_addr: &SocketAddr,
  tls: bool,
  original_uri: &Uri,
  trusted_forwarded_proxies: &[IpNet],
) -> Result<()> {
  let peer_ip = canonicalize_ip(client_addr.to_canonical().ip());
  let has_forwarded = headers.contains_key(header::FORWARDED);
  let normalized_chain = normalize_forwarding_chain(headers, &peer_ip, tls, original_uri, trusted_forwarded_proxies);
  let peer_entry = build_peer_forwarded_entry(headers, peer_ip, tls, original_uri);

  let (normalized_chain, normalized_for_ip_chain) = match forwarded_chain_to_xff(&normalized_chain) {
    Some(ip_chain) => (normalized_chain, ip_chain),
    None => {
      warn!("Normalized forwarding chain contains non-IP hops; falling back to peer-only forwarding view");
      (vec![peer_entry.clone()], vec![peer_ip.to_string()])
    }
  };

  // For X-Real-IP
  let authoritative_client_ip = normalized_chain
    .first()
    .and_then(|entry| entry.for_node.ip_addr())
    .map(|ip| ip.to_string())
    .unwrap_or_else(|| peer_ip.to_string());

  // Update X-Forwarded-For with normalized chain
  overwrite_header_with_csv(headers, X_FORWARDED_FOR, &normalized_for_ip_chain)?;

  // Preserve/update Forwarded only if it was present on input. If callers want to force
  // Forwarded generation, apply_upstream_options_to_header() will regenerate it later.
  if has_forwarded {
    match generate_forwarded_header(&normalized_chain) {
      Ok(forwarded_value) => add_header_entry_overwrite_if_exist(headers, header::FORWARDED.as_str(), forwarded_value)?,
      Err(e) => warn!("Failed to update existing Forwarded header for consistency: {}", e),
    }
  } else {
    headers.remove(header::FORWARDED);
  }

  // Single line cookie header
  // TODO: This should be only for HTTP/1.1. For 2+, this can be multi-lined.
  make_cookie_single_line(headers)?;

  // Always overwrite these headers with rpxy's authoritative downstream view.
  add_header_entry_overwrite_if_exist(headers, X_FORWARDED_PROTO, if tls { "https" } else { "http" })?;
  add_header_entry_overwrite_if_exist(headers, X_FORWARDED_PORT, listen_addr.port().to_string())?;

  /////////// As Nginx-Proxy
  // x-real-ip
  add_header_entry_overwrite_if_exist(headers, X_REAL_IP, authoritative_client_ip)?;
  // x-forwarded-ssl
  add_header_entry_overwrite_if_exist(headers, X_FORWARDED_SSL, if tls { "on" } else { "off" })?;
  // x-original-uri
  add_header_entry_overwrite_if_exist(headers, X_ORIGINAL_URI, original_uri.to_string())?;
  // proxy
  add_header_entry_overwrite_if_exist(headers, "proxy", "")?;

  Ok(())
}

/// Normalize forwarding chain based on trusted proxies configuration.
fn normalize_forwarding_chain(
  headers: &HeaderMap,
  peer_ip: &IpAddr,
  tls: bool,
  original_uri: &Uri,
  trusted_forwarded_proxies: &[IpNet],
) -> Vec<ForwardedEntry> {
  let peer_entry = build_peer_forwarded_entry(headers, *peer_ip, tls, original_uri);
  if !is_trusted_proxy(peer_ip, trusted_forwarded_proxies) {
    return vec![peer_entry];
  }

  let mut forwarding_chain = match extract_forwarding_chain_from_headers(headers) {
    Ok(Some(chain)) if !chain.is_empty() => chain,
    Ok(_) => {
      return vec![peer_entry];
    }
    Err(e) => {
      warn!("Ignoring incoming forwarding headers from trusted proxy due to parse failure: {e}");
      return vec![peer_entry];
    }
  };

  // Append the immediate peer as the last hop in the chain, which is the authoritative view of this proxy.
  forwarding_chain.push(peer_entry.clone());
  let normalized_chain = reduce_trusted_proxy_chain(forwarding_chain, trusted_forwarded_proxies);
  if normalized_chain.is_empty() {
    return vec![peer_entry];
  }
  normalized_chain
}

/// Extract forwarding information chain from Forwarded or X-Forwarded-For/Proto headers.
/// Returns None if neither header is present.
/// If both are present, prefer Forwarded only when it is consistent with the auxiliary X-Forwarded-* view.
fn extract_forwarding_chain_from_headers(headers: &HeaderMap) -> Result<Option<Vec<ForwardedEntry>>> {
  let xff_chain = if headers.contains_key(X_FORWARDED_FOR) {
    Some(parse_x_forwarded_for_header(headers)?)
  } else {
    None
  };
  let forwarded_chain = if headers.contains_key(header::FORWARDED) {
    match parse_forwarded_header(headers) {
      Ok(chain) => Some(chain),
      Err(e) if xff_chain.is_some() => {
        warn!("Ignoring invalid Forwarded header and falling back to X-Forwarded-For: {e}");
        None
      }
      Err(e) => return Err(e),
    }
  } else {
    None
  };

  match (forwarded_chain, xff_chain) {
    (Some(forwarded), Some(xff)) => {
      if forwarded_is_consistent(&forwarded, &xff) {
        Ok(Some(forwarded))
      } else {
        warn!("Incoming Forwarded header is inconsistent with X-Forwarded-* headers; falling back to X-Forwarded-For");
        Ok(Some(xff))
      }
    }
    (Some(forwarded), None) => Ok(Some(forwarded)),
    (None, Some(xff)) => Ok(Some(xff)),
    (None, None) => Ok(None),
  }
}

/// Reduce the forwarding chain by removing trusted proxy hops from the end.
/// We assume the immediate peer is always appended as the last hop.
fn reduce_trusted_proxy_chain(mut chain: Vec<ForwardedEntry>, trusted_forwarded_proxies: &[IpNet]) -> Vec<ForwardedEntry> {
  let mut idx = chain.len();
  while idx > 0 && entry_is_trusted_proxy(&chain[idx - 1], trusted_forwarded_proxies) {
    idx -= 1;
  }

  if idx == 0 {
    return chain;
  }
  let authoritative_idx = idx - 1;
  chain.drain(0..authoritative_idx);
  chain
}

/// Extract IP addresses from X-Forwarded-For header
fn parse_x_forwarded_for_header(headers: &HeaderMap) -> Result<Vec<ForwardedEntry>> {
  let xff = join_header_values(headers, X_FORWARDED_FOR)?.ok_or_else(|| anyhow!("x-forwarded-for header missing"))?;
  let xf_proto = first_header_value(headers, X_FORWARDED_PROTO)?.map(|s| s.trim().to_string());
  let ips: Vec<&str> = xff.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
  let last_idx = ips.len().saturating_sub(1);
  let mut chain = Vec::with_capacity(ips.len());
  for (idx, ip) in ips.into_iter().enumerate() {
    let for_ip = parse_forwarded_ip_token(ip)?;
    chain.push(ForwardedEntry {
      for_node: ForwardedNode::Ip(for_ip, None),
      proto: if idx == last_idx { xf_proto.clone() } else { None },
      host: None,
      by: None,
    });
  }
  Ok(chain)
}

/// Parse Forwarded header according to RFC 7239.
/// This is more complex than X-Forwarded-For due to its richer syntax and potential for multiple field-lines.
fn parse_forwarded_header(headers: &HeaderMap) -> Result<Vec<ForwardedEntry>> {
  let forwarded = join_header_values(headers, header::FORWARDED)?.ok_or_else(|| anyhow!("forwarded header missing"))?;
  let mut chain = Vec::new();
  for entry in split_respecting_quotes(&forwarded, b',').into_iter().filter(|entry| !entry.is_empty()) {
    let forwarded_entry = parse_forwarded_header_entry(entry)?;
    chain.push(forwarded_entry);
  }
  Ok(chain)
}

/// Parse a single entry in Forwarded header, which may contain multiple parameters separated by ';'.
fn parse_forwarded_header_entry(entry: &str) -> Result<ForwardedEntry> {
  let mut for_node = None;
  let mut proto = None;
  let mut host = None;
  let mut by = None;
  for param in split_respecting_quotes(entry, b';').into_iter().filter(|param| !param.is_empty()) {
    let Some((key, value)) = param.split_once('=') else {
      continue;
    };
    let key = key.trim().to_ascii_lowercase();
    let value = value.trim();
    match key.as_str() {
      "for" => {
        if for_node.is_some() {
          return Err(anyhow!("forwarded header entry contains duplicate for= parameter"));
        }
        for_node = Some(parse_forwarded_node(value)?);
      }
      "proto" => {
        if proto.is_some() {
          return Err(anyhow!("forwarded header entry contains duplicate proto= parameter"));
        }
        proto = Some(unquote_http_value(value)?);
      }
      "host" => {
        if host.is_some() {
          return Err(anyhow!("forwarded header entry contains duplicate host= parameter"));
        }
        host = Some(unquote_http_value(value)?);
      }
      "by" => {
        if by.is_some() {
          return Err(anyhow!("forwarded header entry contains duplicate by= parameter"));
        }
        by = Some(unquote_http_value(value)?);
      }
      _ => continue, // Ignore unrecognized parameters
    }
  }
  let Some(for_node) = for_node else {
    return Err(anyhow!("forwarded header entry missing for= parameter"));
  };
  Ok(ForwardedEntry { for_node, proto, host, by })
}

/// Check consistency between Forwarded and X-Forwarded-* headers. This is a sanity check to prevent trusting a forged Forwarded header when X-Forwarded-For is also present and inconsistent.
fn forwarded_is_consistent(forwarded: &[ForwardedEntry], xff_chain: &[ForwardedEntry]) -> bool {
  // Length check
  if forwarded.len() != xff_chain.len() {
    return false;
  }
  // IP chain check
  if !forwarded
    .iter()
    .map(|entry| entry.for_node.ip_addr())
    .eq(xff_chain.iter().map(|entry| entry.for_node.ip_addr()))
  {
    return false;
  }

  // Optional proto check for the last entry, if proto is present in both headers. This is a sanity check to prevent trusting a forged Forwarded header when X-Forwarded-Proto is also present and inconsistent.
  let proto = xff_chain.last().and_then(|entry| entry.proto.as_deref());
  if let (Some(forwarded_proto), Some(proto)) = (forwarded.last().and_then(|entry| entry.proto.as_deref()), proto) {
    if !forwarded_proto.eq_ignore_ascii_case(proto.trim()) {
      return false;
    }
  }

  true
}

fn first_header_value(headers: &HeaderMap, key: impl AsHeaderName) -> Result<Option<String>> {
  let Some(first) = headers.get(key) else {
    return Ok(None);
  };
  first
    .to_str()
    .map(|value| Some(value.to_string()))
    .map_err(|e| anyhow!("invalid header value: {e}"))
}

fn join_header_values(headers: &HeaderMap, key: impl AsHeaderName) -> Result<Option<String>> {
  let values = headers
    .get_all(key)
    .iter()
    .map(|value| value.to_str().map(|s| s.to_string()))
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(|e| anyhow!("invalid header value: {e}"))?;
  if values.is_empty() {
    Ok(None)
  } else {
    Ok(Some(values.join(", ")))
  }
}

fn split_respecting_quotes(input: &str, delimiter: u8) -> Vec<&str> {
  let mut segments = Vec::new();
  let mut start = 0usize;
  let mut in_quotes = false;
  let mut escaped = false;
  for (idx, byte) in input.as_bytes().iter().copied().enumerate() {
    if escaped {
      escaped = false;
      continue;
    }
    match byte {
      b'\\' if in_quotes => escaped = true,
      b'"' => in_quotes = !in_quotes,
      b if b == delimiter && !in_quotes => {
        segments.push(input[start..idx].trim());
        start = idx + 1;
      }
      _ => {}
    }
  }
  segments.push(input[start..].trim());
  segments
}

fn unquote_http_value(value: &str) -> Result<String> {
  let trimmed = value.trim();
  if !trimmed.starts_with('"') {
    return Ok(trimmed.to_string());
  }
  let Some(inner) = trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
    return Err(anyhow!("unterminated quoted-string `{trimmed}`"));
  };
  let mut result = String::with_capacity(inner.len());
  let mut escaped = false;
  for ch in inner.chars() {
    if escaped {
      result.push(ch);
      escaped = false;
      continue;
    }
    if ch == '\\' {
      escaped = true;
      continue;
    }
    result.push(ch);
  }
  if escaped {
    return Err(anyhow!("unterminated escape sequence in quoted-string `{trimmed}`"));
  }
  Ok(result)
}

fn parse_forwarded_ip_token(token: &str) -> Result<IpAddr> {
  match parse_forwarded_node(token)? {
    ForwardedNode::Ip(ip, _) => Ok(ip),
    ForwardedNode::Unknown(_) | ForwardedNode::Obfuscated(_, _) => {
      Err(anyhow!("forwarded node `{token}` is not an IP address"))
    }
  }
}

fn parse_forwarded_node(token: &str) -> Result<ForwardedNode> {
  let trimmed = unquote_http_value(token)?;
  if trimmed.eq_ignore_ascii_case("unknown") {
    return Ok(ForwardedNode::Unknown(None));
  }
  if let Some((node, port)) = trimmed.rsplit_once(':') {
    if let Some(inner) = node.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
      let ip = canonicalize_ip(IpAddr::from_str(inner).map_err(|e| anyhow!("invalid forwarded address `{token}`: {e}"))?);
      return Ok(ForwardedNode::Ip(ip, Some(parse_forwarded_port(port, token)?)));
    }
    if node.eq_ignore_ascii_case("unknown") {
      return Ok(ForwardedNode::Unknown(Some(port.to_string())));
    }
    if node.starts_with('_') {
      return Ok(ForwardedNode::Obfuscated(node.to_string(), Some(port.to_string())));
    }
    if let Ok(ip) = IpAddr::from_str(node) {
      return Ok(ForwardedNode::Ip(canonicalize_ip(ip), Some(parse_forwarded_port(port, token)?)));
    }
  }

  if let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
    let ip = IpAddr::from_str(inner)
      .map_err(|e| anyhow!("invalid forwarded address `{token}`: {e}"))?;
    Ok(ForwardedNode::Ip(canonicalize_ip(ip), None))
  } else if trimmed.starts_with('_') {
    Ok(ForwardedNode::Obfuscated(trimmed, None))
  } else {
    let ip = IpAddr::from_str(&trimmed)
      .map_err(|e| anyhow!("invalid forwarded address `{token}`: {e}"))?;
    Ok(ForwardedNode::Ip(canonicalize_ip(ip), None))
  }
}

fn parse_forwarded_port(port: &str, token: &str) -> Result<u16> {
  port
    .parse::<u16>()
    .map_err(|e| anyhow!("invalid forwarded port in `{token}`: {e}"))
}

/// Canonicalize IP address to ensure consistent matching against trusted proxy list.
fn canonicalize_ip(ip: IpAddr) -> IpAddr {
  SocketAddr::new(ip, 0).to_canonical().ip()
}

fn build_peer_forwarded_entry(headers: &HeaderMap, peer_ip: IpAddr, tls: bool, original_uri: &Uri) -> ForwardedEntry {
  ForwardedEntry {
    for_node: ForwardedNode::Ip(peer_ip, None),
    proto: if tls { Some("https".into()) } else { Some("http".into()) },
    host: host_from_uri_or_host_header(original_uri, headers.get(header::HOST).cloned()).ok(),
    by: None,
  }
}

/// Check if the given IP is in the list of trusted proxies.
fn is_trusted_proxy(ip: &IpAddr, trusted_forwarded_proxies: &[IpNet]) -> bool {
  let canonical = canonicalize_ip(*ip);
  trusted_forwarded_proxies.iter().any(|net| net.contains(&canonical))
}

fn entry_is_trusted_proxy(entry: &ForwardedEntry, trusted_forwarded_proxies: &[IpNet]) -> bool {
  entry
    .for_node
    .ip_addr()
    .map(|ip| is_trusted_proxy(&ip, trusted_forwarded_proxies))
    .unwrap_or(false)
}

fn forwarded_chain_to_xff(chain: &[ForwardedEntry]) -> Option<Vec<String>> {
  chain
    .iter()
    .map(|entry| entry.for_node.ip_addr().map(|ip| ip.to_string()))
    .collect::<Option<Vec<_>>>()
}

fn overwrite_header_with_csv(headers: &mut HeaderMap, key: &str, values: &[String]) -> Result<()> {
  let name = HeaderName::from_bytes(key.as_bytes())?;
  headers.remove(&name);
  headers.insert(name, HeaderValue::from_str(&values.join(", "))?);
  Ok(())
}

/// Generate RFC 7239 Forwarded header from X-Forwarded-For
/// This function assumes that the X-Forwarded-For header is present and well-formed.
/// Earlier hops are emitted as `for=` only, and the last hop carries this proxy's
/// authoritative `proto` and `host` parameters.
fn generate_forwarded_header(normalized_forwarding_chain: &[ForwardedEntry]) -> Result<String> {
  if normalized_forwarding_chain.is_empty() {
    return Err(anyhow!("No forwarding chain found for Forwarded generation"));
  }

  // for= is always present. proto=, host=, and by= might not be present.
  let elements = normalized_forwarding_chain
    .iter()
    .map(|entry| {
      let mut parts = vec![format!("for={}", format_forwarded_node(&entry.for_node)?)];
      if let Some(proto) = &entry.proto {
        parts.push(format!("proto={proto}"));
      }
      if let Some(host) = &entry.host {
        parts.push(format!("host={host}"));
      }
      if let Some(by) = &entry.by {
        parts.push(format!("by={by}"));
      }
      Ok(parts.join(";"))
    })
    .collect::<Result<Vec<_>>>()?;

  Ok(elements.join(", "))
}

/// Format the for= value in Forwarded header according to RFC 7239, which may require quoting and bracketing for IPv6 addresses.
fn format_forwarded_node(node: &ForwardedNode) -> Result<String> {
  match node {
    ForwardedNode::Ip(IpAddr::V4(v4), None) => Ok(v4.to_string()),
    ForwardedNode::Ip(IpAddr::V4(v4), Some(port)) => Ok(format!("\"{}:{}\"", v4, port)),
    ForwardedNode::Ip(IpAddr::V6(v6), None) => Ok(format!("\"[{}]\"", v6)),
    ForwardedNode::Ip(IpAddr::V6(v6), Some(port)) => Ok(format!("\"[{}]:{}\"", v6, port)),
    ForwardedNode::Unknown(None) => Ok("unknown".to_string()),
    ForwardedNode::Unknown(Some(port)) => Ok(format!("\"unknown:{}\"", port)),
    ForwardedNode::Obfuscated(node, None) => Ok(node.clone()),
    ForwardedNode::Obfuscated(node, Some(port)) => Ok(format!("\"{}:{}\"", node, port)),
  }
}

/// Extract host from URI
pub(super) fn host_from_uri_or_host_header(uri: &Uri, host_header_value: Option<header::HeaderValue>) -> Result<String> {
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

/// Remove connection header
pub(super) fn remove_connection_header(headers: &mut HeaderMap) {
  if let Some(values) = headers.get(header::CONNECTION) {
    if let Ok(v) = values.clone().to_str() {
      let keys = v.split(',').map(|m| m.trim()).filter(|m| !m.is_empty());
      for m in keys {
        headers.remove(m);
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

  fn trusted(cidrs: &[&str]) -> Vec<IpNet> {
    cidrs.iter().map(|c| c.parse::<IpNet>().unwrap()).collect()
  }

  #[test]
  fn untrusted_peer_ignores_incoming_forwarding_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));
    headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("1.2.3.4"));
    headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static("https"));
    headers.insert(X_FORWARDED_PORT, HeaderValue::from_static("443"));
    headers.insert(
      header::FORWARDED,
      HeaderValue::from_static("for=1.2.3.4;proto=https;host=app.example"),
    );

    add_forwarding_header(
      &mut headers,
      &"203.0.113.10:4321".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/demo?q=1"),
      &[],
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "203.0.113.10");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "203.0.113.10");
    assert_eq!(headers.get(X_FORWARDED_PROTO).unwrap(), "http");
    assert_eq!(headers.get(X_FORWARDED_PORT).unwrap(), "8080");
    assert_eq!(
      headers.get(header::FORWARDED).unwrap(),
      "for=203.0.113.10;proto=http;host=app.example"
    );
  }

  #[test]
  fn trusted_proxy_keeps_verified_trusted_suffix() {
    let mut headers = HeaderMap::new();
    headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("198.51.100.10, 10.9.0.4"));

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8443".parse().unwrap(),
      true,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "198.51.100.10, 10.9.0.4, 10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "198.51.100.10");
    assert_eq!(headers.get(X_FORWARDED_PROTO).unwrap(), "https");
  }

  #[test]
  fn trusted_proxy_stops_at_last_non_trusted_hop() {
    let mut headers = HeaderMap::new();
    headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("198.51.100.10, 203.0.113.20"));

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "203.0.113.20, 10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "203.0.113.20");
  }

  #[test]
  fn trusted_proxy_uses_forwarded_when_xff_missing() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));
    headers.insert(header::FORWARDED, HeaderValue::from_static("for=198.51.100.10, for=10.9.0.4"));

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "198.51.100.10, 10.9.0.4, 10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "198.51.100.10");
    assert_eq!(
      headers.get(header::FORWARDED).unwrap(),
      "for=198.51.100.10, for=10.9.0.4, for=10.1.2.3;proto=http;host=app.example"
    );
  }

  #[test]
  fn trusted_proxy_falls_back_to_xff_when_forwarded_chain_is_inconsistent() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));
    headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("198.51.100.10"));
    headers.insert(
      header::FORWARDED,
      HeaderValue::from_static("for=203.0.113.20;proto=https;host=app.example"),
    );

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "198.51.100.10, 10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "198.51.100.10");
    assert_eq!(
      headers.get(header::FORWARDED).unwrap(),
      "for=198.51.100.10, for=10.1.2.3;proto=http;host=app.example"
    );
  }

  #[test]
  fn trusted_proxy_accepts_multiple_xff_field_lines() {
    let mut headers = HeaderMap::new();
    headers.append(X_FORWARDED_FOR, HeaderValue::from_static("198.51.100.10"));
    headers.append(X_FORWARDED_FOR, HeaderValue::from_static("10.9.0.4"));

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "198.51.100.10, 10.9.0.4, 10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "198.51.100.10");
  }

  #[test]
  fn trusted_proxy_accepts_multiple_forwarded_field_lines() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));
    headers.append(header::FORWARDED, HeaderValue::from_static("for=198.51.100.10"));
    headers.append(header::FORWARDED, HeaderValue::from_static("for=10.9.0.4"));

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "198.51.100.10, 10.9.0.4, 10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "198.51.100.10");
    assert_eq!(
      headers.get(header::FORWARDED).unwrap(),
      "for=198.51.100.10, for=10.9.0.4, for=10.1.2.3;proto=http;host=app.example"
    );
  }

  #[test]
  fn ipv6_peer_produces_correctly_formatted_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));

    add_forwarding_header(
      &mut headers,
      &"[2001:db8::1]:4321".parse().unwrap(),
      &"[::1]:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &[],
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "2001:db8::1");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "2001:db8::1");
    assert!(!headers.contains_key(header::FORWARDED));
  }

  #[test]
  fn ipv6_peer_with_forwarded_option_generates_quoted_bracket_for() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));

    add_forwarding_header(
      &mut headers,
      &"[2001:db8::1]:4321".parse().unwrap(),
      &"[::1]:8080".parse().unwrap(),
      true,
      &Uri::from_static("/"),
      &[],
    )
    .unwrap();

    let normalized_chain = vec![ForwardedEntry {
      for_node: ForwardedNode::Ip("2001:db8::1".parse().unwrap(), None),
      proto: Some("https".into()),
      host: Some("app.example".into()),
      by: None,
    }];
    let forwarded = generate_forwarded_header(&normalized_chain).unwrap();
    assert_eq!(forwarded, "for=\"[2001:db8::1]\";proto=https;host=app.example");
  }

  #[test]
  fn ipv4_mapped_ipv6_peer_is_canonicalized() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));

    // ::ffff:10.1.2.3 should be canonicalized to 10.1.2.3
    add_forwarding_header(
      &mut headers,
      &"[::ffff:10.1.2.3]:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &[],
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "10.1.2.3");
  }

  #[test]
  fn ipv4_mapped_ipv6_matches_trusted_v4_cidr() {
    let mut headers = HeaderMap::new();
    headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("198.51.100.10"));

    // Peer is ::ffff:10.1.2.3, trusted CIDR is 10.0.0.0/8 (IPv4)
    add_forwarding_header(
      &mut headers,
      &"[::ffff:10.1.2.3]:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "198.51.100.10, 10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "198.51.100.10");
  }

  #[test]
  fn trusted_proxy_parses_forwarded_with_ipv6() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));
    headers.insert(
      header::FORWARDED,
      HeaderValue::from_static("for=\"[2001:db8::1]\";proto=https;host=app.example"),
    );

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "2001:db8::1, 10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "2001:db8::1");
  }

  #[test]
  fn xff_with_empty_segments_assigns_proto_to_last_valid_entry() {
    let mut headers = HeaderMap::new();
    headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("198.51.100.10, , 203.0.113.20"));
    headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static("https"));

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    // proto should be assigned to 203.0.113.20 (the last valid entry), not lost
    assert_eq!(
      headers.get(X_FORWARDED_FOR).unwrap(),
      "203.0.113.20, 10.1.2.3"
    );
  }

  #[test]
  fn trusted_proxy_accepts_forwarded_with_port() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));
    headers.insert(
      header::FORWARDED,
      HeaderValue::from_static("for=\"192.0.2.43:4711\", for=10.9.0.4"),
    );

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "192.0.2.43, 10.9.0.4, 10.1.2.3");
    assert_eq!(
      headers.get(header::FORWARDED).unwrap(),
      "for=\"192.0.2.43:4711\", for=10.9.0.4, for=10.1.2.3;proto=http;host=app.example"
    );
  }

  #[test]
  fn trusted_proxy_with_forwarded_unknown_falls_back_to_peer_only() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));
    headers.insert(
      header::FORWARDED,
      HeaderValue::from_static("for=unknown, for=10.9.0.4"),
    );

    add_forwarding_header(
      &mut headers,
      &"10.1.2.3:1234".parse().unwrap(),
      &"192.0.2.1:8080".parse().unwrap(),
      false,
      &Uri::from_static("/"),
      &trusted(&["10.0.0.0/8"]),
    )
    .unwrap();

    assert_eq!(headers.get(X_FORWARDED_FOR).unwrap(), "10.1.2.3");
    assert_eq!(headers.get(X_REAL_IP).unwrap(), "10.1.2.3");
    assert_eq!(
      headers.get(header::FORWARDED).unwrap(),
      "for=10.1.2.3;proto=http;host=app.example"
    );
  }
}
