// Highly motivated by https://github.com/felipenoris/hyper-reverse-proxy
use super::{utils_headers::*, utils_request::*, utils_synth_response::*, HandlerContext};
use crate::{
  backend::{Backend, UpstreamGroup},
  certs::CryptoSource,
  error::*,
  globals::Globals,
  log::*,
  utils::ServerNameBytesExp,
};
use derive_builder::Builder;
use hyper::{
  client::connect::Connect,
  header::{self, HeaderValue},
  http::uri::Scheme,
  Body, Client, Request, Response, StatusCode, Uri, Version,
};
use std::{env, net::SocketAddr, sync::Arc};
use tokio::{io::copy_bidirectional, time::timeout};

#[derive(Clone, Builder)]
/// HTTP message handler for requests from clients and responses from backend applications,
/// responsible to manipulate and forward messages to upstream backends and downstream clients.
pub struct HttpMessageHandler<T, U>
where
  T: Connect + Clone + Sync + Send + 'static,
  U: CryptoSource + Clone,
{
  forwarder: Arc<Client<T>>,
  globals: Arc<Globals<U>>,
}

impl<T, U> HttpMessageHandler<T, U>
where
  T: Connect + Clone + Sync + Send + 'static,
  U: CryptoSource + Clone,
{
  /// Return with an arbitrary status code of error and log message
  fn return_with_error_log(&self, status_code: StatusCode, log_data: &mut MessageLog) -> Result<Response<Body>> {
    log_data.status_code(&status_code).output();
    http_error(status_code)
  }

  /// Handle incoming request message from a client
  pub async fn handle_request(
    self,
    mut req: Request<Body>,
    client_addr: SocketAddr, // アクセス制御用
    listen_addr: SocketAddr,
    tls_enabled: bool,
    tls_server_name: Option<ServerNameBytesExp>,
  ) -> Result<Response<Body>> {
    ////////
    let mut log_data = MessageLog::from(&req);
    log_data.client_addr(&client_addr);
    //////

    // Here we start to handle with server_name
    let server_name = if let Ok(v) = req.parse_host() {
      ServerNameBytesExp::from(v)
    } else {
      return self.return_with_error_log(StatusCode::BAD_REQUEST, &mut log_data);
    };
    // check consistency of between TLS SNI and HOST/Request URI Line.
    #[allow(clippy::collapsible_if)]
    if tls_enabled && self.globals.proxy_config.sni_consistency {
      if server_name != tls_server_name.unwrap_or_default() {
        return self.return_with_error_log(StatusCode::MISDIRECTED_REQUEST, &mut log_data);
      }
    }
    // Find backend application for given server_name, and drop if incoming request is invalid as request.
    let backend = match self.globals.backends.apps.get(&server_name) {
      Some(be) => be,
      None => {
        let Some(default_server_name) = &self.globals.backends.default_server_name_bytes else {
          return self.return_with_error_log(StatusCode::SERVICE_UNAVAILABLE, &mut log_data);
        };
        debug!("Serving by default app");
        self.globals.backends.apps.get(default_server_name).unwrap()
      }
    };

    // Redirect to https if !tls_enabled and redirect_to_https is true
    if !tls_enabled && backend.https_redirection.unwrap_or(false) {
      debug!("Redirect to secure connection: {}", &backend.server_name);
      log_data.status_code(&StatusCode::PERMANENT_REDIRECT).output();
      return secure_redirection(&backend.server_name, self.globals.proxy_config.https_port, &req);
    }

    // Find reverse proxy for given path and choose one of upstream host
    // Longest prefix match
    let path = req.uri().path();
    let Some(upstream_group) = backend.reverse_proxy.get(path) else {
      return self.return_with_error_log(StatusCode::NOT_FOUND, &mut log_data)
    };

    // Upgrade in request header
    let upgrade_in_request = extract_upgrade(req.headers());
    let request_upgraded = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>();

    // Build request from destination information
    let _context = match self.generate_request_forwarded(
      &client_addr,
      &listen_addr,
      &mut req,
      &upgrade_in_request,
      upstream_group,
      tls_enabled,
    ) {
      Err(e) => {
        error!("Failed to generate destination uri for reverse proxy: {}", e);
        return self.return_with_error_log(StatusCode::SERVICE_UNAVAILABLE, &mut log_data);
      }
      Ok(v) => v,
    };
    debug!("Request to be forwarded: {:?}", req);
    log_data.xff(&req.headers().get("x-forwarded-for"));
    log_data.upstream(req.uri());
    //////

    // Forward request to a chosen backend
    let mut res_backend = {
      let Ok(result) = timeout(self.globals.proxy_config.upstream_timeout, self.forwarder.request(req)).await else {
        return self.return_with_error_log(StatusCode::GATEWAY_TIMEOUT, &mut log_data);
      };
      match result {
        Ok(res) => res,
        Err(e) => {
          error!("Failed to get response from backend: {}", e);
          return self.return_with_error_log(StatusCode::SERVICE_UNAVAILABLE, &mut log_data);
        }
      }
    };

    // Process reverse proxy context generated during the forwarding request generation.
    #[cfg(feature = "sticky-cookie")]
    if let Some(context_from_lb) = _context.context_lb {
      let res_headers = res_backend.headers_mut();
      if let Err(e) = set_sticky_cookie_lb_context(res_headers, &context_from_lb) {
        error!("Failed to append context to the response given from backend: {}", e);
        return self.return_with_error_log(StatusCode::BAD_GATEWAY, &mut log_data);
      }
    }

    if res_backend.status() != StatusCode::SWITCHING_PROTOCOLS {
      // Generate response to client
      if self.generate_response_forwarded(&mut res_backend, backend).is_err() {
        return self.return_with_error_log(StatusCode::INTERNAL_SERVER_ERROR, &mut log_data);
      }
      log_data.status_code(&res_backend.status()).output();
      return Ok(res_backend);
    }

    // Handle StatusCode::SWITCHING_PROTOCOLS in response
    let upgrade_in_response = extract_upgrade(res_backend.headers());
    let should_upgrade = if let (Some(u_req), Some(u_res)) = (upgrade_in_request.as_ref(), upgrade_in_response.as_ref())
    {
      u_req.to_ascii_lowercase() == u_res.to_ascii_lowercase()
    } else {
      false
    };
    if !should_upgrade {
      error!(
        "Backend tried to switch to protocol {:?} when {:?} was requested",
        upgrade_in_response, upgrade_in_request
      );
      return self.return_with_error_log(StatusCode::INTERNAL_SERVER_ERROR, &mut log_data);
    }
    let Some(request_upgraded) = request_upgraded else {
      error!("Request does not have an upgrade extension");
      return self.return_with_error_log(StatusCode::BAD_REQUEST, &mut log_data);
    };
    let Some(onupgrade) = res_backend.extensions_mut().remove::<hyper::upgrade::OnUpgrade>() else {
      error!("Response does not have an upgrade extension");
      return self.return_with_error_log(StatusCode::INTERNAL_SERVER_ERROR, &mut log_data);
    };

    self.globals.runtime_handle.spawn(async move {
      let mut response_upgraded = onupgrade.await.map_err(|e| {
        error!("Failed to upgrade response: {}", e);
        RpxyError::Hyper(e)
      })?;
      let mut request_upgraded = request_upgraded.await.map_err(|e| {
        error!("Failed to upgrade request: {}", e);
        RpxyError::Hyper(e)
      })?;
      copy_bidirectional(&mut response_upgraded, &mut request_upgraded)
        .await
        .map_err(|e| {
          error!("Coping between upgraded connections failed: {}", e);
          RpxyError::Io(e)
        })?;
      Ok(()) as Result<()>
    });
    log_data.status_code(&res_backend.status()).output();
    Ok(res_backend)
  }

  ////////////////////////////////////////////////////
  // Functions to generate messages
  ////////////////////////////////////////////////////

  /// Manipulate a response message sent from a backend application to forward downstream to a client.
  fn generate_response_forwarded<B>(&self, response: &mut Response<B>, chosen_backend: &Backend<U>) -> Result<()>
  where
    B: core::fmt::Debug,
  {
    let headers = response.headers_mut();
    remove_connection_header(headers);
    remove_hop_header(headers);
    add_header_entry_overwrite_if_exist(headers, "server", env!("CARGO_PKG_NAME"))?;

    #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
    {
      // Manipulate ALT_SVC allowing h3 in response message only when mutual TLS is not enabled
      // TODO: This is a workaround for avoiding a client authentication in HTTP/3
      if self.globals.proxy_config.http3
        && chosen_backend
          .crypto_source
          .as_ref()
          .is_some_and(|v| !v.is_mutual_tls())
      {
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
      if let Some(port) = self.globals.proxy_config.https_port {
        headers.remove(header::ALT_SVC.as_str());
      }
    }

    Ok(())
  }

  #[allow(clippy::too_many_arguments)]
  /// Manipulate a request message sent from a client to forward upstream to a backend application
  fn generate_request_forwarded<B>(
    &self,
    client_addr: &SocketAddr,
    listen_addr: &SocketAddr,
    req: &mut Request<B>,
    upgrade: &Option<String>,
    upstream_group: &UpstreamGroup,
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
      let context_to_lb = if let crate::backend::LoadBalance::StickyRoundRobin(lb) = &upstream_group.lb {
        takeout_sticky_cookie_lb_context(req.headers_mut(), &lb.sticky_config.name)?
      } else {
        None
      };
      upstream_group.get(&context_to_lb)
    };
    #[cfg(not(feature = "sticky-cookie"))]
    let (upstream_chosen_opt, _) = upstream_group.get(&None);

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
    apply_upstream_options_to_header(headers, client_addr, upstream_group, &upstream_chosen.uri)?;

    // update uri in request
    if !(upstream_chosen.uri.authority().is_some() && upstream_chosen.uri.scheme().is_some()) {
      return Err(RpxyError::Handler("Upstream uri `scheme` and `authority` is broken"));
    };
    let new_uri = Uri::builder()
      .scheme(upstream_chosen.uri.scheme().unwrap().as_str())
      .authority(upstream_chosen.uri.authority().unwrap().as_str());
    let org_pq = match req.uri().path_and_query() {
      Some(pq) => pq.to_string(),
      None => "/".to_string(),
    }
    .into_bytes();

    // replace some parts of path if opt_replace_path is enabled for chosen upstream
    let new_pq = match &upstream_group.replace_path {
      Some(new_path) => {
        let matched_path: &[u8] = upstream_group.path.as_ref();
        if matched_path.is_empty() || org_pq.len() < matched_path.len() {
          return Err(RpxyError::Handler("Upstream uri `path and query` is broken"));
        };
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
        .insert(header::CONNECTION, HeaderValue::from_str("upgrade")?);
    }

    // If not specified (force_httpXX_upstream) and https, version is preserved except for http/3
    apply_upstream_options_to_request_line(req, upstream_group)?;
    // Maybe workaround: Change version to http/1.1 when destination scheme is http
    if req.version() != Version::HTTP_11 && upstream_chosen.uri.scheme() == Some(&Scheme::HTTP) {
      *req.version_mut() = Version::HTTP_11;
    } else if req.version() == Version::HTTP_3 {
      debug!("HTTP/3 is currently unsupported for request to upstream. Use HTTP/2.");
      *req.version_mut() = Version::HTTP_2;
    }

    Ok(context)
  }
}
