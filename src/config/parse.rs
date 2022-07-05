use super::toml::{ConfigToml, ReverseProxyOption};
use crate::{backend::*, constants::*, error::*, globals::*, log::*};
use clap::Arg;
use parking_lot::Mutex;
use rustc_hash::FxHashMap as HashMap;
use std::net::SocketAddr;

// #[cfg(feature = "tls")]
use std::path::PathBuf;

pub fn parse_opts(globals: &mut Globals, backends: &mut Backends) -> Result<()> {
  let _ = include_str!("../../Cargo.toml");
  let options = clap::command!().arg(
    Arg::new("config_file")
      .long("config")
      .short('c')
      .takes_value(true)
      .help("Configuration file path like \"./config.toml\""),
  );
  let matches = options.get_matches();

  let config = if let Some(config_file_path) = matches.value_of("config_file") {
    ConfigToml::new(config_file_path)?
  } else {
    // Default config Toml
    ConfigToml::default()
  };

  // listen port and scket
  globals.http_port = config.listen_port;
  globals.https_port = config.listen_port_tls;
  ensure!(
    { globals.http_port.is_some() || globals.https_port.is_some() } && {
      if let (Some(p), Some(t)) = (globals.http_port, globals.https_port) {
        p != t
      } else {
        true
      }
    },
    anyhow!("Wrong port spec.")
  );
  let mut listen_addresses: Vec<&str> = Vec::new();
  if let Some(v) = config.listen_only_ipv6 {
    if v {
      listen_addresses.extend(LISTEN_ADDRESSES_V6.iter());
    } else {
      listen_addresses.extend(LISTEN_ADDRESSES_V4.iter());
    }
  } else if let Some(v) = config.listen_ipv6 {
    listen_addresses.extend(LISTEN_ADDRESSES_V4.iter());
    if v {
      listen_addresses.extend(LISTEN_ADDRESSES_V6.iter());
    }
  } else {
    listen_addresses.extend(LISTEN_ADDRESSES_V4.iter());
  }
  globals.listen_sockets = listen_addresses
    .iter()
    .flat_map(|x| {
      let mut v: Vec<SocketAddr> = vec![];
      if let Some(p) = globals.http_port {
        v.push(format!("{}:{}", x, p).parse().unwrap());
      }
      if let Some(p) = globals.https_port {
        v.push(format!("{}:{}", x, p).parse().unwrap());
      }
      v
    })
    .collect();
  if globals.http_port.is_some() {
    info!("Listen port: {}", globals.http_port.unwrap());
  }
  if globals.https_port.is_some() {
    info!("Listen port: {} (for TLS)", globals.https_port.unwrap());
  }

  // max values
  if let Some(c) = config.max_clients {
    globals.max_clients = c as usize;
  }
  if let Some(c) = config.max_concurrent_streams {
    globals.max_concurrent_streams = c;
  }

  // backend apps
  ensure!(config.apps.is_some(), "Missing application spec.");
  let apps = config.apps.unwrap();
  ensure!(!apps.0.is_empty(), "Wrong application spec.");

  // each app
  for (app_name, app) in apps.0.iter() {
    ensure!(app.server_name.is_some(), "Missing server_name");
    let server_name = app.server_name.as_ref().unwrap();

    // TLS settings
    let (tls_cert_path, tls_cert_key_path, https_redirection) = if app.tls.is_none() {
      ensure!(globals.http_port.is_some(), "Required HTTP port");
      (None, None, None)
    } else {
      let tls = app.tls.as_ref().unwrap();
      ensure!(tls.tls_cert_key_path.is_some() && tls.tls_cert_path.is_some());

      (
        tls.tls_cert_path.as_ref().map(PathBuf::from),
        tls.tls_cert_key_path.as_ref().map(PathBuf::from),
        if tls.https_redirection.is_none() {
          Some(true) // Default true
        } else {
          ensure!(globals.https_port.is_some()); // only when both https ports are configured.
          tls.https_redirection
        },
      )
    };
    if globals.http_port.is_none() {
      // if only https_port is specified, tls must be configured
      ensure!(app.tls.is_some())
    }

    // reverse proxy settings
    ensure!(app.reverse_proxy.is_some(), "Missing reverse_proxy");
    let reverse_proxy = get_reverse_proxy(app.reverse_proxy.as_ref().unwrap())?;

    backends.apps.insert(
      server_name.to_owned(),
      Backend {
        app_name: app_name.to_owned(),
        server_name: server_name.to_owned(),
        reverse_proxy,

        tls_cert_path,
        tls_cert_key_path,
        https_redirection,
        server_config: Mutex::new(None),
      },
    );
    info!("Registering application: {} ({})", app_name, server_name);
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
      backends.default_app = Some(d_sn[0].to_owned());
    }
  }

  // experimental
  if let Some(exp) = config.experimental {
    if let Some(b) = exp.h3 {
      globals.http3 = b;
      if b {
        info!("Experimental HTTP/3.0 is enabled. Note it is still very unstable.")
      }
    }
  }

  Ok(())
}

fn get_reverse_proxy(rp_settings: &[ReverseProxyOption]) -> Result<ReverseProxy> {
  let mut upstream: HashMap<String, Upstream> = HashMap::default();
  let mut default_upstream: Option<Upstream> = None;
  rp_settings.iter().for_each(|rpo| {
    let elem = Upstream {
      uri: rpo.upstream.iter().map(|x| x.to_uri().unwrap()).collect(),
      cnt: Default::default(),
      lb: Default::default(),
    };
    if rpo.path.is_some() {
      upstream.insert(rpo.path.as_ref().unwrap().to_owned(), elem);
    } else {
      default_upstream = Some(elem)
    }
  });
  ensure!(
    rp_settings.iter().filter(|rpo| rpo.path.is_none()).count() < 2,
    "Multiple default reverse proxy setting"
  );
  Ok(ReverseProxy {
    default_upstream,
    upstream,
  })
}
