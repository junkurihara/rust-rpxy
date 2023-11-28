use super::{
  handler_main::HandlerContext, utils_headers::*, utils_request::apply_upstream_options_to_request_line,
  HttpMessageHandler,
};
use crate::{
  backend::{BackendApp, UpstreamCandidates},
  constants::RESPONSE_HEADER_SERVER,
  log::*,
  CryptoSource,
};
use anyhow::{anyhow, ensure, Result};
use http::{header, uri::Scheme, HeaderValue, Request, Response, Uri, Version};
use std::net::SocketAddr;

impl<U> HttpMessageHandler<U>
where
  U: CryptoSource + Clone,
{
  ////////////////////////////////////////////////////
  // Functions to generate messages
  ////////////////////////////////////////////////////

  /// Manipulate a response message sent from a backend application to forward downstream to a client.
  pub(super) fn generate_response_forwarded<B>(
    &self,
    response: &mut Response<B>,
    backend_app: &BackendApp<U>,
  ) -> Result<()> {
    let headers = response.headers_mut();
    remove_connection_header(headers);
    remove_hop_header(headers);
    add_header_entry_overwrite_if_exist(headers, "server", RESPONSE_HEADER_SERVER)?;

    #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
    {
      // Manipulate ALT_SVC allowing h3 in response message only when mutual TLS is not enabled
      // TODO: This is a workaround for avoiding a client authentication in HTTP/3
      if self.globals.proxy_config.http3 && backend_app.crypto_source.as_ref().is_some_and(|v| !v.is_mutual_tls()) {
        if let Some(port) = self.globals.proxy_config.https_port {
          add_header_entry_overwrite_if_exist(
            headers,
            header::ALT_SVC.as_str(),
            format!(
              "h3=\":{}\"; ma={}, h3-29=\":{}\"; ma={}",
              port, self.globals.proxy_config.h3_alt_svc_max_age, port, self.globals.proxy_config.h3_alt_svc_max_age
            ),
          )?;
        }
      } else {
        // remove alt-svc to disallow requests via http3
        headers.remove(header::ALT_SVC.as_str());
      }
    }
    #[cfg(not(any(feature = "http3-quinn", feature = "http3-s2n")))]
    {
      if self.globals.proxy_config.https_port.is_some() {
        headers.remove(header::ALT_SVC.as_str());
      }
    }

    Ok(())
  }

  #[allow(clippy::too_many_arguments)]
  /// Manipulate a request message sent from a client to forward upstream to a backend application
  pub(super) fn generate_request_forwarded<B>(
    &self,
    client_addr: &SocketAddr,
    listen_addr: &SocketAddr,
    req: &mut Request<B>,
    upgrade: &Option<String>,
    upstream_candidates: &UpstreamCandidates,
    tls_enabled: bool,
  ) -> Result<HandlerContext> {
    debug!("Generate request to be forwarded");

    // Add te: trailer if contained in original request
    let contains_te_trailers = {
      if let Some(te) = req.headers().get(header::TE) {
        te.as_bytes()
          .split(|v| v == &b',' || v == &b' ')
          .any(|x| x == "trailers".as_bytes())
      } else {
        false
      }
    };

    let uri = req.uri().to_string();
    let headers = req.headers_mut();
    // delete headers specified in header.connection
    remove_connection_header(headers);
    // delete hop headers including header.connection
    remove_hop_header(headers);
    // X-Forwarded-For
    add_forwarding_header(headers, client_addr, listen_addr, tls_enabled, &uri)?;

    // Add te: trailer if te_trailer
    if contains_te_trailers {
      headers.insert(header::TE, HeaderValue::from_bytes("trailers".as_bytes()).unwrap());
    }

    // add "host" header of original server_name if not exist (default)
    if req.headers().get(header::HOST).is_none() {
      let org_host = req.uri().host().ok_or_else(|| anyhow!("Invalid request"))?.to_owned();
      req
        .headers_mut()
        .insert(header::HOST, HeaderValue::from_str(&org_host)?);
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
    // by default, host header is overwritten with upstream hostname
    override_host_header(headers, &upstream_chosen.uri)?;
    // apply upstream options to header
    apply_upstream_options_to_header(headers, upstream_candidates)?;

    // update uri in request
    ensure!(
      upstream_chosen.uri.authority().is_some() && upstream_chosen.uri.scheme().is_some(),
      "Upstream uri `scheme` and `authority` is broken"
    );

    let new_uri = Uri::builder()
      .scheme(upstream_chosen.uri.scheme().unwrap().as_str())
      .authority(upstream_chosen.uri.authority().unwrap().as_str());
    let org_pq = match req.uri().path_and_query() {
      Some(pq) => pq.to_string(),
      None => "/".to_string(),
    }
    .into_bytes();

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
      None => org_pq,
    };
    *req.uri_mut() = new_uri.path_and_query(new_pq).build()?;

    // upgrade
    if let Some(v) = upgrade {
      req.headers_mut().insert(header::UPGRADE, v.parse()?);
      req
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    }

    // If not specified (force_httpXX_upstream) and https, version is preserved except for http/3
    if upstream_chosen.uri.scheme() == Some(&Scheme::HTTP) {
      // Change version to http/1.1 when destination scheme is http
      debug!("Change version to http/1.1 when destination scheme is http unless upstream option enabled.");
      *req.version_mut() = Version::HTTP_11;
    } else if req.version() == Version::HTTP_3 {
      // HTTP/3 is always https
      debug!("HTTP/3 is currently unsupported for request to upstream.");
      *req.version_mut() = Version::HTTP_2;
    }

    apply_upstream_options_to_request_line(req, upstream_candidates)?;

    Ok(context)
  }
}
