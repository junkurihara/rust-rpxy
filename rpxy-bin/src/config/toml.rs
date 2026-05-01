use crate::{
  constants::*,
  error::{anyhow, ensure},
  log::warn,
};
use ahash::HashMap;
use rpxy_lib::{
  AppConfig, AppConfigList, ProxyConfig, ReverseProxyConfig, TlsConfig, UpstreamUri,
  reexports::{IpNet, Uri},
};
use rpxy_trusted_proxies::resolve_trusted_proxy_entries;
use serde::Deserialize;
use std::{
  fs,
  net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};
use tokio::time::Duration;

#[cfg(feature = "proxy-protocol")]
use rpxy_lib::TcpRecvProxyProtocolConfig;

#[cfg(feature = "health-check")]
use rpxy_lib::{HealthCheckConfig, HealthCheckType, LOAD_BALANCE_PRIMARY_BACKUP};

/// Helper type that accepts both a single string and an array of strings in TOML.
///
/// This enables backward-compatible multi-value support:
/// ```toml
/// listen_address_v4 = '192.168.1.1'          # single
/// listen_address_v4 = ['192.168.1.1', '10.0.0.1']  # multiple
/// ```
#[derive(Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(untagged)]
pub enum OneOrMany {
  One(String),
  Many(Vec<String>),
}

impl OneOrMany {
  /// Convert into a `Vec<String>` regardless of variant.
  pub fn into_vec(self) -> Vec<String> {
    match self {
      OneOrMany::One(s) => vec![s],
      OneOrMany::Many(v) => v,
    }
  }
}

#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
/// Main configuration structure parsed from the TOML file.
///
/// # Fields
/// - `listen_port`: Optional TCP port for HTTP.
/// - `listen_port_tls`: Optional TCP port for HTTPS/TLS.
/// - `listen_address_v4`: Optional IPv4 address(es) to bind (default: 0.0.0.0). Accepts a single string or an array.
/// - `listen_address_v6`: Optional IPv6 address(es) to bind (default: ::). Accepts a single string or an array.
/// - `listen_ipv6`: Enable IPv6 listening. If listen_address_v6 is not specified, binds to '::' when true, and disables IPv6 when false (default: false).
/// - `https_redirection_port`: Optional port for HTTP to HTTPS redirection.
/// - `tcp_listen_backlog`: Optional TCP backlog size.
/// - `max_concurrent_streams`: Optional max concurrent streams.
/// - `max_clients`: Optional max client connections.
/// - `trusted_forwarded_proxies`: Optional CIDR(s) or built-in alias names whose incoming forwarding headers are trusted.
/// - `apps`: Optional application definitions.
/// - `default_app`: Optional default application name.
/// - `experimental`: Optional experimental features.
pub struct ConfigToml {
  pub listen_port: Option<u16>,
  pub listen_port_tls: Option<u16>,
  pub listen_address_v4: Option<OneOrMany>,
  pub listen_address_v6: Option<OneOrMany>,
  pub listen_ipv6: Option<bool>,
  pub https_redirection_port: Option<u16>,
  pub tcp_listen_backlog: Option<u32>,
  pub max_concurrent_streams: Option<u32>,
  pub max_clients: Option<u32>,
  pub trusted_forwarded_proxies: Option<OneOrMany>,
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
      // When https_redirection_port is specified, at least TLS is enabled globally.
      // This includes a case that plaintext HTTP listener is not enabled.
      ensure!(
        proxy_config.https_port.is_some(),
        "https_redirection_port must be some only when https_port is specified"
      );
    }
    if !(proxy_config.https_port.is_some() && proxy_config.http_port.is_some()) {
      ensure!(
        all_apps_no_https_redirection,
        "https_redirection can be specified only when both http_port and https_port are specified. Just remove https_redirection settings in each app."
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

#[cfg(feature = "proxy-protocol")]
#[derive(Deserialize, Debug, Default, PartialEq, Eq, Clone)]
pub struct TcpRecvProxyProtocolOption {
  pub trusted_proxies: Vec<String>,
  pub timeout: Option<u64>,
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

  #[cfg(feature = "proxy-protocol")]
  pub tcp_recv_proxy_protocol: Option<TcpRecvProxyProtocolOption>,
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
  #[cfg(feature = "health-check")]
  pub health_check: Option<HealthCheckOption>,
}

#[cfg(feature = "health-check")]
/// TOML deserialization: accepts both `true` and `{ type = "http", ... }`
#[derive(Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(untagged)]
pub enum HealthCheckOption {
  /// Simple boolean: `health_check = true` -> TCP with defaults
  Enabled(bool),
  /// Full config table: `[....health_check] type = "http" ...`
  Config(HealthCheckDetailOption),
}

#[cfg(feature = "health-check")]
#[derive(Deserialize, Debug, PartialEq, Eq, Clone)]
pub struct HealthCheckDetailOption {
  #[serde(default, rename = "type")]
  pub check_type: Option<String>,
  pub interval: Option<u64>,
  pub timeout: Option<u64>,
  pub unhealthy_threshold: Option<u32>,
  pub healthy_threshold: Option<u32>,
  // HTTP-specific
  pub path: Option<String>,
  pub expected_status: Option<u16>,
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
    if self.https_redirection_port.is_some() {
      ensure!(
        proxy_config.https_port.is_some() && proxy_config.http_port.is_some(),
        "https_redirection_port can be explicitly specified only when both http_port and https_port are specified"
      );
    }

    let v4_addrs = self.listen_address_v4.clone().map(|o| o.into_vec());
    let v6_addrs = self.listen_address_v6.clone().map(|o| o.into_vec());
    proxy_config.listen_sockets = build_listen_sockets(
      &v4_addrs,
      &v6_addrs,
      self.listen_ipv6.unwrap_or(false),
      proxy_config.http_port,
      proxy_config.https_port,
    )?;

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
    if let Some(entries) = &self.trusted_forwarded_proxies {
      proxy_config.trusted_forwarded_proxies = resolve_trusted_proxy_entries(entries.clone().into_vec())?.cidrs;
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

      #[cfg(feature = "proxy-protocol")]
      if let Some(pp_option) = &exp.tcp_recv_proxy_protocol {
        ensure!(
          !pp_option.trusted_proxies.is_empty(),
          "tcp_recv_proxy_protocol.trusted_proxies must not be empty"
        );
        let trusted_proxies = pp_option
          .trusted_proxies
          .iter()
          .map(|s| {
            s.parse::<IpNet>()
              .map_err(|e| anyhow!("Invalid CIDR in trusted_proxies: {s}: {e}"))
          })
          .collect::<Result<Vec<_>, _>>()?;
        let timeout = match pp_option.timeout {
          None => Duration::from_millis(rpxy_lib::proxy_protocol_defaults::TIMEOUT_MSEC),
          Some(0) => Duration::ZERO,
          Some(ms) => Duration::from_millis(ms),
        };
        proxy_config.tcp_recv_proxy_protocol = Some(std::sync::Arc::new(TcpRecvProxyProtocolConfig {
          trusted_proxies,
          timeout,
        }));
      }
    }

    Ok(proxy_config)
  }
}

/// Validate and normalize listen address strings, then combine with ports to build socket addresses.
/// Each field accepts one or more addresses. Accepts both bracketed (`[::1]`) and bare (`::1`) forms for IPv6.
fn build_listen_sockets(
  listen_addresses_v4: &Option<Vec<String>>,
  listen_addresses_v6: &Option<Vec<String>>,
  listen_ipv6: bool,
  http_port: Option<u16>,
  https_port: Option<u16>,
) -> Result<Vec<SocketAddr>, anyhow::Error> {
  let mut listen_ips: Vec<IpAddr> = Vec::new();

  // IPv4
  if let Some(addrs) = listen_addresses_v4 {
    ensure!(!addrs.is_empty(), "listen_address_v4 must not be an empty array");

    let listen_v4_ips = addrs
      .iter()
      .map(|addr_str| {
        addr_str
          .parse::<Ipv4Addr>()
          .map_err(|e| anyhow!("Invalid listen_address_v4 '{addr_str}': {e}"))
      })
      .collect::<Result<std::collections::HashSet<_>, _>>()?;
    // Reject unspecified (wildcard) address mixed with specific addresses
    if listen_v4_ips.len() > 1 {
      ensure!(
        !listen_v4_ips.iter().any(|ip| ip.is_unspecified()),
        "listen_address_v4 must not contain the wildcard address '0.0.0.0' when multiple addresses are specified"
      );
    }
    listen_ips.extend(listen_v4_ips.into_iter().map(IpAddr::V4));
  } else {
    listen_ips.push(DEFAULT_LISTEN_ADDRESS_V4.parse().unwrap());
  }

  // IPv6
  if let Some(addrs) = listen_addresses_v6 {
    ensure!(!addrs.is_empty(), "listen_address_v6 must not be an empty array");
    let listen_v6_ips = addrs
      .iter()
      .map(|addr_str| {
        // Strip surrounding brackets if present (e.g. "[::1]" -> "::1") for user convenience
        let stripped = addr_str
          .strip_prefix('[')
          .and_then(|s| s.strip_suffix(']'))
          .unwrap_or(addr_str);
        stripped
          .parse::<Ipv6Addr>()
          .map_err(|e| anyhow!("Invalid listen_address_v6 '{addr_str}': {e}"))
      })
      .collect::<Result<std::collections::HashSet<_>, _>>()?;

    // Reject unspecified (wildcard) address mixed with specific addresses
    if listen_v6_ips.len() > 1 {
      // let v6_unspecified: Ipv6Addr = "::".parse().unwrap();
      ensure!(
        !listen_v6_ips.iter().any(|ip| ip.is_unspecified()),
        "listen_address_v6 must not contain the wildcard address '::' when multiple addresses are specified"
      );
    }
    listen_ips.extend(listen_v6_ips.into_iter().map(IpAddr::V6));
  } else if listen_ipv6 {
    listen_ips.push(DEFAULT_LISTEN_ADDRESS_V6.parse().unwrap());
  }

  // Combine each IP with the configured ports
  let sockets = listen_ips
    .iter()
    .flat_map(|ip| {
      let mut v: Vec<SocketAddr> = vec![];
      if let Some(port) = http_port {
        v.push(SocketAddr::new(*ip, port));
      }
      if let Some(port) = https_port {
        v.push(SocketAddr::new(*ip, port));
      }
      v
    })
    .collect();

  Ok(sockets)
}

impl ConfigToml {
  pub fn new(config_path: &std::path::PathBuf) -> std::result::Result<Self, anyhow::Error> {
    let config_str = fs::read_to_string(config_path)?;

    // Check unused fields during deserialization
    let t = toml::Deserializer::parse(&config_str)?;
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

    // validate load balance + health check combinations
    #[cfg(feature = "health-check")]
    reverse_proxy_config
      .iter()
      .try_for_each(|rpc| validate_lb_health_check(server_name_string, rpc.load_balance.as_deref(), &rpc.health_check))?;

    // tls settings
    let tls_config = if let Some(tls) = self.tls.as_ref() {
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

      // Default true
      let https_redirection = tls.https_redirection.unwrap_or(true);

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
      if rpo.upstream.is_empty() {
        return Err(anyhow!("[{}] At least one upstream must be specified", &_server_name_string));
      }
      let upstream_res: Vec<Option<UpstreamUri>> = rpo.upstream.iter().map(|v| v.try_into().ok()).collect();
      if !upstream_res.iter().all(|v| v.is_some()) {
        return Err(anyhow!("[{}] Upstream uri is invalid", &_server_name_string));
      }
      let upstream = upstream_res.into_iter().map(|v| v.unwrap()).collect();

      #[cfg(feature = "health-check")]
      let health_check = rpo
        .health_check
        .as_ref()
        .map(|hc| build_health_check_config(hc, _server_name_string))
        .transpose()?
        .flatten();

      reverse_proxies.push(ReverseProxyConfig {
        path: rpo.path.clone(),
        replace_path: rpo.replace_path.clone(),
        upstream,
        upstream_options: rpo.upstream_options.clone(),
        load_balance: rpo.load_balance.clone(),
        #[cfg(feature = "health-check")]
        health_check,
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

#[cfg(feature = "health-check")]
/// Convert TOML health check option to internal config, with validation
fn build_health_check_config(option: &HealthCheckOption, server_name: &str) -> Result<Option<HealthCheckConfig>, anyhow::Error> {
  use rpxy_lib::health_check_defaults as hc_defaults;

  match option {
    HealthCheckOption::Enabled(false) => Ok(None),
    HealthCheckOption::Enabled(true) => {
      // TCP check with all defaults
      Ok(Some(HealthCheckConfig {
        check_type: HealthCheckType::Tcp,
        interval: Duration::from_secs(hc_defaults::DEFAULT_INTERVAL_SEC),
        timeout: Duration::from_secs(hc_defaults::DEFAULT_TIMEOUT_SEC),
        unhealthy_threshold: hc_defaults::DEFAULT_UNHEALTHY_THRESHOLD,
        healthy_threshold: hc_defaults::DEFAULT_HEALTHY_THRESHOLD,
      }))
    }
    HealthCheckOption::Config(detail) => {
      let check_type_str = detail.check_type.as_deref().unwrap_or("tcp");
      let check_type = match check_type_str {
        "tcp" => HealthCheckType::Tcp,
        "http" => {
          let path = detail
            .path
            .as_ref()
            .ok_or_else(|| anyhow!("[{server_name}] health_check.path is required when type = \"http\""))?;
          ensure!(
            path.starts_with('/'),
            "[{server_name}] health_check.path must start with \"/\" (got \"{path}\")",
          );
          let expected_status = detail.expected_status.unwrap_or(hc_defaults::DEFAULT_EXPECTED_STATUS);
          HealthCheckType::Http {
            path: path.clone(),
            expected_status,
          }
        }
        other => {
          return Err(anyhow!("[{server_name}] Unknown health_check type: \"{other}\""));
        }
      };

      let interval = Duration::from_secs(detail.interval.unwrap_or(hc_defaults::DEFAULT_INTERVAL_SEC));
      let timeout = Duration::from_secs(detail.timeout.unwrap_or(hc_defaults::DEFAULT_TIMEOUT_SEC));

      ensure!(
        timeout < interval,
        "[{server_name}] health_check.timeout ({timeout:?}) must be less than interval ({interval:?})",
      );

      let unhealthy_threshold = detail.unhealthy_threshold.unwrap_or(hc_defaults::DEFAULT_UNHEALTHY_THRESHOLD);
      let healthy_threshold = detail.healthy_threshold.unwrap_or(hc_defaults::DEFAULT_HEALTHY_THRESHOLD);

      ensure!(
        unhealthy_threshold >= 1,
        "[{server_name}] health_check.unhealthy_threshold must be >= 1",
      );
      ensure!(
        healthy_threshold >= 1,
        "[{server_name}] health_check.healthy_threshold must be >= 1",
      );

      Ok(Some(HealthCheckConfig {
        check_type,
        interval,
        timeout,
        unhealthy_threshold,
        healthy_threshold,
      }))
    }
  }
}

#[cfg(feature = "health-check")]
/// Validate load balance + health check combinations
/// Currently only "primary_backup" requires health check, and other load balance strategies don't have specific requirements for health checks.
fn validate_lb_health_check(
  server_name: &str,
  load_balance: Option<&str>,
  health_check: &Option<HealthCheckConfig>,
) -> Result<(), anyhow::Error> {
  if load_balance == Some(LOAD_BALANCE_PRIMARY_BACKUP) && health_check.is_none() {
    return Err(anyhow!(
      "[{server_name}] load_balance = \"primary_backup\" requires health_check to be enabled",
    ));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[cfg(feature = "health-check")]
  fn http_health_check_option(
    path: Option<&str>,
    interval: Option<u64>,
    timeout: Option<u64>,
    unhealthy_threshold: Option<u32>,
    healthy_threshold: Option<u32>,
  ) -> HealthCheckOption {
    HealthCheckOption::Config(HealthCheckDetailOption {
      check_type: Some("http".to_string()),
      interval,
      timeout,
      unhealthy_threshold,
      healthy_threshold,
      path: path.map(str::to_string),
      expected_status: None,
    })
  }

  #[test]
  fn one_or_many_deserialize_single_string() {
    let toml_str = r#"val = '192.168.1.1'"#;
    #[derive(Deserialize)]
    struct T {
      val: OneOrMany,
    }
    let t: T = toml::from_str(toml_str).unwrap();
    assert_eq!(t.val.into_vec(), vec!["192.168.1.1".to_string()]);
  }

  #[test]
  fn one_or_many_deserialize_array() {
    let toml_str = r#"val = ['192.168.1.1', '10.0.0.1']"#;
    #[derive(Deserialize)]
    struct T {
      val: OneOrMany,
    }
    let t: T = toml::from_str(toml_str).unwrap();
    assert_eq!(t.val.into_vec(), vec!["192.168.1.1".to_string(), "10.0.0.1".to_string()]);
  }

  #[test]
  fn build_sockets_single_v4() {
    let sockets = build_listen_sockets(&Some(vec!["127.0.0.1".into()]), &None, false, Some(8080), None).unwrap();
    assert_eq!(sockets, vec!["127.0.0.1:8080".parse::<SocketAddr>().unwrap()]);
  }

  #[test]
  fn build_sockets_multiple_v4() {
    let addrs = Some(vec!["192.168.1.1".into(), "10.0.0.1".into()]);
    let sockets = build_listen_sockets(&addrs, &None, false, Some(80), Some(443)).unwrap();
    assert_eq!(sockets.len(), 4);
    assert!(sockets.contains(&"192.168.1.1:80".parse::<SocketAddr>().unwrap()));
    assert!(sockets.contains(&"192.168.1.1:443".parse::<SocketAddr>().unwrap()));
    assert!(sockets.contains(&"10.0.0.1:80".parse::<SocketAddr>().unwrap()));
    assert!(sockets.contains(&"10.0.0.1:443".parse::<SocketAddr>().unwrap()));
  }

  #[test]
  fn build_sockets_default_v4_when_none() {
    let sockets = build_listen_sockets(&None, &None, false, Some(8080), None).unwrap();
    assert_eq!(sockets, vec!["0.0.0.0:8080".parse::<SocketAddr>().unwrap()]);
  }

  #[test]
  fn build_sockets_v6_with_brackets() {
    let sockets = build_listen_sockets(&None, &Some(vec!["[::1]".into()]), false, Some(8080), None).unwrap();
    assert!(sockets.contains(&"[::1]:8080".parse::<SocketAddr>().unwrap()));
  }

  #[test]
  fn build_sockets_multiple_v6() {
    let v6 = Some(vec!["::1".into(), "fe80::1".into()]);
    let sockets = build_listen_sockets(&None, &v6, false, Some(80), None).unwrap();
    assert_eq!(sockets.len(), 3); // default v4 + 2 v6
    assert!(sockets.contains(&"[::1]:80".parse::<SocketAddr>().unwrap()));
    assert!(sockets.contains(&"[fe80::1]:80".parse::<SocketAddr>().unwrap()));
  }

  #[test]
  fn build_sockets_duplicate_v4_deduplicated() {
    let addrs = Some(vec!["10.0.0.1".into(), "10.0.0.1".into()]);
    let sockets = build_listen_sockets(&addrs, &None, false, Some(80), None).unwrap();
    assert_eq!(sockets.len(), 1);
  }

  #[test]
  fn build_sockets_empty_array_rejected() {
    let result = build_listen_sockets(&Some(vec![]), &None, false, Some(80), None);
    assert!(result.is_err());
  }

  #[test]
  fn build_sockets_ipv6_in_v4_field_rejected() {
    let result = build_listen_sockets(&Some(vec!["::1".into()]), &None, false, Some(80), None);
    assert!(result.is_err());
  }

  #[test]
  fn build_sockets_ipv4_in_v6_field_rejected() {
    let result = build_listen_sockets(&None, &Some(vec!["127.0.0.1".into()]), false, Some(80), None);
    assert!(result.is_err());
  }

  #[test]
  fn build_sockets_wildcard_v4_with_multiple_rejected() {
    let addrs = Some(vec!["0.0.0.0".into(), "192.168.1.1".into()]);
    let result = build_listen_sockets(&addrs, &None, false, Some(80), None);
    assert!(result.is_err());
  }

  #[test]
  fn build_sockets_wildcard_v6_with_multiple_rejected() {
    let result = build_listen_sockets(&None, &Some(vec!["::".into(), "::1".into()]), false, Some(80), None);
    assert!(result.is_err());
  }

  #[test]
  fn build_sockets_wildcard_v4_single_allowed() {
    let sockets = build_listen_sockets(&Some(vec!["0.0.0.0".into()]), &None, false, Some(80), None).unwrap();
    assert_eq!(sockets.len(), 1);
  }

  #[test]
  fn build_sockets_wildcard_v6_single_allowed() {
    let sockets = build_listen_sockets(&None, &Some(vec!["::".into()]), false, Some(80), None).unwrap();
    assert_eq!(sockets.len(), 2); // default v4 + single v6
  }

  #[test]
  fn config_toml_single_address_backward_compat() {
    let toml_str = r#"
      listen_port = 8080
      listen_address_v4 = '127.0.0.1'
    "#;
    let config: ConfigToml = toml::from_str(toml_str).unwrap();
    assert_eq!(config.listen_address_v4.unwrap().into_vec(), vec!["127.0.0.1".to_string()]);
  }

  #[test]
  fn config_toml_multiple_addresses() {
    let toml_str = r#"
      listen_port = 8080
      listen_address_v4 = ['192.168.1.1', '10.0.0.1']
      listen_address_v6 = ['::1', 'fe80::1']
    "#;
    let config: ConfigToml = toml::from_str(toml_str).unwrap();
    assert_eq!(
      config.listen_address_v4.unwrap().into_vec(),
      vec!["192.168.1.1".to_string(), "10.0.0.1".to_string()]
    );
    assert_eq!(
      config.listen_address_v6.unwrap().into_vec(),
      vec!["::1".to_string(), "fe80::1".to_string()]
    );
  }

  #[test]
  fn trusted_forwarded_proxies_default_to_empty() {
    let toml_str = r#"
      listen_port = 8080
    "#;
    let config: ConfigToml = toml::from_str(toml_str).unwrap();
    let proxy_config: ProxyConfig = (&config).try_into().unwrap();
    assert!(proxy_config.trusted_forwarded_proxies.is_empty());
  }

  #[test]
  fn trusted_forwarded_proxies_accept_single_and_many() {
    let single = r#"
      listen_port = 8080
      trusted_forwarded_proxies = "10.0.0.0/8"
    "#;
    let config: ConfigToml = toml::from_str(single).unwrap();
    let proxy_config: ProxyConfig = (&config).try_into().unwrap();
    assert_eq!(proxy_config.trusted_forwarded_proxies.len(), 1);
    assert_eq!(
      proxy_config.trusted_forwarded_proxies[0],
      "10.0.0.0/8".parse::<IpNet>().unwrap()
    );

    let many = r#"
      listen_port = 8080
      trusted_forwarded_proxies = ["10.0.0.0/8", "192.168.0.0/16"]
    "#;
    let config: ConfigToml = toml::from_str(many).unwrap();
    let proxy_config: ProxyConfig = (&config).try_into().unwrap();
    assert_eq!(proxy_config.trusted_forwarded_proxies.len(), 2);
    assert_eq!(
      proxy_config.trusted_forwarded_proxies[1],
      "192.168.0.0/16".parse::<IpNet>().unwrap()
    );
  }

  #[test]
  fn trusted_forwarded_proxies_accept_builtin_aliases() {
    let alias = r#"
      listen_port = 8080
      trusted_forwarded_proxies = "cloudflare"
    "#;
    let config: ConfigToml = toml::from_str(alias).unwrap();
    let proxy_config: ProxyConfig = (&config).try_into().unwrap();
    assert!(
      proxy_config
        .trusted_forwarded_proxies
        .contains(&"173.245.48.0/20".parse::<IpNet>().unwrap())
    );
    assert!(
      proxy_config
        .trusted_forwarded_proxies
        .contains(&"2400:cb00::/32".parse::<IpNet>().unwrap())
    );
  }

  #[test]
  fn trusted_forwarded_proxies_accept_cloudfront_alias() {
    let alias = r#"
      listen_port = 8080
      trusted_forwarded_proxies = "cloudfront"
    "#;
    let config: ConfigToml = toml::from_str(alias).unwrap();
    let proxy_config: ProxyConfig = (&config).try_into().unwrap();
    assert!(
      proxy_config
        .trusted_forwarded_proxies
        .contains(&"120.52.22.96/27".parse::<IpNet>().unwrap())
    );
    assert!(
      proxy_config
        .trusted_forwarded_proxies
        .contains(&"13.35.0.0/16".parse::<IpNet>().unwrap())
    );
  }

  #[test]
  fn trusted_forwarded_proxies_accept_mixed_alias_and_cidr() {
    let alias = r#"
      listen_port = 8080
      trusted_forwarded_proxies = ["fastly", "10.0.0.0/8"]
    "#;
    let config: ConfigToml = toml::from_str(alias).unwrap();
    let proxy_config: ProxyConfig = (&config).try_into().unwrap();
    assert!(
      proxy_config
        .trusted_forwarded_proxies
        .contains(&"23.235.32.0/20".parse::<IpNet>().unwrap())
    );
    assert!(
      proxy_config
        .trusted_forwarded_proxies
        .contains(&"2a04:4e40::/32".parse::<IpNet>().unwrap())
    );
    assert!(
      proxy_config
        .trusted_forwarded_proxies
        .contains(&"10.0.0.0/8".parse::<IpNet>().unwrap())
    );
  }

  #[cfg(feature = "health-check")]
  #[test]
  fn build_health_check_config_enabled_true_uses_tcp_defaults() {
    let config = build_health_check_config(&HealthCheckOption::Enabled(true), "example.com").unwrap();
    let config = config.expect("health check config must exist");

    assert_eq!(config.check_type, HealthCheckType::Tcp);
    assert_eq!(
      config.interval,
      Duration::from_secs(rpxy_lib::health_check_defaults::DEFAULT_INTERVAL_SEC)
    );
    assert_eq!(
      config.timeout,
      Duration::from_secs(rpxy_lib::health_check_defaults::DEFAULT_TIMEOUT_SEC)
    );
    assert_eq!(
      config.unhealthy_threshold,
      rpxy_lib::health_check_defaults::DEFAULT_UNHEALTHY_THRESHOLD
    );
    assert_eq!(
      config.healthy_threshold,
      rpxy_lib::health_check_defaults::DEFAULT_HEALTHY_THRESHOLD
    );
  }

  #[cfg(feature = "health-check")]
  #[test]
  fn build_health_check_config_http_requires_path() {
    let err = build_health_check_config(
      &http_health_check_option(None, Some(10), Some(5), Some(2), Some(2)),
      "example.com",
    )
    .unwrap_err();

    assert!(err.to_string().contains("health_check.path is required"));
  }

  #[cfg(feature = "health-check")]
  #[test]
  fn build_health_check_config_http_path_must_start_with_slash() {
    let err = build_health_check_config(
      &http_health_check_option(Some("health"), Some(10), Some(5), Some(2), Some(2)),
      "example.com",
    )
    .unwrap_err();

    assert!(err.to_string().contains("health_check.path must start with"));
  }

  #[cfg(feature = "health-check")]
  #[test]
  fn build_health_check_config_timeout_must_be_less_than_interval() {
    let err = build_health_check_config(
      &http_health_check_option(Some("/health"), Some(5), Some(5), Some(2), Some(2)),
      "example.com",
    )
    .unwrap_err();

    assert!(err.to_string().contains("health_check.timeout"));
  }

  #[cfg(feature = "health-check")]
  #[test]
  fn build_health_check_config_thresholds_must_be_positive() {
    let err = build_health_check_config(
      &http_health_check_option(Some("/health"), Some(10), Some(5), Some(0), Some(1)),
      "example.com",
    )
    .unwrap_err();
    assert!(err.to_string().contains("unhealthy_threshold"));

    let err = build_health_check_config(
      &http_health_check_option(Some("/health"), Some(10), Some(5), Some(1), Some(0)),
      "example.com",
    )
    .unwrap_err();
    assert!(err.to_string().contains("healthy_threshold"));
  }

  #[test]
  fn empty_upstream_list_is_rejected() {
    let app = Application {
      server_name: Some("example.com".into()),
      reverse_proxy: Some(vec![ReverseProxyOption {
        path: None,
        replace_path: None,
        upstream: vec![],
        upstream_options: None,
        load_balance: None,
        #[cfg(feature = "health-check")]
        health_check: None,
      }]),
      tls: None,
    };
    let result: Result<Vec<ReverseProxyConfig>, _> = (&app).try_into();
    assert!(result.is_err());
    let err = result.err().unwrap();
    assert!(err.to_string().contains("At least one upstream must be specified"));
  }

  #[cfg(feature = "health-check")]
  #[test]
  fn validate_lb_health_check_primary_backup_requires_health_check() {
    let err = validate_lb_health_check("example.com", Some(LOAD_BALANCE_PRIMARY_BACKUP), &None).unwrap_err();
    assert!(err.to_string().contains("requires health_check to be enabled"));

    let health_check = Some(HealthCheckConfig {
      check_type: HealthCheckType::Tcp,
      interval: Duration::from_secs(10),
      timeout: Duration::from_secs(5),
      unhealthy_threshold: 2,
      healthy_threshold: 2,
    });
    assert!(validate_lb_health_check("example.com", Some(LOAD_BALANCE_PRIMARY_BACKUP), &health_check).is_ok());
  }
}
