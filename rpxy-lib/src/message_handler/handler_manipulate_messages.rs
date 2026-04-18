use super::{HttpMessageHandler, handler_main::HandlerContext, header_ops::*, request_ops::update_request_line};
use crate::{
  backend::{BackendApp, UpstreamCandidates},
  constants::RESPONSE_HEADER_SERVER,
  log::*,
  name_exp::ServerName,
};
use anyhow::{Result, anyhow, ensure};
use http::{HeaderValue, Request, Response, Uri, header};
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
  pub(super) fn generate_response_forwarded<B>(&self, response: &mut Response<B>, backend_app: &BackendApp) -> Result<()> {
    let headers = response.headers_mut();
    remove_connection_header(headers);
    remove_hop_header(headers);
    add_header_entry_overwrite_if_exist(headers, header::SERVER, RESPONSE_HEADER_SERVER)?;

    #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
    {
      // Manipulate ALT_SVC allowing h3 in response message only when mutual TLS is not enabled
      // TODO: This is a workaround for avoiding a client authentication in HTTP/3
      if self.globals.proxy_config.http3
        && backend_app.https_redirection.is_some()
        && backend_app.mutual_tls.as_ref().is_some_and(|v| !v)
      {
        if let Some(port) = self.globals.proxy_config.https_redirection_port {
          add_header_entry_overwrite_if_exist(
            headers,
            header::ALT_SVC,
            format!("h3=\":{}\"; ma={}", port, self.globals.proxy_config.h3_alt_svc_max_age),
          )?;
        }
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
    let (upstream_chosen_opt, context_from_lb) = {
      let context_to_lb = if let crate::backend::LoadBalance::StickyRoundRobin(lb) = &upstream_candidates.load_balance {
        takeout_sticky_cookie_lb_context(req.headers_mut(), &lb.sticky_config.name)?
      } else {
        None
      };
      upstream_candidates.get(&context_to_lb)
    };
    #[cfg(not(feature = "sticky-cookie"))]
    let (upstream_chosen_opt, _) = upstream_candidates.get(&None);

    let upstream_chosen = upstream_chosen_opt.ok_or_else(|| anyhow!("Failed to get upstream"))?;
    let context = HandlerContext {
      #[cfg(feature = "sticky-cookie")]
      context_lb: context_from_lb,
      #[cfg(not(feature = "sticky-cookie"))]
      context_lb: None,
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
    let org_pq = req.uri().path_and_query().map(|pq| pq.as_str()).unwrap_or("/").as_bytes();

    // replace some parts of path if opt_replace_path is enabled for chosen upstream
    let new_pq = match &upstream_candidates.replace_path {
      Some(new_path) => {
        let matched_path: &[u8] = upstream_candidates.path.as_ref();
        ensure!(
          !matched_path.is_empty() && org_pq.len() >= matched_path.len(),
          "Upstream uri `path and query` is broken"
        );
        let mut new_pq = Vec::<u8>::with_capacity(org_pq.len() - matched_path.len() + new_path.len());
        new_pq.extend_from_slice(new_path.as_ref());
        new_pq.extend_from_slice(&org_pq[matched_path.len()..]);
        new_pq
      }
      None => org_pq.to_vec(),
    };
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
