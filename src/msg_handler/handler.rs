// Highly motivated by https://github.com/felipenoris/hyper-reverse-proxy
use super::{utils_headers::*, utils_request::*, utils_response::ResLog, utils_synth_response::*};
use crate::{
  backend::{ServerNameLC, Upstream},
  error::*,
  globals::Globals,
  log::*,
};
use hyper::{
  client::connect::Connect,
  header::{self, HeaderValue},
  http::uri::Scheme,
  Body, Client, Request, Response, StatusCode, Uri, Version,
};
use std::{env, net::SocketAddr, sync::Arc};
use tokio::{io::copy_bidirectional, time::timeout};

#[derive(Clone)]
pub struct HttpMessageHandler<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub forwarder: Arc<Client<T>>,
  pub globals: Arc<Globals>,
}

impl<T> HttpMessageHandler<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  fn return_with_error_log(
    &self,
    status_code: StatusCode,
    log_data: &mut MessageLog,
  ) -> Result<Response<Body>> {
    log_data.status_code(&status_code).output();
    http_error(status_code)
  }

  pub async fn handle_request(
    self,
    mut req: Request<Body>,
    client_addr: SocketAddr, // アクセス制御用
    listen_addr: SocketAddr,
    tls_enabled: bool,
    tls_server_name: Option<ServerNameLC>,
  ) -> Result<Response<Body>> {
    req.log_debug(&client_addr, Some("(from Client)"));
    ////////
    let mut log_data = MessageLog::from(&req);
    log_data.client_addr(&client_addr);
    //////

    // Here we start to handle with server_name
    let server_name_bytes = if let Ok(v) = req.parse_host() {
      v.to_ascii_lowercase()
    } else {
      return self.return_with_error_log(StatusCode::BAD_REQUEST, &mut log_data);
    };
    // check consistency of between TLS SNI and HOST/Request URI Line.
    #[allow(clippy::collapsible_if)]
    if tls_enabled && self.globals.sni_consistency {
      if !server_name_bytes.eq_ignore_ascii_case(&tls_server_name.unwrap_or_default()) {
        return self.return_with_error_log(StatusCode::MISDIRECTED_REQUEST, &mut log_data);
      }
    }
    // Find backend application for given server_name, and drop if incoming request is invalid as request.
    let backend = if let Some(be) = self.globals.backends.apps.get(&server_name_bytes) {
      be
    } else if let Some(default_server_name) = &self.globals.backends.default_server_name {
      debug!("Serving by default app");
      self.globals.backends.apps.get(default_server_name).unwrap()
    } else {
      return self.return_with_error_log(StatusCode::SERVICE_UNAVAILABLE, &mut log_data);
    };

    // Redirect to https if !tls_enabled and redirect_to_https is true
    if !tls_enabled && backend.https_redirection.unwrap_or(false) {
      debug!("Redirect to secure connection: {}", &backend.server_name);
      log_data
        .status_code(&StatusCode::PERMANENT_REDIRECT)
        .output();
      return secure_redirection(&backend.server_name, self.globals.https_port, &req);
    }

    // Find reverse proxy for given path and choose one of upstream host
    // Longest prefix match
    let path = req.uri().path();
    let upstream = if let Some(upstream) = backend.reverse_proxy.get(path) {
      upstream
    } else {
      return self.return_with_error_log(StatusCode::NOT_FOUND, &mut log_data);
    };
    let upstream_scheme_host = if let Some(u) = upstream.get() {
      u
    } else {
      return self.return_with_error_log(StatusCode::SERVICE_UNAVAILABLE, &mut log_data);
    };

    // Upgrade in request header
    let upgrade_in_request = extract_upgrade(req.headers());
    let request_upgraded = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>();

    // Build request from destination information
    let req_forwarded = if let Ok(req) = self.generate_request_forwarded(
      &client_addr,
      &listen_addr,
      req,
      upstream_scheme_host,
      &upgrade_in_request,
      upstream,
      tls_enabled,
    ) {
      req
    } else {
      error!("Failed to generate destination uri for reverse proxy");
      return self.return_with_error_log(StatusCode::SERVICE_UNAVAILABLE, &mut log_data);
    };
    // debug!("Request to be forwarded: {:?}", req_forwarded);
    req_forwarded.log_debug(&client_addr, Some("(to Backend)"));
    log_data.xff(&req_forwarded.headers().get("x-forwarded-for"));
    log_data.upstream(&upstream_scheme_host.to_string());
    //////

    // Forward request to
    let mut res_backend = {
      match timeout(
        self.globals.upstream_timeout,
        self.forwarder.request(req_forwarded),
      )
      .await
      {
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

    res_backend.log_debug(&backend.server_name, &client_addr, Some("(from Backend)"));
    // let response_log = res_backend.status().to_string();

    if res_backend.status() != StatusCode::SWITCHING_PROTOCOLS {
      // Generate response to client
      if self.generate_response_forwarded(&mut res_backend).is_ok() {
        // info!("{} => {}", request_log, response_log);
        res_backend.log_debug(&backend.server_name, &client_addr, Some("(to Client)"));
        log_data.status_code(&res_backend.status()).output();
        return Ok(res_backend);
      } else {
        return self.return_with_error_log(StatusCode::INTERNAL_SERVER_ERROR, &mut log_data);
      }
    }

    // Handle StatusCode::SWITCHING_PROTOCOLS in response
    let upgrade_in_response = extract_upgrade(res_backend.headers());
    if if let (Some(u_req), Some(u_res)) =
      (upgrade_in_request.as_ref(), upgrade_in_response.as_ref())
    {
      u_req.to_ascii_lowercase() == u_res.to_ascii_lowercase()
    } else {
      false
    } {
      if let Some(request_upgraded) = request_upgraded {
        let onupgrade = if let Some(onupgrade) = res_backend
          .extensions_mut()
          .remove::<hyper::upgrade::OnUpgrade>()
        {
          onupgrade
        } else {
          error!("Response does not have an upgrade extension");
          return self.return_with_error_log(StatusCode::INTERNAL_SERVER_ERROR, &mut log_data);
        };

        self.globals.runtime_handle.spawn(async move {
          let mut response_upgraded = onupgrade.await.map_err(|e| {
            error!("Failed to upgrade response: {}", e);
            anyhow!("Failed to upgrade response: {}", e)
          })?;
          let mut request_upgraded = request_upgraded.await.map_err(|e| {
            error!("Failed to upgrade request: {}", e);
            anyhow!("Failed to upgrade request: {}", e)
          })?;
          copy_bidirectional(&mut response_upgraded, &mut request_upgraded)
            .await
            .map_err(|e| anyhow!("Coping between upgraded connections failed: {}", e))?;
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
  ) -> Result<()> {
    let headers = response.headers_mut();
    remove_connection_header(headers);
    remove_hop_header(headers);
    add_header_entry_overwrite_if_exist(headers, "server", env!("CARGO_PKG_NAME"))?;

    #[cfg(feature = "h3")]
    {
      if self.globals.http3 {
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
      }
    }

    Ok(())
  }

  #[allow(clippy::too_many_arguments)]
  fn generate_request_forwarded<B: core::fmt::Debug>(
    &self,
    client_addr: &SocketAddr,
    listen_addr: &SocketAddr,
    mut req: Request<B>,
    upstream_scheme_host: &Uri,
    upgrade: &Option<String>,
    upstream: &Upstream,
    tls_enabled: bool,
  ) -> Result<Request<B>> {
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

    let headers = req.headers_mut();
    // delete headers specified in header.connection
    remove_connection_header(headers);
    // delete hop headers including header.connection
    remove_hop_header(headers);
    // X-Forwarded-For
    add_forwarding_header(headers, client_addr, listen_addr, tls_enabled)?;

    // Add te: trailer if te_trailer
    if contains_te_trailers {
      headers.insert(
        header::TE,
        HeaderValue::from_bytes("trailers".as_bytes()).unwrap(),
      );
    }

    // add "host" header of original server_name if not exist (default)
    if req.headers().get(header::HOST).is_none() {
      let org_host = req
        .uri()
        .host()
        .ok_or_else(|| anyhow!("Invalid request"))?
        .to_owned();
      req
        .headers_mut()
        .insert(header::HOST, HeaderValue::from_str(&org_host)?);
    };

    // apply upstream-specific headers given in upstream_option
    let headers = req.headers_mut();
    apply_upstream_options_to_header(headers, client_addr, upstream_scheme_host, upstream)?;

    // update uri in request
    ensure!(upstream_scheme_host.authority().is_some() && upstream_scheme_host.scheme().is_some());
    let new_uri = Uri::builder()
      .scheme(upstream_scheme_host.scheme().unwrap().as_str())
      .authority(upstream_scheme_host.authority().unwrap().as_str());
    let pq = req.uri().path_and_query();
    *req.uri_mut() = match pq {
      None => new_uri,
      Some(x) => new_uri.path_and_query(x.to_owned()),
    }
    .build()?;

    // upgrade
    if let Some(v) = upgrade {
      req.headers_mut().insert(header::UPGRADE, v.parse()?);
      req
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_str("upgrade")?);
    }

    // Change version to http/1.1 when destination scheme is http
    if req.version() != Version::HTTP_11 && upstream_scheme_host.scheme() == Some(&Scheme::HTTP) {
      *req.version_mut() = Version::HTTP_11;
    } else if req.version() == Version::HTTP_3 {
      debug!("HTTP/3 is currently unsupported for request to upstream. Use HTTP/2.");
      *req.version_mut() = Version::HTTP_2;
    }

    Ok(req)
  }
}
