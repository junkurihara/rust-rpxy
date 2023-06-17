use crate::{backend::Upstream, error::*};
use rustc_hash::FxHashMap as HashMap;
use serde::Deserialize;
use std::fs;

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
impl UpstreamParams {
  pub fn to_upstream(&self) -> Result<Upstream> {
    let mut scheme = "http";
    if let Some(t) = self.tls {
      if t {
        scheme = "https";
      }
    }
    let location = format!("{}://{}", scheme, self.location);
    Ok(Upstream {
      uri: location.parse::<hyper::Uri>().map_err(|e| anyhow!("{}", e))?,
    })
  }
}

impl ConfigToml {
  pub fn new(config_file: &str) -> std::result::Result<Self, anyhow::Error> {
    let config_str = fs::read_to_string(config_file).context("Failed to read config file")?;

    toml::from_str(&config_str).context("Failed to parse toml config")
  }
}
