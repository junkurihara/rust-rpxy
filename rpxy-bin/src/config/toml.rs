use crate::{
  constants::*,
  error::{anyhow, ensure},
  log::warn,
};
use ahash::HashMap;
use rpxy_lib::{AppConfig, AppConfigList, ProxyConfig, ReverseProxyConfig, TlsConfig, UpstreamUri, reexports::Uri};
use serde::Deserialize;
use std::{fs, net::SocketAddr};
use tokio::time::Duration;

#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
/// Main configuration structure parsed from the TOML file.
///
/// # Fields
/// - `listen_port`: Optional TCP port for HTTP.
/// - `listen_port_tls`: Optional TCP port for HTTPS/TLS.
/// - `listen_ipv6`: Enable IPv6 listening.
/// - `https_redirection_port`: Optional port for HTTP to HTTPS redirection.
/// - `tcp_listen_backlog`: Optional TCP backlog size.
/// - `max_concurrent_streams`: Optional max concurrent streams.
/// - `max_clients`: Optional max client connections.
/// - `apps`: Optional application definitions.
/// - `default_app`: Optional default application name.
/// - `experimental`: Optional experimental features.
pub struct ConfigToml {
  pub listen_port: Option<u16>,
  pub listen_port_tls: Option<u16>,
  pub listen_ipv6: Option<bool>,
  pub https_redirection_port: Option<u16>,
  pub tcp_listen_backlog: Option<u32>,
  pub max_concurrent_streams: Option<u32>,
  pub max_clients: Option<u32>,
  pub apps: Option<Apps>,
  pub default_app: Option<String>,
  pub experimental: Option<Experimental>,
}

/// Extension trait for config validation and building
pub trait ConfigTomlExt {
  fn validate_and_build_settings(&self) -> Result<(ProxyConfig, AppConfigList), anyhow::Error>;
}

impl ConfigTomlExt for ConfigToml {
  fn validate_and_build_settings(&self) -> Result<(ProxyConfig, AppConfigList), anyhow::Error> {
    let proxy_config: ProxyConfig = self.try_into()?;
    let apps = self.apps.as_ref().ok_or(anyhow!("Missing application spec"))?;

    // Ensure at least one app is defined
    ensure!(!apps.0.is_empty(), "Wrong application spec.");

    // Helper: all apps have TLS
    let all_apps_have_tls = apps.0.values().all(|app| app.tls.is_some());

    // Helper: all apps have https_redirection unset
    let all_apps_no_https_redirection = apps.0.values().all(|app| {
      if let Some(tls) = app.tls.as_ref() {
        tls.https_redirection.is_none()
      } else {
        true
      }
    });

    if proxy_config.http_port.is_none() {
      ensure!(all_apps_have_tls, "Some apps serve only plaintext HTTP");
    }
    if proxy_config.https_redirection_port.is_some() {
      ensure!(
        proxy_config.https_port.is_some() && proxy_config.http_port.is_some(),
        "https_redirection_port can be specified only when both http_port and https_port are specified"
      );
    }
    if !(proxy_config.https_port.is_some() && proxy_config.http_port.is_some()) {
      ensure!(
        all_apps_no_https_redirection,
        "https_redirection can be specified only when both http_port and https_port are specified"
      );
    }

    // Build AppConfigList
    let mut app_config_list_inner = Vec::<AppConfig>::new();
    for (app_name, app) in apps.0.iter() {
      let _server_name_string = app.server_name.as_ref().ok_or(anyhow!("No server name"))?;
      let registered_app_name = app_name.to_ascii_lowercase();
      let app_config = app.build_app_config(&registered_app_name)?;
      app_config_list_inner.push(app_config);
    }
    let app_config_list = AppConfigList {
      inner: app_config_list_inner,
      default_app: self.default_app.clone().map(|v| v.to_ascii_lowercase()),
    };

    Ok((proxy_config, app_config_list))
  }
}

#[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
/// HTTP/3 protocol options for server configuration.
///
/// # Fields
/// - `alt_svc_max_age`: Optional max age for Alt-Svc header.
/// - `request_max_body_size`: Optional maximum request body size.
/// - `max_concurrent_connections`: Optional maximum concurrent connections.
/// - `max_concurrent_bidistream`: Optional maximum concurrent bidirectional streams.
/// - `max_concurrent_unistream`: Optional maximum concurrent unidirectional streams.
/// - `max_idle_timeout`: Optional maximum idle timeout in milliseconds.
pub struct Http3Option {
  pub alt_svc_max_age: Option<u32>,
  pub request_max_body_size: Option<usize>,
  pub max_concurrent_connections: Option<u32>,
  pub max_concurrent_bidistream: Option<u32>,
  pub max_concurrent_unistream: Option<u32>,
  pub max_idle_timeout: Option<u64>,
}

#[cfg(feature = "cache")]
#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct CacheOption {
  pub cache_dir: Option<String>,
  pub max_cache_entry: Option<usize>,
  pub max_cache_each_size: Option<usize>,
  pub max_cache_each_size_on_memory: Option<usize>,
}

#[cfg(feature = "acme")]
#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct AcmeOption {
  pub dir_url: Option<String>,
  pub email: String,
  pub registry_path: Option<String>,
}

#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct Experimental {
  #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
  pub h3: Option<Http3Option>,

  #[cfg(feature = "cache")]
  pub cache: Option<CacheOption>,

  #[cfg(feature = "acme")]
  pub acme: Option<AcmeOption>,

  pub ignore_sni_consistency: Option<bool>,
  pub connection_handling_timeout: Option<u64>,
}

#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct Apps(pub HashMap<String, Application>);

#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct Application {
  pub server_name: Option<String>,
  pub reverse_proxy: Option<Vec<ReverseProxyOption>>,
  pub tls: Option<TlsOption>,
}

#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct TlsOption {
  pub tls_cert_path: Option<String>,
  pub tls_cert_key_path: Option<String>,
  pub https_redirection: Option<bool>,
  pub client_ca_cert_path: Option<String>,
  #[cfg(feature = "acme")]
  pub acme: Option<bool>,
}

#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct ReverseProxyOption {
  pub path: Option<String>,
  pub replace_path: Option<String>,
  pub upstream: Vec<UpstreamParams>,
  pub upstream_options: Option<Vec<String>>,
  pub load_balance: Option<String>,
}

#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
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
      https_redirection_port: if self.https_redirection_port.is_some() {
        self.https_redirection_port
      } else {
        self.listen_port_tls
      },
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

    // tcp backlog
    if let Some(backlog) = self.tcp_listen_backlog {
      proxy_config.tcp_listen_backlog = backlog;
    }

    // max values
    if let Some(c) = self.max_clients {
      proxy_config.max_clients = c as usize;
    }
    if let Some(c) = self.max_concurrent_streams {
      proxy_config.max_concurrent_streams = c;
    }

    // experimental
    if let Some(exp) = &self.experimental {
      #[cfg(any(feature = "http3-quinn", feature = "http3-s2n"))]
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
            proxy_config.h3_max_concurrent_bidistream = x;
          }
          if let Some(x) = h3option.max_concurrent_unistream {
            proxy_config.h3_max_concurrent_unistream = x;
          }
          if let Some(x) = h3option.max_idle_timeout {
            if x == 0u64 {
              proxy_config.h3_max_idle_timeout = None;
            } else {
              proxy_config.h3_max_idle_timeout = Some(Duration::from_secs(x))
            }
          }
        }
      }

      if let Some(ignore) = exp.ignore_sni_consistency {
        proxy_config.sni_consistency = !ignore;
      }

      if let Some(timeout) = exp.connection_handling_timeout {
        if timeout == 0u64 {
          proxy_config.connection_handling_timeout = None;
        } else {
          proxy_config.connection_handling_timeout = Some(Duration::from_secs(timeout));
        }
      }

      #[cfg(feature = "cache")]
      if let Some(cache_option) = &exp.cache {
        proxy_config.cache_enabled = true;
        proxy_config.cache_dir = match &cache_option.cache_dir {
          Some(cache_dir) => Some(std::path::PathBuf::from(cache_dir)),
          None => Some(std::path::PathBuf::from(CACHE_DIR)),
        };
        if let Some(num) = cache_option.max_cache_entry {
          proxy_config.cache_max_entry = num;
        }
        if let Some(num) = cache_option.max_cache_each_size {
          proxy_config.cache_max_each_size = num;
        }
        if let Some(num) = cache_option.max_cache_each_size_on_memory {
          proxy_config.cache_max_each_size_on_memory = num;
        }
      }
    }

    Ok(proxy_config)
  }
}

impl ConfigToml {
  pub fn new(config_file: &str) -> std::result::Result<Self, anyhow::Error> {
    let config_str = fs::read_to_string(config_file)?;

    // Check unused fields during deserialization
    let t = toml::de::Deserializer::new(&config_str);
    let mut unused = ahash::HashSet::default();

    let res = serde_ignored::deserialize(t, |path| {
      unused.insert(path.to_string());
    })
    .map_err(|e| anyhow!(e));

    if !unused.is_empty() {
      let str = unused.iter().fold(String::new(), |acc, x| acc + x + "\n");
      warn!("Configuration file contains unsupported fields. Check typos:\n{}", str);
    }

    res
  }
}

impl Application {
  pub fn build_app_config(&self, app_name: &str) -> std::result::Result<AppConfig, anyhow::Error> {
    let server_name_string = self.server_name.as_ref().ok_or(anyhow!("Missing server_name"))?;

    // reverse proxy settings
    let reverse_proxy_config: Vec<ReverseProxyConfig> = self.try_into()?;

    // tls settings
    let tls_config = if self.tls.is_some() {
      let tls = self.tls.as_ref().unwrap();

      #[cfg(not(feature = "acme"))]
      ensure!(tls.tls_cert_key_path.is_some() && tls.tls_cert_path.is_some());

      #[cfg(feature = "acme")]
      {
        if tls.acme.unwrap_or(false) {
          ensure!(tls.tls_cert_key_path.is_none() && tls.tls_cert_path.is_none());
        } else {
          ensure!(tls.tls_cert_key_path.is_some() && tls.tls_cert_path.is_some());
        }
      }

      let https_redirection = if tls.https_redirection.is_none() {
        true // Default true
      } else {
        tls.https_redirection.unwrap()
      };

      Some(TlsConfig {
        mutual_tls: tls.client_ca_cert_path.is_some(),
        https_redirection,
        #[cfg(feature = "acme")]
        acme: tls.acme.unwrap_or(false),
      })
    } else {
      None
    };

    Ok(AppConfig {
      app_name: app_name.to_owned(),
      server_name: server_name_string.to_owned(),
      reverse_proxy: reverse_proxy_config,
      tls: tls_config,
    })
  }
}

impl TryInto<Vec<ReverseProxyConfig>> for &Application {
  type Error = anyhow::Error;

  fn try_into(self) -> std::result::Result<Vec<ReverseProxyConfig>, Self::Error> {
    let _server_name_string = self.server_name.as_ref().ok_or(anyhow!("Missing server_name"))?;
    let rp_settings = self.reverse_proxy.as_ref().ok_or(anyhow!("Missing reverse_proxy"))?;

    let mut reverse_proxies: Vec<ReverseProxyConfig> = Vec::new();

    for rpo in rp_settings.iter() {
      let upstream_res: Vec<Option<UpstreamUri>> = rpo.upstream.iter().map(|v| v.try_into().ok()).collect();
      if !upstream_res.iter().all(|v| v.is_some()) {
        return Err(anyhow!("[{}] Upstream uri is invalid", &_server_name_string));
      }
      let upstream = upstream_res.into_iter().map(|v| v.unwrap()).collect();

      reverse_proxies.push(ReverseProxyConfig {
        path: rpo.path.clone(),
        replace_path: rpo.replace_path.clone(),
        upstream,
        upstream_options: rpo.upstream_options.clone(),
        load_balance: rpo.load_balance.clone(),
      })
    }

    Ok(reverse_proxies)
  }
}

impl TryInto<UpstreamUri> for &UpstreamParams {
  type Error = anyhow::Error;

  fn try_into(self) -> std::result::Result<UpstreamUri, Self::Error> {
    let scheme = match self.tls {
      Some(true) => "https",
      _ => "http",
    };
    let location = format!("{}://{}", scheme, self.location);
    Ok(UpstreamUri {
      inner: location.parse::<Uri>().map_err(|e| anyhow!("{}", e))?,
    })
  }
}
