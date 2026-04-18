use anyhow::{Result, anyhow};
use http::{HeaderMap, HeaderValue, Uri, header};
use ipnet::IpNet;

use crate::{
  backend::{UpstreamCandidates, UpstreamOption},
  log::*,
  name_exp::ServerName,
};

use super::{
  common::{add_header_entry_overwrite_if_exist, add_header_entry_overwrite_if_exist_name, host_from_uri_or_host_header},
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

/// Apply the `default_app` fallback rewrite to request headers.
///
/// Called when the request was matched via the `default_app` fallback path: the incoming `Host`
/// did not match any configured `server_name`, so it is untrusted by definition.
///
/// - `Host` is force-overwritten with `authoritative_host` (the default app's configured `server_name`).
///   This wins against `keep_original_host` / `set_upstream_host` upstream options.
/// - `X-Forwarded-Host` is overwritten with the original client-visible host (URI authority for
///   absolute-form requests, otherwise the original `Host` header). Any client-supplied
///   `X-Forwarded-Host` is dropped — same trust-boundary rule as other `X-Forwarded-*` headers.
/// - If the original host cannot be derived, `X-Forwarded-Host` is cleared rather than left as-is.
pub(in crate::message_handler) fn apply_default_app_fallback_rewrite(
  headers: &mut HeaderMap,
  original_uri: &Uri,
  original_host_header: Option<&HeaderValue>,
  authoritative_host: &ServerName,
) -> Result<()> {
  match host_from_uri_or_host_header(original_uri, original_host_header) {
    Ok(original_host) => {
      add_header_entry_overwrite_if_exist(headers, "x-forwarded-host", original_host)?;
    }
    Err(_) => {
      headers.remove("x-forwarded-host");
    }
  }
  headers.insert(header::HOST, HeaderValue::from_bytes(authoritative_host.as_ref())?);
  Ok(())
}

/// Apply options to request header, which are specified in the configuration
/// This function is called after almost all other headers has been set and updated.
pub(in crate::message_handler) fn apply_upstream_options_to_header(
  headers: &mut HeaderMap,
  original_uri: &Uri,
  original_host_header: Option<&HeaderValue>,
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
          let authoritative_host = host_from_uri_or_host_header(original_uri, original_host_header).ok();
          let Some(forwarding_chain) = extract_forwarding_chain_from_headers(headers, authoritative_host)? else {
            warn!("Failed to generate Forwarded header: no X-Forwarded-For information found in headers");
            continue;
          };
          let normalized_chain = reduce_trusted_proxy_chain(forwarding_chain, trusted_forwarded_proxies);
          match generate_forwarded_header(&normalized_chain) {
            Ok(forwarded_value) => {
              add_header_entry_overwrite_if_exist_name(headers, header::FORWARDED, forwarded_value)?;
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

    let original_host = HeaderValue::from_static("app.example:8443");
    apply_upstream_options_to_header(
      &mut headers,
      &"/hello".parse::<Uri>().unwrap(),
      Some(&original_host),
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

  #[test]
  fn fallback_rewrite_forces_host_and_overwrites_xfh() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("attacker.example"));
    // A client-supplied X-Forwarded-Host must be dropped, not preserved.
    headers.insert("x-forwarded-host", HeaderValue::from_static("spoofed.example"));

    let original_host = HeaderValue::from_static("attacker.example");
    apply_default_app_fallback_rewrite(
      &mut headers,
      &"/path".parse::<Uri>().unwrap(),
      Some(&original_host),
      &ServerName::from("default.app.example"),
    )
    .unwrap();

    assert_eq!(headers.get(header::HOST).unwrap(), "default.app.example");
    assert_eq!(headers.get("x-forwarded-host").unwrap(), "attacker.example");
  }

  #[test]
  fn fallback_rewrite_uses_uri_authority_for_absolute_form() {
    let mut headers = HeaderMap::new();
    // Host header and URI authority disagree (absolute-form request).
    headers.insert(header::HOST, HeaderValue::from_static("host-header.example"));

    let original_host = HeaderValue::from_static("host-header.example");
    apply_default_app_fallback_rewrite(
      &mut headers,
      &"http://uri-authority.example:8080/path".parse::<Uri>().unwrap(),
      Some(&original_host),
      &ServerName::from("default.app.example"),
    )
    .unwrap();

    assert_eq!(headers.get(header::HOST).unwrap(), "default.app.example");
    // URI authority wins over Host header per host_from_uri_or_host_header semantics.
    assert_eq!(headers.get("x-forwarded-host").unwrap(), "uri-authority.example:8080");
  }

  #[test]
  fn fallback_rewrite_clears_xfh_when_no_original_host_is_available() {
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-host", HeaderValue::from_static("spoofed.example"));

    // Neither URI authority nor Host header present.
    apply_default_app_fallback_rewrite(
      &mut headers,
      &"/relative".parse::<Uri>().unwrap(),
      None,
      &ServerName::from("default.app.example"),
    )
    .unwrap();

    assert_eq!(headers.get(header::HOST).unwrap(), "default.app.example");
    assert!(headers.get("x-forwarded-host").is_none());
  }
}
