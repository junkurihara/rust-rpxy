use anyhow::{Result, anyhow};
use http::{HeaderMap, HeaderValue, Uri, header};
use ipnet::IpNet;

use crate::{
  backend::{UpstreamCandidates, UpstreamOption},
  log::*,
};

use super::{
  common::{add_header_entry_overwrite_if_exist, host_from_uri_or_host_header},
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
  original_uri: &Uri,
  original_host_header: Option<HeaderValue>,
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
          let authoritative_host = host_from_uri_or_host_header(original_uri, original_host_header.clone()).ok();
          let Some(forwarding_chain) = extract_forwarding_chain_from_headers(headers, authoritative_host)? else {
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::backend::{LoadBalance, Upstream};
  use ahash::HashSet;

  #[test]
  fn forwarded_header_generation_keeps_authoritative_host_on_last_hop() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example:8443"));
    headers.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.10"));
    headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));

    let upstream = Upstream {
      uri: "http://backend.internal".parse().unwrap(),
      #[cfg(feature = "health-check")]
      health: None,
    };
    let upstream_candidates = UpstreamCandidates {
      inner: vec![upstream],
      path: "/".into(),
      replace_path: None,
      load_balance: LoadBalance::default(),
      options: HashSet::from_iter([UpstreamOption::ForwardedHeader]),
      #[cfg(feature = "health-check")]
      health_check_config: None,
    };

    apply_upstream_options_to_header(
      &mut headers,
      &"/hello".parse::<Uri>().unwrap(),
      Some(HeaderValue::from_static("app.example:8443")),
      &"http://backend.internal".parse::<Uri>().unwrap(),
      &upstream_candidates,
      &[],
    )
    .unwrap();

    assert_eq!(
      headers.get(header::FORWARDED).unwrap(),
      "for=198.51.100.10;proto=https;host=\"app.example:8443\""
    );
  }
}
