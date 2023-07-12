use crate::{
  backend::{Backend, BackendBuilder, ReverseProxy, Upstream, UpstreamGroup, UpstreamGroupBuilder, UpstreamOption},
  certs::CryptoSource,
  constants::*,
  error::*,
  globals::ProxyConfig,
  utils::PathNameBytesExp,
};
use rustc_hash::FxHashMap as HashMap;
use serde::Deserialize;
use std::{fs, net::SocketAddr};

#[derive(Deserialize, Debug, Default)]
pub struct ConfigToml {
  pub listen_port: Option<u16>,
  pub listen_port_tls: Option<u16>,
  pub listen_ipv6: Option<bool>,
  pub max_concurrent_streams: Option<u32>,
  pub max_clients: Option<u32>,
  pub apps: Option<Apps>,
  pub default_app: Option<String>,
  pub experimental: Option<Experimental>,
}

#[cfg(feature = "http3")]
#[derive(Deserialize, Debug, Default)]
pub struct Http3Option {
  pub alt_svc_max_age: Option<u32>,
  pub request_max_body_size: Option<usize>,
  pub max_concurrent_connections: Option<u32>,
  pub max_concurrent_bidistream: Option<u32>,
  pub max_concurrent_unistream: Option<u32>,
  pub max_idle_timeout: Option<u64>,
}

#[derive(Deserialize, Debug, Default)]
pub struct Experimental {
  #[cfg(feature = "http3")]
  pub h3: Option<Http3Option>,
  pub ignore_sni_consistency: Option<bool>,
}

#[derive(Deserialize, Debug, Default)]
pub struct Apps(pub HashMap<String, Application>);

#[derive(Deserialize, Debug, Default)]
pub struct Application {
  pub server_name: Option<String>,
  pub reverse_proxy: Option<Vec<ReverseProxyOption>>,
  pub tls: Option<TlsOption>,
}

#[derive(Deserialize, Debug, Default)]
pub struct TlsOption {
  pub tls_cert_path: Option<String>,
  pub tls_cert_key_path: Option<String>,
  pub https_redirection: Option<bool>,
  pub client_ca_cert_path: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
pub struct ReverseProxyOption {
  pub path: Option<String>,
  pub replace_path: Option<String>,
  pub upstream: Vec<UpstreamParams>,
  pub upstream_options: Option<Vec<String>>,
  pub load_balance: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
pub struct UpstreamParams {
  pub location: String,
  pub tls: Option<bool>,
}

impl TryInto<ProxyConfig> for &ConfigToml {
  type Error = anyhow::Error;

  fn try_into(self) -> std::result::Result<ProxyConfig, Self::Error> {
    let mut proxy_config = ProxyConfig {
      // listen port and socket
      http_port: self.listen_port,
      https_port: self.listen_port_tls,
      ..Default::default()
    };
    ensure!(
      proxy_config.http_port.is_some() || proxy_config.https_port.is_some(),
      anyhow!("Either/Both of http_port or https_port must be specified")
    );
    if proxy_config.http_port.is_some() && proxy_config.https_port.is_some() {
      ensure!(
        proxy_config.http_port.unwrap() != proxy_config.https_port.unwrap(),
        anyhow!("http_port and https_port must be different")
      );
    }

    // NOTE: when [::]:xx is bound, both v4 and v6 listeners are enabled.
    let listen_addresses: Vec<&str> = if let Some(true) = self.listen_ipv6 {
      LISTEN_ADDRESSES_V6.to_vec()
    } else {
      LISTEN_ADDRESSES_V4.to_vec()
    };
    proxy_config.listen_sockets = listen_addresses
      .iter()
      .flat_map(|addr| {
        let mut v: Vec<SocketAddr> = vec![];
        if let Some(port) = proxy_config.http_port {
          v.push(format!("{addr}:{port}").parse().unwrap());
        }
        if let Some(port) = proxy_config.https_port {
          v.push(format!("{addr}:{port}").parse().unwrap());
        }
        v
      })
      .collect();

    // max values
    if let Some(c) = self.max_clients {
      proxy_config.max_clients = c as usize;
    }
    if let Some(c) = self.max_concurrent_streams {
      proxy_config.max_concurrent_streams = c;
    }

    // experimental
    if let Some(exp) = &self.experimental {
      #[cfg(feature = "http3")]
      {
        if let Some(h3option) = &exp.h3 {
          proxy_config.http3 = true;
          if let Some(x) = h3option.alt_svc_max_age {
            proxy_config.h3_alt_svc_max_age = x;
          }
          if let Some(x) = h3option.request_max_body_size {
            proxy_config.h3_request_max_body_size = x;
          }
          if let Some(x) = h3option.max_concurrent_connections {
            proxy_config.h3_max_concurrent_connections = x;
          }
          if let Some(x) = h3option.max_concurrent_bidistream {
            proxy_config.h3_max_concurrent_bidistream = x.into();
          }
          if let Some(x) = h3option.max_concurrent_unistream {
            proxy_config.h3_max_concurrent_unistream = x.into();
          }
          if let Some(x) = h3option.max_idle_timeout {
            if x == 0u64 {
              proxy_config.h3_max_idle_timeout = None;
            } else {
              proxy_config.h3_max_idle_timeout =
                Some(quinn::IdleTimeout::try_from(tokio::time::Duration::from_secs(x)).unwrap())
            }
          }
        }
      }

      if let Some(ignore) = exp.ignore_sni_consistency {
        proxy_config.sni_consistency = !ignore;
      }
    }

    Ok(proxy_config)
  }
}

impl ConfigToml {
  pub fn new(config_file: &str) -> std::result::Result<Self, RpxyError> {
    let config_str = fs::read_to_string(config_file).map_err(RpxyError::Io)?;

    toml::from_str(&config_str).map_err(RpxyError::TomlDe)
  }
}

impl<T> TryInto<Backend<T>> for &Application
where
  T: CryptoSource + Clone,
{
  type Error = anyhow::Error;

  fn try_into(self) -> std::result::Result<Backend<T>, Self::Error> {
    let server_name_string = self.server_name.as_ref().ok_or(anyhow!("Missing server_name"))?;

    // backend builder
    let mut backend_builder = BackendBuilder::default();
    // reverse proxy settings
    let reverse_proxy = self.try_into()?;

    backend_builder
      .app_name(server_name_string)
      .server_name(server_name_string)
      .reverse_proxy(reverse_proxy);

    // TLS settings and build backend instance
    let backend = if self.tls.is_none() {
      backend_builder.build()?
    } else {
      let tls = self.tls.as_ref().unwrap();
      ensure!(tls.tls_cert_key_path.is_some() && tls.tls_cert_path.is_some());

      let https_redirection = if tls.https_redirection.is_none() {
        Some(true) // Default true
      } else {
        tls.https_redirection
      };

      backend_builder
        .tls_cert_path(&tls.tls_cert_path)
        .tls_cert_key_path(&tls.tls_cert_key_path)
        .https_redirection(https_redirection)
        .client_ca_cert_path(&tls.client_ca_cert_path)
        .build()?
    };
    Ok(backend)
  }
}

impl TryInto<ReverseProxy> for &Application {
  type Error = anyhow::Error;

  fn try_into(self) -> std::result::Result<ReverseProxy, Self::Error> {
    let server_name_string = self.server_name.as_ref().ok_or(anyhow!("Missing server_name"))?;
    let rp_settings = self.reverse_proxy.as_ref().ok_or(anyhow!("Missing reverse_proxy"))?;

    let mut upstream: HashMap<PathNameBytesExp, UpstreamGroup> = HashMap::default();

    rp_settings.iter().for_each(|rpo| {
      let upstream_vec: Vec<Upstream> = rpo.upstream.iter().map(|x| x.try_into().unwrap()).collect();
      // let upstream_iter = rpo.upstream.iter().map(|x| x.to_upstream().unwrap());
      // let lb_upstream_num = vec_upstream.len();
      let elem = UpstreamGroupBuilder::default()
        .upstream(&upstream_vec)
        .path(&rpo.path)
        .replace_path(&rpo.replace_path)
        .lb(&rpo.load_balance, &upstream_vec, server_name_string, &rpo.path)
        .opts(&rpo.upstream_options)
        .build()
        .unwrap();

      upstream.insert(elem.path.clone(), elem);
    });
    ensure!(
      rp_settings.iter().filter(|rpo| rpo.path.is_none()).count() < 2,
      "Multiple default reverse proxy setting"
    );
    ensure!(
      upstream
        .iter()
        .all(|(_, elem)| !(elem.opts.contains(&UpstreamOption::ConvertHttpsTo11)
          && elem.opts.contains(&UpstreamOption::ConvertHttpsTo2))),
      "either one of force_http11 or force_http2 can be enabled"
    );

    Ok(ReverseProxy { upstream })
  }
}

impl TryInto<Upstream> for &UpstreamParams {
  type Error = RpxyError;

  fn try_into(self) -> std::result::Result<Upstream, Self::Error> {
    let scheme = match self.tls {
      Some(true) => "https",
      _ => "http",
    };
    let location = format!("{}://{}", scheme, self.location);
    Ok(Upstream {
      uri: location.parse::<hyper::Uri>().map_err(|e| anyhow!("{}", e))?,
    })
  }
}
