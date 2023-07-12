use super::toml::ConfigToml;
use crate::{backend::Backends, certs::CryptoSource, error::*, globals::*, log::*, utils::BytesName};
use clap::Arg;
use tokio::runtime::Handle;

pub fn build_globals<T>(runtime_handle: Handle) -> std::result::Result<Globals<T>, anyhow::Error>
where
  T: CryptoSource + Clone,
{
  let _ = include_str!("../../Cargo.toml");
  let options = clap::command!().arg(
    Arg::new("config_file")
      .long("config")
      .short('c')
      .value_name("FILE")
      .help("Configuration file path like \"./config.toml\""),
  );
  let matches = options.get_matches();

  ///////////////////////////////////
  let config = if let Some(config_file_path) = matches.get_one::<String>("config_file") {
    ConfigToml::new(config_file_path)?
  } else {
    // Default config Toml
    ConfigToml::default()
  };

  ///////////////////////////////////
  // build proxy config
  let proxy_config: ProxyConfig = (&config).try_into()?;
  // For loggings
  if proxy_config.listen_sockets.iter().any(|addr| addr.is_ipv6()) {
    info!("Listen both IPv4 and IPv6")
  } else {
    info!("Listen IPv4")
  }
  if proxy_config.http_port.is_some() {
    info!("Listen port: {}", proxy_config.http_port.unwrap());
  }
  if proxy_config.https_port.is_some() {
    info!("Listen port: {} (for TLS)", proxy_config.https_port.unwrap());
  }
  if proxy_config.http3 {
    info!("Experimental HTTP/3.0 is enabled. Note it is still very unstable.");
  }
  if !proxy_config.sni_consistency {
    info!("Ignore consistency between TLS SNI and Host header (or Request line). Note it violates RFC.");
  }

  ///////////////////////////////////
  // backend_apps
  let apps = config.apps.ok_or(anyhow!("Missing application spec"))?;

  // assertions for all backend apps
  ensure!(!apps.0.is_empty(), "Wrong application spec.");
  // if only https_port is specified, tls must be configured for all apps
  if proxy_config.http_port.is_none() {
    ensure!(
      apps.0.iter().all(|(_, app)| app.tls.is_some()),
      "Some apps serves only plaintext HTTP"
    );
  }
  // https redirection can be configured if both ports are active
  if !(proxy_config.https_port.is_some() && proxy_config.http_port.is_some()) {
    ensure!(
      apps.0.iter().all(|(_, app)| {
        if let Some(tls) = app.tls.as_ref() {
          tls.https_redirection.is_none()
        } else {
          true
        }
      }),
      "https_redirection can be specified only when both http_port and https_port are specified"
    );
  }

  // build backends
  let mut backends = Backends::new();
  for (app_name, app) in apps.0.iter() {
    let server_name_string = app.server_name.as_ref().ok_or(anyhow!("No server name"))?;
    let backend = app.try_into()?;
    backends.apps.insert(server_name_string.to_server_name_vec(), backend);
    info!("Registering application: {} ({})", app_name, server_name_string);
  }

  // default backend application for plaintext http requests
  if let Some(d) = config.default_app {
    let d_sn: Vec<&str> = backends
      .apps
      .iter()
      .filter(|(_k, v)| v.app_name == d)
      .map(|(_, v)| v.server_name.as_ref())
      .collect();
    if !d_sn.is_empty() {
      info!(
        "Serving plaintext http for requests to unconfigured server_name by app {} (server_name: {}).",
        d, d_sn[0]
      );
      backends.default_server_name_bytes = Some(d_sn[0].to_server_name_vec());
    }
  }

  ///////////////////////////////////
  let globals = Globals {
    proxy_config,
    backends,
    request_count: Default::default(),
    runtime_handle,
  };

  Ok(globals)
}
