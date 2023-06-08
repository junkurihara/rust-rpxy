use super::toml::{ConfigToml, ReverseProxyOption};
use crate::{
  backend::{BackendBuilder, ReverseProxy, Upstream, UpstreamGroup, UpstreamGroupBuilder, UpstreamOption},
  constants::*,
  error::*,
  globals::*,
  log::*,
  utils::{BytesName, PathNameBytesExp},
};
use clap::Arg;
use rustc_hash::FxHashMap as HashMap;
use std::net::SocketAddr;

pub fn parse_opts(globals: &mut Globals) -> std::result::Result<(), anyhow::Error> {
  let _ = include_str!("../../Cargo.toml");
  let options = clap::command!().arg(
    Arg::new("config_file")
      .long("config")
      .short('c')
      .value_name("FILE")
      .help("Configuration file path like \"./config.toml\""),
  );
  let matches = options.get_matches();

  let config = if let Some(config_file_path) = matches.get_one::<String>("config_file") {
    ConfigToml::new(config_file_path)?
  } else {
    // Default config Toml
    ConfigToml::default()
  };

  // listen port and socket
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
  // NOTE: when [::]:xx is bound, both v4 and v6 listeners are enabled.
  let listen_addresses: Vec<&str> = match config.listen_ipv6 {
    Some(true) => {
      info!("Listen both IPv4 and IPv6");
      LISTEN_ADDRESSES_V6.to_vec()
    }
    Some(false) | None => {
      info!("Listen IPv4");
      LISTEN_ADDRESSES_V4.to_vec()
    }
  };
  globals.listen_sockets = listen_addresses
    .iter()
    .flat_map(|x| {
      let mut v: Vec<SocketAddr> = vec![];
      if let Some(p) = globals.http_port {
        v.push(format!("{x}:{p}").parse().unwrap());
      }
      if let Some(p) = globals.https_port {
        v.push(format!("{x}:{p}").parse().unwrap());
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
    let server_name_string = app.server_name.as_ref().unwrap();
    if globals.http_port.is_none() {
      // if only https_port is specified, tls must be configured
      ensure!(app.tls.is_some())
    }

    // backend builder
    let mut backend_builder = BackendBuilder::default();
    // reverse proxy settings
    ensure!(app.reverse_proxy.is_some(), "Missing reverse_proxy");
    let reverse_proxy = get_reverse_proxy(server_name_string, app.reverse_proxy.as_ref().unwrap())?;

    backend_builder
      .app_name(server_name_string)
      .server_name(server_name_string)
      .reverse_proxy(reverse_proxy);

    // TLS settings and build backend instance
    let backend = if app.tls.is_none() {
      ensure!(globals.http_port.is_some(), "Required HTTP port");
      backend_builder.build()?
    } else {
      let tls = app.tls.as_ref().unwrap();
      ensure!(tls.tls_cert_key_path.is_some() && tls.tls_cert_path.is_some());

      let https_redirection = if tls.https_redirection.is_none() {
        Some(true) // Default true
      } else {
        ensure!(globals.https_port.is_some()); // only when both https ports are configured.
        tls.https_redirection
      };

      backend_builder
        .tls_cert_path(&tls.tls_cert_path)
        .tls_cert_key_path(&tls.tls_cert_key_path)
        .https_redirection(https_redirection)
        .client_ca_cert_path(&tls.client_ca_cert_path)
        .build()?
    };

    globals
      .backends
      .apps
      .insert(server_name_string.to_server_name_vec(), backend);
    info!("Registering application: {} ({})", app_name, server_name_string);
  }

  // default backend application for plaintext http requests
  if let Some(d) = config.default_app {
    let d_sn: Vec<&str> = globals
      .backends
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
      globals.backends.default_server_name_bytes = Some(d_sn[0].to_server_name_vec());
    }
  }

  // experimental
  if let Some(exp) = config.experimental {
    #[cfg(feature = "http3")]
    {
      if let Some(h3option) = exp.h3 {
        globals.http3 = true;
        info!("Experimental HTTP/3.0 is enabled. Note it is still very unstable.");
        if let Some(x) = h3option.alt_svc_max_age {
          globals.h3_alt_svc_max_age = x;
        }
        if let Some(x) = h3option.request_max_body_size {
          globals.h3_request_max_body_size = x;
        }
        if let Some(x) = h3option.max_concurrent_connections {
          globals.h3_max_concurrent_connections = x;
        }
        if let Some(x) = h3option.max_concurrent_bidistream {
          globals.h3_max_concurrent_bidistream = x.into();
        }
        if let Some(x) = h3option.max_concurrent_unistream {
          globals.h3_max_concurrent_unistream = x.into();
        }
        if let Some(x) = h3option.max_idle_timeout {
          if x == 0u64 {
            globals.h3_max_idle_timeout = None;
          } else {
            globals.h3_max_idle_timeout =
              Some(quinn::IdleTimeout::try_from(tokio::time::Duration::from_secs(x)).unwrap())
          }
        }
      }
    }

    if let Some(b) = exp.ignore_sni_consistency {
      globals.sni_consistency = !b;
      if b {
        info!("Ignore consistency between TLS SNI and Host header (or Request line). Note it violates RFC.");
      }
    }
  }

  Ok(())
}

fn get_reverse_proxy(
  server_name_string: &str,
  rp_settings: &[ReverseProxyOption],
) -> std::result::Result<ReverseProxy, anyhow::Error> {
  let mut upstream: HashMap<PathNameBytesExp, UpstreamGroup> = HashMap::default();

  rp_settings.iter().for_each(|rpo| {
    let upstream_vec: Vec<Upstream> = rpo.upstream.iter().map(|x| x.to_upstream().unwrap()).collect();
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
