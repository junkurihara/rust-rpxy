// Highly motivated by https://github.com/felipenoris/hyper-reverse-proxy
use super::Proxy;
use crate::{error::*, log::*};
use hyper::{
  client::connect::Connect,
  header::{HeaderMap, HeaderValue},
  Body, Request, Response, StatusCode, Uri,
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
    debug!("Handling request: {:?}", req);
    // Here we start to handle with hostname
    // Find backend application for given hostname
    let (hostname, _port) = if let Ok(v) = parse_host_port(&req, self.tls_enabled) {
      v
    } else {
      return http_error(StatusCode::SERVICE_UNAVAILABLE);
    };
    let backend = if let Some(be) = self.backends.get(hostname.as_str()) {
      be
    } else {
      return http_error(StatusCode::SERVICE_UNAVAILABLE);
    };

    // Redirect to https if tls_enabled is false and redirect_to_https is true
    let path_and_query = req.uri().path_and_query().unwrap().as_str().to_owned();
    if !self.tls_enabled && backend.https_redirection.unwrap_or(false) {
      debug!("Redirect to secure connection: {}", hostname);
      return secure_redirection(&hostname, self.globals.https_port, &path_and_query);
    }

    // Find reverse proxy for given path
    let path = req.uri().path();
    let destination_scheme_host =
      if let Some(uri) = backend.reverse_proxy.destination_uris.get(path) {
        uri.to_owned()
      } else {
        backend.reverse_proxy.default_destination_uri.clone()
      };

    // Upgrade in request header
    let upgrade_in_request = extract_upgrade(req.headers());
    let request_upgraded = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>();

    // Build request from destination information
    let req_forwarded = if let Ok(req) = generate_request_forwarded(
      client_addr,
      req,
      destination_scheme_host,
      path_and_query,
      &upgrade_in_request,
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
    debug!("Response from backend: {:?}", res_backend.status());

    if res_backend.status() == StatusCode::SWITCHING_PROTOCOLS {
      // Handle StatusCode::SWITCHING_PROTOCOLS in response
      let upgrade_in_response = extract_upgrade(res_backend.headers());
      if upgrade_in_request == upgrade_in_response {
        if let Some(request_upgraded) = request_upgraded {
          let mut response_upgraded = res_backend
            .extensions_mut()
            .remove::<hyper::upgrade::OnUpgrade>()
            .expect("Response does not have an upgrade extension")
            .await?;
          tokio::spawn(async move {
            let mut request_upgraded = request_upgraded.await.expect("Failed to upgrade request");
            copy_bidirectional(&mut response_upgraded, &mut request_upgraded)
              .await
              .expect("Coping between upgraded connections failed");
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
  destination_scheme_host: Uri,
  path_and_query: String,
  upgrade: &Option<String>,
) -> Result<Request<B>> {
  debug!("Generate request to be forwarded");

  // update "host" key in request header
  if req.headers().contains_key("host") {
    // HTTP/1.1
    req.headers_mut().insert(
      "host",
      HeaderValue::from_str(destination_scheme_host.host().unwrap())
        .map_err(|_| anyhow!("Failed to insert destination host into forwarded request"))?,
    );
  }

  // Add te: trailer if contained in original request
  let te_trailer = {
    if let Some(te) = req.headers().get("te") {
      te.to_str()
        .unwrap()
        .split(',')
        .any(|x| x.trim() == "trailer")
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
  if te_trailer {
    headers.insert("te", "trailer".parse().unwrap());
  }

  // update uri in request
  *req.uri_mut() = Uri::builder()
    .scheme(destination_scheme_host.scheme().unwrap().as_str())
    .authority(destination_scheme_host.authority().unwrap().as_str())
    .path_and_query(&path_and_query)
    .build()?;

  // upgrade
  if let Some(v) = upgrade {
    req.headers_mut().insert("upgrade", v.parse().unwrap());
    req
      .headers_mut()
      .insert("connection", HeaderValue::from_str("upgrade")?);
  }

  Ok(req)
}

fn add_forwarding_header(headers: &mut HeaderMap, client_addr: SocketAddr) -> Result<()> {
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
  let _ = HOP_HEADERS.iter().for_each(|key| {
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
  hostname: &str,
  tls_port: Option<u16>,
  path_and_query: &str,
) -> Result<Response<Body>> {
  let dest_uri: String = if let Some(tls_port) = tls_port {
    if tls_port == 443 {
      format!("https://{}{}", hostname, path_and_query)
    } else {
      format!("https://{}:{}{}", hostname, tls_port, path_and_query)
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

fn parse_host_port(req: &Request<Body>, tls_enabled: bool) -> Result<(String, u16)> {
  let hostname_port_headers = req.headers().get("host");
  let hostname_uri = req.uri().host();
  let port_uri = req.uri().port_u16();

  if hostname_port_headers.is_none() && hostname_uri.is_none() {
    bail!("No host in request header");
  }

  let (hostname, port) = match (hostname_uri, hostname_port_headers) {
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
      let hostname = hp_as_uri
        .host()
        .ok_or_else(|| anyhow!("Failed to parse hostname"))?;
      let port = if let Some(p) = hp_as_uri.port() {
        p.as_u16()
      } else if tls_enabled {
        443
      } else {
        80
      };
      (hostname.to_string(), port)
    }
    (None, None) => {
      bail!("Host unspecified in request")
    }
  };

  Ok((hostname, port))
}

// fn get_upgrade_type(headers: &HeaderMap) -> Option<String> {
//   #[allow(clippy::blocks_in_if_conditions)]
//   if headers
//     .get(&*CONNECTION_HEADER)
//     .map(|value| {
//       value
//         .to_str()
//         .unwrap()
//         .split(',')
//         .any(|e| e.trim() == *UPGRADE_HEADER)
//     })
//     .unwrap_or(false)
//   {
//     if let Some(upgrade_value) = headers.get(&*UPGRADE_HEADER) {
//       debug!(
//         "Found upgrade header with value: {}",
//         upgrade_value.to_str().unwrap().to_owned()
//       );

//       return Some(upgrade_value.to_str().unwrap().to_owned());
//     }
//   }

//   None
// }
