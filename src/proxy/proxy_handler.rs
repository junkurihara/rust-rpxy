// Highly motivated by https://github.com/felipenoris/hyper-reverse-proxy
use super::{Proxy, Upstream, UpstreamOption};
use crate::{constants::*, error::*, log::*};
use hyper::{
  client::connect::Connect,
  header::{HeaderMap, HeaderValue},
  http::uri::Scheme,
  Body, Request, Response, StatusCode, Uri, Version,
};
use std::net::SocketAddr;
use tokio::io::copy_bidirectional;

const HOP_HEADERS: &[&str] = &[
  "connection",
  "te",
  "trailer",
  "keep-alive",
  "proxy-connection",
  "proxy-authenticate",
  "proxy-authorization",
  "transfer-encoding",
  "upgrade",
];

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  pub async fn handle_request(
    self,
    mut req: Request<Body>,
    client_addr: SocketAddr, // アクセス制御用
  ) -> Result<Response<Body>> {
    info!(
      "Handling {:?} request from {}: {} {:?} {} {:?}",
      req.version(),
      client_addr,
      req.method(),
      req
        .headers()
        .get("host")
        .map_or_else(|| "<none>", |h| h.to_str().unwrap()),
      req.uri(),
      req
        .headers()
        .get("user-agent")
        .map_or_else(|| "<none>", |ua| ua.to_str().unwrap())
    );
    // Here we start to handle with server_name
    // Find backend application for given server_name
    let (server_name, _port) = if let Ok(v) = parse_host_port(&req, self.tls_enabled) {
      v
    } else {
      return http_error(StatusCode::SERVICE_UNAVAILABLE);
    };
    let backend = if let Some(be) = self.backends.apps.get(server_name.as_str()) {
      be
    } else if let Some(default_be) = &self.backends.default_app {
      debug!("Serving by default app: {}", default_be);
      self.backends.apps.get(default_be).unwrap()
    } else {
      return http_error(StatusCode::SERVICE_UNAVAILABLE);
    };

    // Redirect to https if tls_enabled is false and redirect_to_https is true
    let path_and_query = req.uri().path_and_query().unwrap().as_str().to_owned();
    if !self.tls_enabled && backend.https_redirection.unwrap_or(false) {
      debug!("Redirect to secure connection: {}", server_name);
      return secure_redirection(&server_name, self.globals.https_port, &path_and_query);
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
    let req_forwarded = if let Ok(req) = generate_request_forwarded(
      client_addr,
      req,
      upstream_scheme_host,
      path_and_query,
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
          res_backend.headers_mut().insert(
            hyper::header::ALT_SVC,
            format!(
              "h3=\":{}\"; ma={}, h3-29=\":{}\"; ma={}",
              port, H3_ALT_SVC_MAX_AGE, port, H3_ALT_SVC_MAX_AGE
            )
            .parse()
            .unwrap(),
          );
        }
      }
    }
    debug!("Response from backend: {:?}", res_backend.status());

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
          Ok(res_backend)
        } else {
          error!("Request does not have an upgrade extension");
          http_error(StatusCode::BAD_GATEWAY)
        }
      } else {
        error!(
          "Backend tried to switch to protocol {:?} when {:?} was requested",
          upgrade_in_response, upgrade_in_request
        );
        http_error(StatusCode::BAD_GATEWAY)
      }
    } else {
      // Generate response to client
      if generate_response_forwarded(&mut res_backend).is_ok() {
        Ok(res_backend)
      } else {
        http_error(StatusCode::BAD_GATEWAY)
      }
    }
  }
}

fn generate_response_forwarded<B: core::fmt::Debug>(response: &mut Response<B>) -> Result<()> {
  let headers = response.headers_mut();
  remove_hop_header(headers);
  remove_connection_header(headers);
  Ok(())
}

fn generate_request_forwarded<B: core::fmt::Debug>(
  client_addr: SocketAddr,
  mut req: Request<B>,
  upstream_scheme_host: &Uri,
  path_and_query: String,
  upgrade: &Option<String>,
  upstream: &Upstream,
) -> Result<Request<B>> {
  debug!("Generate request to be forwarded");

  // Add te: trailer if contained in original request
  let te_trailers = {
    if let Some(te) = req.headers().get("te") {
      te.to_str()
        .unwrap()
        .split(',')
        .any(|x| x.trim() == "trailers")
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
  add_forwarding_header(headers, client_addr)?;

  // Add te: trailer if te_trailer
  if te_trailers {
    headers.insert("te", "trailer".parse().unwrap());
  }

  // add "host" header of original server_name if not exist (default)
  if req.headers().get(hyper::header::HOST).is_none() {
    let org_host = req.uri().host().unwrap_or("none").to_owned();
    req.headers_mut().insert(
      hyper::header::HOST,
      HeaderValue::from_str(org_host.as_str()).unwrap(),
    );
  };

  // apply upstream-specific headers given in upstream_option
  let headers = req.headers_mut();
  apply_upstream_options_to_header(headers, client_addr, upstream_scheme_host, upstream)?;

  // update uri in request
  *req.uri_mut() = Uri::builder()
    .scheme(upstream_scheme_host.scheme().unwrap().as_str())
    .authority(upstream_scheme_host.authority().unwrap().as_str())
    .path_and_query(&path_and_query)
    .build()?;

  // upgrade
  if let Some(v) = upgrade {
    req.headers_mut().insert("upgrade", v.parse().unwrap());
    req
      .headers_mut()
      .insert(hyper::header::CONNECTION, HeaderValue::from_str("upgrade")?);
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

fn apply_upstream_options_to_header(
  headers: &mut HeaderMap,
  _client_addr: SocketAddr,
  upstream_scheme_host: &Uri,
  upstream: &Upstream,
) -> Result<()> {
  upstream.opts.iter().for_each(|opt| match opt {
    UpstreamOption::OverrideHost => {
      let upstream_host = upstream_scheme_host.host().unwrap();
      headers
        .insert(
          hyper::header::HOST,
          HeaderValue::from_str(upstream_host).unwrap(),
        )
        .unwrap();
    }
  });
  Ok(())
}

fn add_forwarding_header(headers: &mut HeaderMap, client_addr: SocketAddr) -> Result<()> {
  // default process
  // optional process defined by upstream_option is applied in fn apply_upstream_options
  let client_ip = client_addr.ip();
  match headers.entry("x-forwarded-for") {
    hyper::header::Entry::Vacant(entry) => {
      entry.insert(client_ip.to_string().parse()?);
    }
    hyper::header::Entry::Occupied(entry) => {
      let client_ip_str = client_ip.to_string();
      let mut addr = String::with_capacity(entry.get().as_bytes().len() + 2 + client_ip_str.len());

      addr.push_str(std::str::from_utf8(entry.get().as_bytes()).unwrap());
      addr.push(',');
      addr.push(' ');
      addr.push_str(&client_ip_str);
    }
  }
  Ok(())
}

fn remove_connection_header(headers: &mut HeaderMap) {
  if headers.get("connection").is_some() {
    let v = headers.get("connection").cloned().unwrap();
    for m in v.to_str().unwrap().split(',') {
      if !m.is_empty() {
        headers.remove(m.trim());
      }
    }
  }
}

fn remove_hop_header(headers: &mut HeaderMap) {
  HOP_HEADERS.iter().for_each(|key| {
    headers.remove(*key);
  });
}

fn http_error(status_code: StatusCode) -> Result<Response<Body>> {
  let response = Response::builder()
    .status(status_code)
    .body(Body::empty())
    .unwrap();
  Ok(response)
}

fn extract_upgrade(headers: &HeaderMap) -> Option<String> {
  if let Some(c) = headers.get("connection") {
    if c
      .to_str()
      .unwrap_or("")
      .split(',')
      .into_iter()
      .any(|w| w.trim().to_ascii_lowercase() == "upgrade")
    {
      if let Some(u) = headers.get("upgrade") {
        let m = u.to_str().unwrap().to_string();
        debug!("Upgrade in request header: {}", m);
        return Some(m);
      }
    }
  }
  None
}

fn secure_redirection(
  server_name: &str,
  tls_port: Option<u16>,
  path_and_query: &str,
) -> Result<Response<Body>> {
  let dest_uri: String = if let Some(tls_port) = tls_port {
    if tls_port == 443 {
      format!("https://{}{}", server_name, path_and_query)
    } else {
      format!("https://{}:{}{}", server_name, tls_port, path_and_query)
    }
  } else {
    bail!("Internal error! TLS port is not set internally.");
  };
  let response = Response::builder()
    .status(StatusCode::MOVED_PERMANENTLY)
    .header("Location", dest_uri)
    .body(Body::empty())
    .unwrap();
  Ok(response)
}

fn parse_host_port<B: core::fmt::Debug>(
  req: &Request<B>,
  tls_enabled: bool,
) -> Result<(String, u16)> {
  let host_port_headers = req.headers().get("host");
  let host_uri = req.uri().host();
  let port_uri = req.uri().port_u16();

  if host_port_headers.is_none() && host_uri.is_none() {
    bail!("No host in request header");
  }

  let (host, port) = match (host_uri, host_port_headers) {
    (Some(x), _) => {
      let port = if let Some(p) = port_uri {
        p
      } else if tls_enabled {
        443
      } else {
        80
      };
      (x.to_string(), port)
    }
    (None, Some(x)) => {
      let hp_as_uri = x.to_str().unwrap().parse::<Uri>().unwrap();
      let host = hp_as_uri
        .host()
        .ok_or_else(|| anyhow!("Failed to parse host"))?;
      let port = if let Some(p) = hp_as_uri.port() {
        p.as_u16()
      } else if tls_enabled {
        443
      } else {
        80
      };
      (host.to_string(), port)
    }
    (None, None) => {
      bail!("Host unspecified in request")
    }
  };

  Ok((host, port))
}
