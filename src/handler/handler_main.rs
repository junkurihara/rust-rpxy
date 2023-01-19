// Highly motivated by https://github.com/felipenoris/hyper-reverse-proxy
use super::{utils_headers::*, utils_request::*, utils_synth_response::*};
use crate::{
  backend::{Backend, UpstreamGroup},
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
pub struct HttpMessageHandler<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  forwarder: Arc<Client<T>>,
  globals: Arc<Globals>,
}

impl<T> HttpMessageHandler<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  fn return_with_error_log(&self, status_code: StatusCode, log_data: &mut MessageLog) -> Result<Response<Body>> {
    log_data.status_code(&status_code).output();
    http_error(status_code)
  }

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
    if tls_enabled && self.globals.sni_consistency {
      if server_name != tls_server_name.unwrap_or_default() {
        return self.return_with_error_log(StatusCode::MISDIRECTED_REQUEST, &mut log_data);
      }
    }
    // Find backend application for given server_name, and drop if incoming request is invalid as request.
    let backend = if let Some(be) = self.globals.backends.apps.get(&server_name) {
      be
    } else if let Some(default_server_name) = &self.globals.backends.default_server_name_bytes {
      debug!("Serving by default app");
      self.globals.backends.apps.get(default_server_name).unwrap()
    } else {
      return self.return_with_error_log(StatusCode::SERVICE_UNAVAILABLE, &mut log_data);
    };

    // Redirect to https if !tls_enabled and redirect_to_https is true
    if !tls_enabled && backend.https_redirection.unwrap_or(false) {
      debug!("Redirect to secure connection: {}", &backend.server_name);
      log_data.status_code(&StatusCode::PERMANENT_REDIRECT).output();
      return secure_redirection(&backend.server_name, self.globals.https_port, &req);
    }

    // Find reverse proxy for given path and choose one of upstream host
    // Longest prefix match
    let path = req.uri().path();
    let upstream_group = match backend.reverse_proxy.get(path) {
      Some(ug) => ug,
      None => return self.return_with_error_log(StatusCode::NOT_FOUND, &mut log_data),
    };

    // Upgrade in request header
    let upgrade_in_request = extract_upgrade(req.headers());
    let request_upgraded = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>();

    // Build request from destination information
    if let Err(e) = self.generate_request_forwarded(
      &client_addr,
      &listen_addr,
      &mut req,
      &upgrade_in_request,
      upstream_group,
      tls_enabled,
    ) {
      error!("Failed to generate destination uri for reverse proxy: {}", e);
      return self.return_with_error_log(StatusCode::SERVICE_UNAVAILABLE, &mut log_data);
    };
    debug!("Request to be forwarded: {:?}", req);
    log_data.xff(&req.headers().get("x-forwarded-for"));
    log_data.upstream(req.uri());
    //////

    // Forward request to
    let mut res_backend = {
      match timeout(self.globals.upstream_timeout, self.forwarder.request(req)).await {
        Err(_) => {
          return self.return_with_error_log(StatusCode::GATEWAY_TIMEOUT, &mut log_data);
        }
        Ok(x) => match x {
          Ok(res) => res,
          Err(e) => {
            error!("Failed to get response from backend: {}", e);
            return self.return_with_error_log(StatusCode::SERVICE_UNAVAILABLE, &mut log_data);
          }
        },
      }
    };

    if res_backend.status() != StatusCode::SWITCHING_PROTOCOLS {
      // Generate response to client
      if self.generate_response_forwarded(&mut res_backend, backend).is_ok() {
        log_data.status_code(&res_backend.status()).output();
        return Ok(res_backend);
      } else {
        return self.return_with_error_log(StatusCode::INTERNAL_SERVER_ERROR, &mut log_data);
      }
    }

    // Handle StatusCode::SWITCHING_PROTOCOLS in response
    let upgrade_in_response = extract_upgrade(res_backend.headers());
    if if let (Some(u_req), Some(u_res)) = (upgrade_in_request.as_ref(), upgrade_in_response.as_ref()) {
      u_req.to_ascii_lowercase() == u_res.to_ascii_lowercase()
    } else {
      false
    } {
      if let Some(request_upgraded) = request_upgraded {
        let onupgrade = if let Some(onupgrade) = res_backend.extensions_mut().remove::<hyper::upgrade::OnUpgrade>() {
          onupgrade
        } else {
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
      } else {
        error!("Request does not have an upgrade extension");
        self.return_with_error_log(StatusCode::BAD_REQUEST, &mut log_data)
      }
    } else {
      error!(
        "Backend tried to switch to protocol {:?} when {:?} was requested",
        upgrade_in_response, upgrade_in_request
      );
      self.return_with_error_log(StatusCode::INTERNAL_SERVER_ERROR, &mut log_data)
    }
  }

  ////////////////////////////////////////////////////
  // Functions to generate messages

  fn generate_response_forwarded<B: core::fmt::Debug>(
    &self,
    response: &mut Response<B>,
    chosen_backend: &Backend,
  ) -> Result<()> {
    let headers = response.headers_mut();
    remove_connection_header(headers);
    remove_hop_header(headers);
    add_header_entry_overwrite_if_exist(headers, "server", env!("CARGO_PKG_NAME"))?;

    #[cfg(feature = "http3")]
    {
      // TODO: Workaround for avoid h3 for client authentication
      if self.globals.http3 && chosen_backend.client_ca_cert_path.is_none() {
        if let Some(port) = self.globals.https_port {
          add_header_entry_overwrite_if_exist(
            headers,
            header::ALT_SVC.as_str(),
            format!(
              "h3=\":{}\"; ma={}, h3-29=\":{}\"; ma={}",
              port, self.globals.h3_alt_svc_max_age, port, self.globals.h3_alt_svc_max_age
            ),
          )?;
        }
      } else {
        // remove alt-svc to disallow requests via http3
        headers.remove(header::ALT_SVC.as_str());
      }
    }
    #[cfg(not(feature = "http3"))]
    {
      if let Some(port) = self.globals.https_port {
        headers.remove(header::ALT_SVC.as_str());
      }
    }

    Ok(())
  }

  #[allow(clippy::too_many_arguments)]
  fn generate_request_forwarded<B>(
    &self,
    client_addr: &SocketAddr,
    listen_addr: &SocketAddr,
    req: &mut Request<B>,
    upgrade: &Option<String>,
    upstream_group: &UpstreamGroup,
    tls_enabled: bool,
  ) -> Result<()> {
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

    // Fix unique upstream destination since there could be multiple ones.
    let upstream_chosen = upstream_group.get().ok_or_else(|| anyhow!("Failed to get upstream"))?;

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

    Ok(())
  }
}
