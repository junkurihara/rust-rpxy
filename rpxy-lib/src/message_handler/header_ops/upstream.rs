use anyhow::{Result, anyhow};
use http::{HeaderMap, HeaderValue, Uri, header};
use ipnet::IpNet;

use crate::{
  backend::{UpstreamCandidates, UpstreamOption},
  log::*,
};

use super::{
  common::add_header_entry_overwrite_if_exist,
  forwarding::{extract_forwarding_chain_from_headers, generate_forwarded_header, reduce_trusted_proxy_chain},
};

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
pub(in crate::message_handler) fn apply_upstream_options_to_header(
  headers: &mut HeaderMap,
  upstream_base_uri: &Uri,
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
