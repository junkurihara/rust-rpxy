use super::{HttpMessageHandler, handler_main::HandlerContext, header_ops::*, request_ops::update_request_line};
use crate::{
  backend::{BackendApp, UpstreamCandidates},
  constants::RESPONSE_HEADER_SERVER,
  log::*,
  name_exp::{PathName, ServerName},
};
use anyhow::{Result, anyhow, ensure};
use http::{HeaderValue, Request, Response, Uri, header, uri::PathAndQuery};
use hyper_util::client::legacy::connect::Connect;
use std::net::SocketAddr;

impl<C> HttpMessageHandler<C>
where
  C: Send + Sync + Connect + Clone + 'static,
{
  ////////////////////////////////////////////////////
  // Functions to generate messages
  ////////////////////////////////////////////////////

  #[allow(unused_variables)]
  /// Manipulate a response message sent from a backend application to forward downstream to a client.
  pub(super) fn generate_response_forwarded<B>(
    &self,
    response: &mut Response<B>,
    backend_app: &BackendApp,
    is_secure_transport: bool,
  ) -> Result<()> {
    let headers = response.headers_mut();
    remove_connection_header(headers);
    remove_hop_header(headers);
    add_header_entry_overwrite_if_exist(headers, header::SERVER, RESPONSE_HEADER_SERVER)?;

    #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
    {
      // Manipulate ALT_SVC allowing h3 in response message only when mutual TLS is not enabled
      // TODO: This is a workaround for avoiding a client authentication in HTTP/3
      if let Some(port) = h3_alt_svc_port(&self.globals.proxy_config, backend_app.mutual_tls, is_secure_transport) {
        add_header_entry_overwrite_if_exist(
          headers,
          header::ALT_SVC,
          format!("h3=\":{}\"; ma={}", port, self.globals.proxy_config.h3_alt_svc_max_age),
        )?;
      } else {
        // remove alt-svc to disallow requests via http3
        headers.remove(header::ALT_SVC);
      }
    }
    #[cfg(not(any(feature = "http3-quinn", feature = "http3-s2n")))]
    {
      if self.globals.proxy_config.https_port.is_some() {
        headers.remove(header::ALT_SVC);
      }
    }

    Ok(())
  }

  #[allow(clippy::too_many_arguments)]
  /// Manipulate a request message sent from a client to forward upstream to a backend application.
  ///
  /// `fallback_host`: set to `Some(server_name)` when the request was matched via the `default_app`
  /// fallback path. In that case the incoming `Host` is untrusted and will be force-overwritten
  /// with the given authoritative value. `X-Forwarded-Host` is rebuilt separately by
  /// `add_forwarding_header()` as part of the general forwarding-header policy.
  pub(super) fn generate_request_forwarded<B>(
    &self,
    client_addr: &SocketAddr,
    listen_addr: &SocketAddr,
    req: &mut Request<B>,
    upgrade: &Option<String>,
    upstream_candidates: &UpstreamCandidates,
    tls_enabled: bool,
    fallback_host: Option<&ServerName>,
  ) -> Result<HandlerContext> {
    trace!("Generate request to be forwarded");

    // Add te: trailer if contained in original request
    let contains_te_trailers = {
      req
        .headers()
        .get(header::TE)
        .map(|te| {
          te.as_bytes()
            .split(|v| v == &b',' || v == &b' ')
            .any(|x| x == "trailers".as_bytes())
        })
        .unwrap_or(false)
    };

    let original_uri = req.uri().clone();
    let original_host_header = req.headers().get(header::HOST).cloned();
    let headers = req.headers_mut();
    // delete headers specified in header.connection
    remove_connection_header(headers);
    // delete hop headers including header.connection
    remove_hop_header(headers);
    // Capture the client-visible scheme from the inbound forwarding headers BEFORE
    // add_forwarding_header() overwrites X-Forwarded-Proto with rpxy's listener TLS state.
    // Used by the sticky-cookie `Secure` attribute on the response side.
    #[cfg(feature = "sticky-cookie")]
    let sticky_cookie_secure = client_visible_secure(
      tls_enabled,
      client_addr,
      headers,
      &self.globals.proxy_config.trusted_forwarded_proxies,
    );
    // X-Forwarded-For (and Forwarded if exists)
    add_forwarding_header(
      headers,
      client_addr,
      listen_addr,
      tls_enabled,
      &original_uri,
      &self.globals.proxy_config.trusted_forwarded_proxies,
    )?;

    // Add te: trailer if te_trailer
    if contains_te_trailers {
      headers.insert(header::TE, HeaderValue::from_bytes("trailers".as_bytes()).unwrap());
    }

    // by default, add "host" header of original server_name if not exist
    if original_host_header.is_none() {
      let org_host = req.uri().host().ok_or_else(|| anyhow!("Invalid request"))?.to_owned();
      req.headers_mut().insert(header::HOST, HeaderValue::from_str(&org_host)?);
    };

    /////////////////////////////////////////////
    // Fix unique upstream destination since there could be multiple ones.
    #[cfg(feature = "sticky-cookie")]
    let (upstream_chosen_opt, context_from_lb, sticky_cookie_config) = {
      let mut sticky_cookie_config = None;
      let context_to_lb = if let crate::backend::LoadBalance::StickyRoundRobin(lb) = &upstream_candidates.load_balance {
        let cipher = self
          .globals
          .sticky_cookie_cipher
          .as_deref()
          .ok_or_else(|| anyhow!("sticky-cookie cipher is not configured"))?;
        sticky_cookie_config = Some(lb.sticky_config.clone());
        takeout_sticky_cookie_lb_context(req.headers_mut(), &lb.sticky_config, cipher)?
      } else {
        None
      };
      let (upstream_chosen_opt, context_from_lb) = upstream_candidates.get(&context_to_lb);
      (upstream_chosen_opt, context_from_lb, sticky_cookie_config)
    };
    #[cfg(not(feature = "sticky-cookie"))]
    let (upstream_chosen_opt, _) = upstream_candidates.get(&None);

    let upstream_chosen = upstream_chosen_opt.ok_or_else(|| anyhow!("Failed to get upstream"))?;
    let context = HandlerContext {
      #[cfg(feature = "sticky-cookie")]
      context_lb: context_from_lb,
      #[cfg(not(feature = "sticky-cookie"))]
      context_lb: None,
      #[cfg(feature = "sticky-cookie")]
      sticky_cookie_secure,
      #[cfg(feature = "sticky-cookie")]
      sticky_cookie_config,
    };
    /////////////////////////////////////////////

    // apply upstream-specific headers given in upstream_option
    let headers = req.headers_mut();
    // apply upstream options to header, after X-Forwarded-For is added
    apply_upstream_options_to_header(
      headers,
      &original_uri,
      original_host_header.as_ref(),
      &upstream_chosen.uri,
      upstream_candidates,
      &self.globals.proxy_config.trusted_forwarded_proxies,
    )?;

    // Default-app fallback hardening: when the request was matched via the `default_app`
    // path, the incoming `Host` is untrusted. Force-overwrite it with the default app's
    // authoritative server_name. Observational forwarding headers such as
    // `X-Forwarded-Host` are rebuilt earlier by `add_forwarding_header()`.
    if let Some(authoritative_host) = fallback_host {
      apply_default_app_host_rewrite(headers, authoritative_host)?;
    }

    // update uri in request
    ensure!(
      upstream_chosen.uri.authority().is_some() && upstream_chosen.uri.scheme().is_some(),
      "Upstream uri `scheme` and `authority` is broken"
    );

    let new_uri = Uri::builder()
      .scheme(upstream_chosen.uri.scheme().unwrap().as_str())
      .authority(upstream_chosen.uri.authority().unwrap().as_str());

    // Build the upstream path+query (applying replace_path if configured for this group).
    let new_pq = rebuild_path_and_query(
      req.uri(),
      upstream_candidates.replace_path.as_ref(),
      &upstream_candidates.path,
    )?;
    *req.uri_mut() = new_uri.path_and_query(new_pq).build()?;

    // upgrade
    if let Some(v) = upgrade {
      req.headers_mut().insert(header::UPGRADE, v.parse()?);
      req
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    }
    if upgrade.is_none() {
      // can update request line i.e., http version, only if not upgrade (http 1.1)
      update_request_line(req, upstream_chosen, upstream_candidates)?;
    }

    Ok(context)
  }
}

#[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
fn h3_alt_svc_port(
  proxy_config: &crate::globals::ProxyConfig,
  backend_mutual_tls: Option<bool>,
  is_secure_transport: bool,
) -> Option<u16> {
  if proxy_config.http3 && is_secure_transport && backend_mutual_tls == Some(false) {
    proxy_config.public_https_port
  } else {
    None
  }
}

/// Build the path-and-query for the outgoing upstream request.
///
/// Without `replace_path`, the original request path+query is reused as-is via a shallow
/// `PathAndQuery` clone (no byte copy or re-validation), falling back to `/` when the request
/// carries none. With `replace_path`, the matched route prefix is swapped for the replacement
/// path while preserving the remainder (and any query string).
fn rebuild_path_and_query(req_uri: &Uri, replace_path: Option<&PathName>, matched_path: &PathName) -> Result<PathAndQuery> {
  let Some(new_path) = replace_path else {
    return Ok(
      req_uri
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| PathAndQuery::from_static("/")),
    );
  };

  let org_pq = req_uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/").as_bytes();
  let matched: &[u8] = matched_path.as_ref();
  ensure!(
    !matched.is_empty() && org_pq.len() >= matched.len(),
    "Upstream uri `path and query` is broken"
  );
  let mut v = Vec::<u8>::with_capacity(org_pq.len() - matched.len() + new_path.len());
  v.extend_from_slice(new_path.as_ref());
  v.extend_from_slice(&org_pq[matched.len()..]);
  // Wrap InvalidUri in http::Error so the error type matches the previous
  // `.path_and_query(Vec).build()?` path exactly.
  let pq = PathAndQuery::try_from(v).map_err(http::Error::from)?;
  Ok(pq)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::name_exp::ByteName;

  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  fn proxy_config_for_h3_alt_svc(http3: bool, public_https_port: Option<u16>) -> crate::globals::ProxyConfig {
    crate::globals::ProxyConfig {
      http3,
      public_https_port,
      ..Default::default()
    }
  }

  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  #[test]
  fn h3_alt_svc_port_advertises_on_secure_non_mtls_transport() {
    let proxy_config = proxy_config_for_h3_alt_svc(true, Some(443));
    assert_eq!(h3_alt_svc_port(&proxy_config, Some(false), true), Some(443));
  }

  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  #[test]
  fn h3_alt_svc_port_does_not_advertise_on_plain_http() {
    let proxy_config = proxy_config_for_h3_alt_svc(true, Some(443));
    assert_eq!(h3_alt_svc_port(&proxy_config, Some(false), false), None);
  }

  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  #[test]
  fn h3_alt_svc_port_does_not_advertise_for_mtls_or_plaintext_app() {
    let proxy_config = proxy_config_for_h3_alt_svc(true, Some(443));
    assert_eq!(h3_alt_svc_port(&proxy_config, Some(true), true), None);
    assert_eq!(h3_alt_svc_port(&proxy_config, None, true), None);
  }

  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  #[test]
  fn h3_alt_svc_port_requires_h3_enabled_and_public_port() {
    let h3_disabled = proxy_config_for_h3_alt_svc(false, Some(443));
    assert_eq!(h3_alt_svc_port(&h3_disabled, Some(false), true), None);

    let no_public_port = proxy_config_for_h3_alt_svc(true, None);
    assert_eq!(h3_alt_svc_port(&no_public_port, Some(false), true), None);
  }

  #[test]
  fn rebuild_path_and_query_none_preserves_path_and_query() {
    let uri = Uri::from_static("http://example.com/a/b?x=1&y=2");
    let matched = "/".to_path_name();
    let pq = rebuild_path_and_query(&uri, None, &matched).unwrap();
    assert_eq!(pq.as_str(), "/a/b?x=1&y=2");
  }

  #[test]
  fn rebuild_path_and_query_none_defaults_to_root_when_absent() {
    let uri = Uri::from_static("http://example.com");
    let matched = "/".to_path_name();
    let pq = rebuild_path_and_query(&uri, None, &matched).unwrap();
    assert_eq!(pq.as_str(), "/");
  }

  #[test]
  fn rebuild_path_and_query_replaces_matched_prefix_keeping_query() {
    let uri = Uri::from_static("http://example.com/foo/bar?q=1");
    let matched = "/foo".to_path_name();
    let replace = "/new".to_path_name();
    let pq = rebuild_path_and_query(&uri, Some(&replace), &matched).unwrap();
    assert_eq!(pq.as_str(), "/new/bar?q=1");
  }
}
