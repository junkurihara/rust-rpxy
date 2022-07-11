use crate::error::*;
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

#[derive(Deserialize, Debug, Default)]
pub struct Experimental {
  pub h3: Option<bool>,
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
}

#[derive(Deserialize, Debug, Default)]
pub struct ReverseProxyOption {
  pub path: Option<String>,
  pub upstream: Vec<UpstreamParams>,
  pub upstream_options: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, Default)]
pub struct UpstreamParams {
  pub location: String,
  pub tls: Option<bool>,
}
impl UpstreamParams {
  pub fn to_uri(&self) -> Result<hyper::Uri> {
    let mut scheme = "http";
    if let Some(t) = self.tls {
      if t {
        scheme = "https";
      }
    }
    let location = format!("{}://{}", scheme, self.location);
    location.parse::<hyper::Uri>().map_err(|e| anyhow!("{}", e))
  }
}

impl ConfigToml {
  pub fn new(config_file: &str) -> Result<Self> {
    let config_str = fs::read_to_string(config_file).context("Failed to read config file")?;

    toml::from_str(&config_str).context("Failed to parse toml config")
  }
}
