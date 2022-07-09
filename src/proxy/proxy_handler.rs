// Highly motivated by https://github.com/felipenoris/hyper-reverse-proxy
use super::{utils_headers::*, utils_request::*, utils_synth_response::*, Proxy, Upstream};
use crate::{constants::*, error::*, log::*};
use hyper::{
  client::connect::Connect,
  header::{self, HeaderValue},
  http::uri::Scheme,
  Body, Request, Response, StatusCode, Uri, Version,
};
use std::net::SocketAddr;
use tokio::io::copy_bidirectional;

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub async fn handle_request(
    self,
    mut req: Request<Body>,
    client_addr: SocketAddr, // アクセス制御用
  ) -> Result<Response<Body>> {
    let request_log = log_request_msg(&req, client_addr);

    // Here we start to handle with server_name
    // Find backend application for given server_name
    let (server_name, _port) = if let Ok(v) = parse_host_port(&req) {
      v
    } else {
      info!("{} => {}", request_log, StatusCode::BAD_REQUEST);
      return http_error(StatusCode::BAD_REQUEST);
    };

    if !self.backends.apps.contains_key(&server_name) && self.backends.default_app.is_none() {
      info!("{} => {}", request_log, StatusCode::SERVICE_UNAVAILABLE);
      return http_error(StatusCode::SERVICE_UNAVAILABLE);
    }
    let backend = if let Some(be) = self.backends.apps.get(&server_name) {
      be
    } else {
      let default_be = self.backends.default_app.as_ref().unwrap();
      debug!("Serving by default app: {}", default_be);
      self.backends.apps.get(default_be).unwrap()
    };

    // Redirect to https if !tls_enabled and redirect_to_https is true
    if !self.tls_enabled && backend.https_redirection.unwrap_or(false) {
      debug!("Redirect to secure connection: {}", server_name);
      info!("{} => {}", request_log, StatusCode::PERMANENT_REDIRECT);
      return secure_redirection(&server_name, self.globals.https_port, &req);
    }

    // Find reverse proxy for given path and choose one of upstream host
    // TODO: More flexible path matcher
    let path = req.uri().path();
    let upstream = if let Some(upstream) = backend.reverse_proxy.upstream.get(path) {
      upstream
    } else if let Some(default_upstream) = &backend.reverse_proxy.default_upstream {
      default_upstream
    } else {
      return http_error(StatusCode::NOT_FOUND);
    };
    let upstream_scheme_host = if let Some(u) = upstream.get() {
      u
    } else {
      return http_error(StatusCode::INTERNAL_SERVER_ERROR);
    };

    // Upgrade in request header
    let upgrade_in_request = extract_upgrade(req.headers());
    let request_upgraded = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>();

    // Build request from destination information
    let req_forwarded = if let Ok(req) = self.generate_request_forwarded(
      client_addr,
      req,
      upstream_scheme_host,
      &upgrade_in_request,
      upstream,
    ) {
      req
    } else {
      error!("Failed to generate destination uri for reverse proxy");
      return http_error(StatusCode::SERVICE_UNAVAILABLE);
    };
    debug!("Request to be forwarded: {:?}", req_forwarded);

    // Forward request to
    let mut res_backend = match self.forwarder.request(req_forwarded).await {
      Ok(res) => res,
      Err(e) => {
        error!("Failed to get response from backend: {}", e);
        return http_error(StatusCode::BAD_REQUEST);
      }
    };
    #[cfg(feature = "h3")]
    {
      if self.globals.http3 {
        if let Some(port) = self.globals.https_port {
          let alt_svc_value = HeaderValue::from_str(&format!(
            "h3=\":{}\"; ma={}, h3-29=\":{}\"; ma={}",
            port, H3_ALT_SVC_MAX_AGE, port, H3_ALT_SVC_MAX_AGE
          ))
          .unwrap();
          res_backend
            .headers_mut()
            .insert(header::ALT_SVC, alt_svc_value);
        }
      }
    }
    debug!("Response from backend: {:?}", res_backend.status());
    let response_log = res_backend.status().to_string();

    if res_backend.status() == StatusCode::SWITCHING_PROTOCOLS {
      // Handle StatusCode::SWITCHING_PROTOCOLS in response
      let upgrade_in_response = extract_upgrade(res_backend.headers());
      if upgrade_in_request == upgrade_in_response {
        if let Some(request_upgraded) = request_upgraded {
          let mut response_upgraded = res_backend
            .extensions_mut()
            .remove::<hyper::upgrade::OnUpgrade>()
            .ok_or_else(|| anyhow!("Response does not have an upgrade extension"))? // TODO: any response code?
            .await?;
          // TODO: H3で死ぬことがある
          // thread 'rpxy' panicked at 'Failed to upgrade request: hyper::Error(User(ManualUpgrade))', src/proxy/proxy_handler.rs:124:63
          tokio::spawn(async move {
            let mut request_upgraded = request_upgraded.await.map_err(|e| {
              error!("Failed to upgrade request: {}", e);
              anyhow!("Failed to upgrade request: {}", e)
            })?; // TODO: any response code?
            copy_bidirectional(&mut response_upgraded, &mut request_upgraded)
              .await
              .map_err(|e| anyhow!("Coping between upgraded connections failed: {}", e))?; // TODO: any response code?
            Ok(()) as Result<()>
          });
          info!("{} => {}", request_log, response_log);
          Ok(res_backend)
        } else {
          error!("Request does not have an upgrade extension");
          info!("{} => {}", request_log, StatusCode::BAD_REQUEST);
          http_error(StatusCode::BAD_REQUEST)
        }
      } else {
        error!(
          "Backend tried to switch to protocol {:?} when {:?} was requested",
          upgrade_in_response, upgrade_in_request
        );
        info!("{} => {}", request_log, StatusCode::SERVICE_UNAVAILABLE);
        http_error(StatusCode::SERVICE_UNAVAILABLE)
      }
    } else {
      // Generate response to client
      if self.generate_response_forwarded(&mut res_backend).is_ok() {
        info!("{} => {}", request_log, response_log);
        Ok(res_backend)
      } else {
        info!("{} => {}", request_log, StatusCode::BAD_GATEWAY);
        http_error(StatusCode::BAD_GATEWAY)
      }
    }
  }

  ////////////////////////////////////////////////////
  // Functions to generate messages

  fn generate_response_forwarded<B: core::fmt::Debug>(
    &self,
    response: &mut Response<B>,
  ) -> Result<()> {
    let headers = response.headers_mut();
    remove_hop_header(headers);
    remove_connection_header(headers);
    append_header_entry(
      headers,
      "server",
      &format!("{}/{}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
    )?;

    Ok(())
  }

  fn generate_request_forwarded<B: core::fmt::Debug>(
    &self,
    client_addr: SocketAddr,
    mut req: Request<B>,
    upstream_scheme_host: &Uri,
    upgrade: &Option<String>,
    upstream: &Upstream,
  ) -> Result<Request<B>> {
    debug!("Generate request to be forwarded");

    // Add te: trailer if contained in original request
    let te_trailers = {
      if let Some(te) = req.headers().get(header::TE) {
        te.to_str()?.split(',').any(|x| x.trim() == "trailers")
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
    add_forwarding_header(headers, client_addr, self.tls_enabled)?;
    // println!("{:?}", headers);

    // Add te: trailer if te_trailer
    if te_trailers {
      headers.insert(header::TE, "trailer".parse()?);
    }

    // add "host" header of original server_name if not exist (default)
    if req.headers().get(header::HOST).is_none() {
      let org_host = req.uri().host().unwrap_or("none").to_owned();
      req
        .headers_mut()
        .insert(header::HOST, HeaderValue::from_str(org_host.as_str())?);
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
      req.headers_mut().insert("upgrade", v.parse()?);
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
