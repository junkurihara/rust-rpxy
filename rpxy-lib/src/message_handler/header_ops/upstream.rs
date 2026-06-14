use anyhow::{Result, anyhow};
use http::{HeaderMap, HeaderValue, header};
use ipnet::IpNet;

use crate::{
  backend::{Upstream, UpstreamCandidates, UpstreamOption},
  log::*,
  name_exp::ServerName,
};

use super::{
  common::add_header_entry_overwrite_if_exist,
  forwarding::{extract_forwarding_chain_from_headers, generate_forwarded_header, reduce_trusted_proxy_chain},
};

/// overwrite HOST value with upstream hostname (like 192.168.xx.x seen from rpxy)
///
/// The value (`host` or `host:port`) is pre-rendered once per upstream at config-build time
/// (`Upstream::host_header`); here we only clone-insert it - a `Bytes` refcount bump, with no
/// per-request formatting or validation. `None` (an upstream uri without a host) preserves the
/// original "No hostname is given" error.
fn override_host_header(headers: &mut HeaderMap, host_header: Option<&HeaderValue>) -> Result<()> {
  let value = host_header.ok_or_else(|| anyhow!("No hostname is given"))?;
  // overwrite host header, this removes all the HOST header values
  headers.insert(header::HOST, value.clone());
  Ok(())
}

/// Apply the `default_app` host rewrite to request headers.
///
/// Called when the request was matched via the `default_app` fallback path: the incoming `Host`
/// did not match any configured `server_name`, so it is untrusted by definition.
///
/// - `Host` is force-overwritten with `authoritative_host` (the default app's configured `server_name`).
///   This wins against `keep_original_host` / `set_upstream_host` upstream options.
pub(in crate::message_handler) fn apply_default_app_host_rewrite(
  headers: &mut HeaderMap,
  authoritative_host: &ServerName,
) -> Result<()> {
  headers.insert(header::HOST, HeaderValue::from_bytes(authoritative_host.as_ref())?);
  Ok(())
}

/// Apply options to request header, which are specified in the configuration
/// This function is called after almost all other headers has been set and updated.
/// `authoritative_host` is the host of the original request (URI host preferred, port
/// included, Host-header fallback), computed once by the caller before any Host rewrite.
pub(in crate::message_handler) fn apply_upstream_options_to_header(
  headers: &mut HeaderMap,
  authoritative_host: Option<&str>,
  upstream_chosen: &Upstream,
  upstream_candidates: &UpstreamCandidates,
  trusted_forwarded_proxies: &[IpNet],
) -> Result<()> {
  for opt in upstream_candidates.options.iter() {
    match opt {
      UpstreamOption::SetUpstreamHost => {
        // prioritize KeepOriginalHost
        if !upstream_candidates.options.contains(&UpstreamOption::KeepOriginalHost) {
          // overwrite host header with the chosen upstream's pre-rendered Host value
          override_host_header(headers, upstream_chosen.host_header())?;
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
          let Some(forwarding_chain) = extract_forwarding_chain_from_headers(headers, authoritative_host)? else {
            warn!("Failed to generate Forwarded header: no X-Forwarded-For information found in headers");
            continue;
          };
          let normalized_chain = reduce_trusted_proxy_chain(forwarding_chain, trusted_forwarded_proxies);
          match generate_forwarded_header(&normalized_chain) {
            Ok(forwarded_value) => {
              add_header_entry_overwrite_if_exist(headers, header::FORWARDED, forwarded_value)?;
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
  use crate::{
    backend::{LoadBalance, Upstream},
    globals::UpstreamUri,
  };
  use ahash::HashSet;

  #[test]
  fn forwarded_header_generation_keeps_authoritative_host_on_last_hop() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example:8443"));
    headers.insert(
      http::HeaderName::from_static("x-forwarded-for"),
      HeaderValue::from_static("198.51.100.10"),
    );
    headers.insert(
      http::HeaderName::from_static("x-forwarded-proto"),
      HeaderValue::from_static("https"),
    );

    let upstream = Upstream::from(&UpstreamUri {
      inner: "http://backend.internal".parse().unwrap(),
    });
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
      Some("app.example:8443"),
      &upstream_candidates.inner[0],
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
  fn default_app_host_rewrite_forces_host() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("attacker.example"));

    apply_default_app_host_rewrite(&mut headers, &ServerName::from("default.app.example")).unwrap();

    assert_eq!(headers.get(header::HOST).unwrap(), "default.app.example");
  }

  #[test]
  fn default_app_host_rewrite_does_not_depend_on_uri_authority() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("host-header.example"));

    apply_default_app_host_rewrite(&mut headers, &ServerName::from("default.app.example")).unwrap();

    assert_eq!(headers.get(header::HOST).unwrap(), "default.app.example");
  }

  #[test]
  fn default_app_host_rewrite_works_without_original_host_information() {
    let mut headers = HeaderMap::new();
    apply_default_app_host_rewrite(&mut headers, &ServerName::from("default.app.example")).unwrap();

    assert_eq!(headers.get(header::HOST).unwrap(), "default.app.example");
  }

  #[test]
  fn set_upstream_host_rewrites_host() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));

    let upstream = Upstream::from(&UpstreamUri {
      inner: "http://backend.internal:8080".parse().unwrap(),
    });
    let upstream_candidates = UpstreamCandidates {
      inner: vec![upstream],
      path: "/".into(),
      replace_path: None,
      load_balance: LoadBalance::default(),
      options: HashSet::from_iter([UpstreamOption::SetUpstreamHost]),
      #[cfg(feature = "health-check")]
      health_check_config: None,
    };

    apply_upstream_options_to_header(
      &mut headers,
      Some("app.example"),
      &upstream_candidates.inner[0],
      &upstream_candidates,
      &[],
    )
    .unwrap();

    assert_eq!(headers.get(header::HOST).unwrap(), "backend.internal:8080");
  }

  #[test]
  fn keep_original_host_prevents_set_upstream_host_rewrite() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));

    let upstream = Upstream::from(&UpstreamUri {
      inner: "http://backend.internal:8080".parse().unwrap(),
    });
    let upstream_candidates = UpstreamCandidates {
      inner: vec![upstream],
      path: "/".into(),
      replace_path: None,
      load_balance: LoadBalance::default(),
      options: HashSet::from_iter([UpstreamOption::SetUpstreamHost, UpstreamOption::KeepOriginalHost]),
      #[cfg(feature = "health-check")]
      health_check_config: None,
    };

    apply_upstream_options_to_header(
      &mut headers,
      Some("app.example"),
      &upstream_candidates.inner[0],
      &upstream_candidates,
      &[],
    )
    .unwrap();

    assert_eq!(headers.get(header::HOST).unwrap(), "app.example");
  }

  #[test]
  fn set_upstream_host_without_host_returns_error_and_leaves_host_unchanged() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("app.example"));

    // An upstream uri without a host yields host_header == None; the override must surface the
    // original "No hostname is given" error and leave the HOST header untouched.
    let upstream = Upstream::from(&UpstreamUri {
      inner: "/no-host".parse().unwrap(),
    });
    assert!(upstream.host_header().is_none());
    let upstream_candidates = UpstreamCandidates {
      inner: vec![upstream],
      path: "/".into(),
      replace_path: None,
      load_balance: LoadBalance::default(),
      options: HashSet::from_iter([UpstreamOption::SetUpstreamHost]),
      #[cfg(feature = "health-check")]
      health_check_config: None,
    };

    let err = apply_upstream_options_to_header(
      &mut headers,
      Some("app.example"),
      &upstream_candidates.inner[0],
      &upstream_candidates,
      &[],
    )
    .unwrap_err();

    assert!(err.to_string().contains("No hostname is given"));
    assert_eq!(headers.get(header::HOST).unwrap(), "app.example");
  }
}
