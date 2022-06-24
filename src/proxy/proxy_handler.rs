use super::Proxy;
use crate::{error::*, log::*};
use hyper::{
  client::connect::Connect,
  header::{HeaderMap, HeaderName, HeaderValue},
  Body, Request, Response, StatusCode, Uri,
};
use std::net::SocketAddr;

// pub static HEADERS: phf::Map<&'static str, HeaderName> = phf_map! {
//   "CONNECTION" => HeaderName::from_static("connection"),
//   "ws" => "wss",
// };

impl<T> Proxy<T>
where
  T: Connect + Clone + Sync + Send + 'static,
{
  // TODO: ここでbackendの名前単位でリクエストを分岐させる
  pub async fn handle_request(
    self,
    req: Request<Body>,
    client_ip: SocketAddr, // アクセス制御用
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
    let destination_host_uri = if let Some(uri) = backend.reverse_proxy.destination_uris.get(path) {
      uri.to_owned()
    } else {
      backend.reverse_proxy.default_destination_uri.clone()
    };

    // TODO: Upgrade
    // TODO: X-Forwarded-For
    // TODO: Transfer Encoding

    // Build request from destination information
    let req_forwarded = if let Ok(req) =
      generate_request_forwarded(client_ip, req, destination_host_uri, path_and_query)
    {
      req
    } else {
      error!("Failed to generate destination uri for reverse proxy");
      return http_error(StatusCode::SERVICE_UNAVAILABLE);
    };
    debug!("Request to be forwarded: {:?}", req_forwarded);

    // // Forward request to
    // let res_backend = match self.forwarder.request(req_forwarded).await {
    //   Ok(res) => res,
    //   Err(e) => {
    //     error!("Failed to get response from backend: {}", e);
    //     return http_error(StatusCode::BAD_REQUEST);
    //   }
    // };
    // debug!("Response from backend: {:?}", res_backend.status());
    // Ok(res_backend)

    http_error(StatusCode::NOT_FOUND)
  }
}

// Motivated by https://github.com/felipenoris/hyper-reverse-proxy
fn generate_request_forwarded<B: core::fmt::Debug>(
  client_ip: SocketAddr,
  mut req: Request<B>,
  destination_host_uri: Uri,
  path_and_query: String,
) -> Result<Request<B>> {
  debug!("Generate request to be forwarded");

  // update "host" key in request header
  if req.headers().contains_key("host") {
    // HTTP/1.1
    req.headers_mut().insert(
      "host",
      HeaderValue::from_str(destination_host_uri.host().unwrap())
        .map_err(|_| anyhow!("Failed to insert destination host into forwarded request"))?,
    );
  }

  // update uri in request
  *req.uri_mut() = Uri::builder()
    .scheme(destination_host_uri.scheme().unwrap().as_str())
    .authority(destination_host_uri.authority().unwrap().as_str())
    .path_and_query(&path_and_query)
    .build()?;

  Ok(req)
}

fn http_error(status_code: StatusCode) -> Result<Response<Body>> {
  let response = Response::builder()
    .status(status_code)
    .body(Body::empty())
    .unwrap();
  Ok(response)
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
