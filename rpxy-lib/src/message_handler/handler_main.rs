use super::{
  http_log::HttpMessageLog,
  http_result::{HttpError, HttpResult},
  synthetic_response::{secure_redirection_response, synthetic_error_response},
  utils_headers::*,
  utils_request::InspectParseHost,
};
use crate::{
  backend::{BackendAppManager, LoadBalanceContext},
  error::*,
  forwarder::{ForwardRequest, Forwarder},
  globals::Globals,
  hyper_ext::body::{RequestBody, ResponseBody},
  log::*,
  name_exp::ServerName,
};
use derive_builder::Builder;
use http::{Method, Request, Response, StatusCode};
use hyper_util::{client::legacy::connect::Connect, rt::TokioIo};
use std::{net::SocketAddr, sync::Arc};
use tokio::io::copy_bidirectional;

#[allow(dead_code)]
#[derive(Debug)]
/// Context object to handle sticky cookies at HTTP message handler
pub(super) struct HandlerContext {
  #[cfg(feature = "sticky-cookie")]
  pub(super) context_lb: Option<LoadBalanceContext>,
  #[cfg(not(feature = "sticky-cookie"))]
  pub(super) context_lb: Option<()>,
}

#[derive(Clone, Builder)]
/// HTTP message handler for requests from clients and responses from backend applications,
/// responsible to manipulate and forward messages to upstream backends and downstream clients.
pub struct HttpMessageHandler<C>
where
  C: Send + Sync + Connect + Clone + 'static,
{
  forwarder: Arc<Forwarder<C>>,
  pub(super) globals: Arc<Globals>,
  app_manager: Arc<BackendAppManager>,
}

impl<C> HttpMessageHandler<C>
where
  C: Send + Sync + Connect + Clone + 'static,
{
  /// Handle incoming request message from a client.
  /// Responsible to passthrough responses from backend applications or generate synthetic error responses.
  pub async fn handle_request(
    &self,
    req: Request<RequestBody>,
    client_addr: SocketAddr, // For access control
    listen_addr: SocketAddr,
    tls_enabled: bool,
    tls_server_name: Option<ServerName>,
  ) -> RpxyResult<Response<ResponseBody>> {
    // preparing log data
    let mut log_data = HttpMessageLog::from(&req);
    log_data.client_addr(&client_addr);

    let http_result = self
      .handle_request_inner(&mut log_data, req, client_addr, listen_addr, tls_enabled, tls_server_name)
      .await;

    // passthrough or synthetic response
    match http_result {
      Ok(v) => {
        log_data.status_code(&v.status()).output();
        Ok(v)
      }
      Err(e) => {
        error!("{e}");
        let code = StatusCode::from(e);
        log_data.status_code(&code).output();
        synthetic_error_response(code)
      }
    }
  }

  /// Handle inner with no synthetic error response.
  /// Synthetic response is generated by caller.
  async fn handle_request_inner(
    &self,
    log_data: &mut HttpMessageLog,
    mut req: Request<RequestBody>,
    client_addr: SocketAddr, // For access control
    listen_addr: SocketAddr,
    tls_enabled: bool,
    tls_server_name: Option<ServerName>,
  ) -> HttpResult<Response<ResponseBody>> {
    // Block CONNECT requests because a) makes no sense to run a forward proxy behind a reverse proxy = fringe use case b) might have serious security implications for badly configured upstreams c) it doesn't work with current implementation (bodies are not forwarded)
    if matches!(*req.method(), Method::CONNECT) {
      return Err(HttpError::UnsupportedMethod);
    }

    // Here we start to inspect and parse with server_name
    let server_name = req
      .inspect_parse_host()
      .map(|v| ServerName::from(v.as_slice()))
      .map_err(|_e| HttpError::InvalidHostInRequestHeader)?;

    // check consistency of between TLS SNI and HOST/Request URI Line.
    #[allow(clippy::collapsible_if)]
    if tls_enabled && self.globals.proxy_config.sni_consistency {
      if server_name != tls_server_name.unwrap_or_default() {
        return Err(HttpError::SniHostInconsistency);
      }
    }
    // Find backend application for given server_name, and drop if incoming request is invalid as request.
    let backend_app = match self.app_manager.apps.get(&server_name) {
      Some(backend_app) => backend_app,
      None => {
        let Some(default_server_name) = &self.app_manager.default_server_name else {
          return Err(HttpError::NoMatchingBackendApp);
        };
        debug!("Serving by default app");
        self.app_manager.apps.get(default_server_name).unwrap()
      }
    };

    // Redirect to https if !tls_enabled and redirect_to_https is true
    if !tls_enabled && backend_app.https_redirection.unwrap_or(false) {
      debug!(
        "Redirect to secure connection: {}",
        <&ServerName as TryInto<String>>::try_into(&backend_app.server_name).unwrap_or_default()
      );
      return secure_redirection_response(
        &backend_app.server_name,
        self.globals.proxy_config.https_redirection_port,
        &req,
      );
    }

    // Find reverse proxy for given path and choose one of upstream host
    // Longest prefix match
    let path = req.uri().path();
    let Some(upstream_candidates) = backend_app.path_manager.get(path) else {
      return Err(HttpError::NoUpstreamCandidates);
    };

    // Upgrade in request header
    let upgrade_in_request = extract_upgrade(req.headers());
    if upgrade_in_request.is_some() && req.version() != http::Version::HTTP_11 {
      return Err(HttpError::FailedToUpgrade(format!(
        "Unsupported HTTP version: {:?}",
        req.version()
      )));
    }
    // let request_upgraded = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>();
    let req_on_upgrade = hyper::upgrade::on(&mut req);

    // Build request from destination information
    let _context = match self.generate_request_forwarded(
      &client_addr,
      &listen_addr,
      &mut req,
      &upgrade_in_request,
      upstream_candidates,
      tls_enabled,
    ) {
      Err(e) => {
        return Err(HttpError::FailedToGenerateUpstreamRequest(e.to_string()));
      }
      Ok(v) => v,
    };
    debug!(
      "Request to be forwarded: [uri {}, method: {}, version {:?}, headers {:?}]",
      req.uri(),
      req.method(),
      req.version(),
      req.headers()
    );
    log_data.xff(&req.headers().get("x-forwarded-for"));
    log_data.upstream(req.uri());
    //////

    //////////////
    // Forward request to a chosen backend
    let mut res_backend = match self.forwarder.request(req).await {
      Ok(v) => v,
      Err(e) => {
        return Err(HttpError::FailedToGetResponseFromBackend(e.to_string()));
      }
    };
    //////////////
    // Process reverse proxy context generated during the forwarding request generation.
    #[cfg(feature = "sticky-cookie")]
    if let Some(context_from_lb) = _context.context_lb {
      let res_headers = res_backend.headers_mut();
      if let Err(e) = set_sticky_cookie_lb_context(res_headers, &context_from_lb) {
        return Err(HttpError::FailedToAddSetCookeInResponse(e.to_string()));
      }
    }

    if res_backend.status() != StatusCode::SWITCHING_PROTOCOLS {
      // Generate response to client
      if let Err(e) = self.generate_response_forwarded(&mut res_backend, backend_app) {
        return Err(HttpError::FailedToGenerateDownstreamResponse(e.to_string()));
      }
      return Ok(res_backend);
    }

    // Handle StatusCode::SWITCHING_PROTOCOLS in response
    let upgrade_in_response = extract_upgrade(res_backend.headers());
    let should_upgrade = match (upgrade_in_request.as_ref(), upgrade_in_response.as_ref()) {
      (Some(u_req), Some(u_res)) => u_req.eq_ignore_ascii_case(u_res),
      _ => false,
    };

    if !should_upgrade {
      return Err(HttpError::FailedToUpgrade(format!(
        "Backend tried to switch to protocol {:?} when {:?} was requested",
        upgrade_in_response, upgrade_in_request
      )));
    }
    // let Some(request_upgraded) = request_upgraded else {
    //   return Err(HttpError::NoUpgradeExtensionInRequest);
    // };

    // let Some(onupgrade) = res_backend.extensions_mut().remove::<hyper::upgrade::OnUpgrade>() else {
    //   return Err(HttpError::NoUpgradeExtensionInResponse);
    // };
    let res_on_upgrade = hyper::upgrade::on(&mut res_backend);

    self.globals.runtime_handle.spawn(async move {
      let mut response_upgraded = TokioIo::new(res_on_upgrade.await.map_err(|e| {
        error!("Failed to upgrade response: {}", e);
        RpxyError::FailedToUpgradeResponse(e.to_string())
      })?);
      let mut request_upgraded = TokioIo::new(req_on_upgrade.await.map_err(|e| {
        error!("Failed to upgrade request: {}", e);
        RpxyError::FailedToUpgradeRequest(e.to_string())
      })?);
      copy_bidirectional(&mut response_upgraded, &mut request_upgraded)
        .await
        .map_err(|e| {
          error!("Coping between upgraded connections failed: {}", e);
          RpxyError::FailedToCopyBidirectional(e.to_string())
        })?;
      Ok(()) as RpxyResult<()>
    });

    Ok(res_backend)
  }
}
