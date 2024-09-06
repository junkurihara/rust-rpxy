use super::toml::ConfigToml;
use crate::error::{anyhow, ensure};
use clap::{Arg, ArgAction};
use hot_reload::{ReloaderReceiver, ReloaderService};
use rpxy_certs::{build_cert_reloader, CryptoFileSourceBuilder, CryptoReloader, ServerCryptoBase};
use rpxy_lib::{AppConfig, AppConfigList, ProxyConfig};
use rustc_hash::FxHashMap as HashMap;

#[cfg(feature = "acme")]
use rpxy_acme::{AcmeManager, ACME_DIR_URL, ACME_REGISTRY_PATH};

/// Parsed options
pub struct Opts {
  pub config_file_path: String,
  pub watch: bool,
}

/// Parse arg values passed from cli
pub fn parse_opts() -> Result<Opts, anyhow::Error> {
  let _ = include_str!("../../Cargo.toml");
  let options = clap::command!()
    .arg(
      Arg::new("config_file")
        .long("config")
        .short('c')
        .value_name("FILE")
        .required(true)
        .help("Configuration file path like ./config.toml"),
    )
    .arg(
      Arg::new("watch")
        .long("watch")
        .short('w')
        .action(ArgAction::SetTrue)
        .help("Activate dynamic reloading of the config file via continuous monitoring"),
    );
  let matches = options.get_matches();

  ///////////////////////////////////
  let config_file_path = matches.get_one::<String>("config_file").unwrap().to_owned();
  let watch = matches.get_one::<bool>("watch").unwrap().to_owned();

  Ok(Opts { config_file_path, watch })
}

pub fn build_settings(config: &ConfigToml) -> std::result::Result<(ProxyConfig, AppConfigList), anyhow::Error> {
  // build proxy config
  let proxy_config: ProxyConfig = config.try_into()?;

  // backend_apps
  let apps = config.apps.clone().ok_or(anyhow!("Missing application spec"))?;

  // assertions for all backend apps
  ensure!(!apps.0.is_empty(), "Wrong application spec.");
  // if only https_port is specified, tls must be configured for all apps
  if proxy_config.http_port.is_none() {
    ensure!(
      apps.0.iter().all(|(_, app)| app.tls.is_some()),
      "Some apps serves only plaintext HTTP"
    );
  }
  // https redirection port must be configured only when both http_port and https_port are configured.
  if proxy_config.https_redirection_port.is_some() {
    ensure!(
      proxy_config.https_port.is_some() && proxy_config.http_port.is_some(),
      "https_redirection_port can be specified only when both http_port and https_port are specified"
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

  // build applications
  let mut app_config_list_inner = Vec::<AppConfig>::new();

  for (app_name, app) in apps.0.iter() {
    let _server_name_string = app.server_name.as_ref().ok_or(anyhow!("No server name"))?;
    let registered_app_name = app_name.to_ascii_lowercase();
    let app_config = app.build_app_config(&registered_app_name)?;
    app_config_list_inner.push(app_config);
  }

  let app_config_list = AppConfigList {
    inner: app_config_list_inner,
    default_app: config.default_app.clone().map(|v| v.to_ascii_lowercase()), // default backend application for plaintext http requests
  };

  Ok((proxy_config, app_config_list))
}

/* ----------------------- */
/// Build cert map
pub async fn build_cert_manager(
  config: &ConfigToml,
) -> Result<
  Option<(
    ReloaderService<CryptoReloader, ServerCryptoBase>,
    ReloaderReceiver<ServerCryptoBase>,
  )>,
  anyhow::Error,
> {
  let apps = config.apps.as_ref().ok_or(anyhow!("No apps"))?;
  if config.listen_port_tls.is_none() {
    return Ok(None);
  }

  #[cfg(feature = "acme")]
  let acme_option = config.experimental.as_ref().and_then(|v| v.acme.clone());
  #[cfg(feature = "acme")]
  let acme_dir_url = acme_option
    .as_ref()
    .and_then(|v| v.dir_url.as_deref())
    .unwrap_or(ACME_DIR_URL);
  #[cfg(feature = "acme")]
  let acme_registry_path = acme_option
    .as_ref()
    .and_then(|v| v.registry_path.as_deref())
    .unwrap_or(ACME_REGISTRY_PATH);

  let mut crypto_source_map = HashMap::default();
  for app in apps.0.values() {
    if let Some(tls) = app.tls.as_ref() {
      let server_name = app.server_name.as_ref().ok_or(anyhow!("No server name"))?;

      #[cfg(not(feature = "acme"))]
      ensure!(tls.tls_cert_key_path.is_some() && tls.tls_cert_path.is_some());

      #[cfg(feature = "acme")]
      let tls = {
        let mut tls = tls.clone();
        if let Some(true) = tls.acme {
          ensure!(acme_option.is_some() && tls.tls_cert_key_path.is_none() && tls.tls_cert_path.is_none());
          // Both of tls_cert_key_path and tls_cert_path must be the same for ACME since it's a single file
          let subdir = format!("{}/{}", acme_registry_path, server_name.to_ascii_lowercase());
          let file_name =
            rpxy_acme::DirCache::cached_cert_file_name(&[server_name.to_ascii_lowercase()], acme_dir_url.to_ascii_lowercase());
          tls.tls_cert_key_path = Some(format!("{}/{}", subdir, file_name));
          tls.tls_cert_path = Some(format!("{}/{}", subdir, file_name));
        }
        tls
      };

      let crypto_file_source = CryptoFileSourceBuilder::default()
        .tls_cert_path(tls.tls_cert_path.as_ref().unwrap())
        .tls_cert_key_path(tls.tls_cert_key_path.as_ref().unwrap())
        .client_ca_cert_path(tls.client_ca_cert_path.as_deref())
        .build()?;
      crypto_source_map.insert(server_name.to_owned(), crypto_file_source);
    }
  }
  let res = build_cert_reloader(&crypto_source_map, None).await?;
  Ok(Some(res))
}

/* ----------------------- */
#[cfg(feature = "acme")]
/// Build acme manager
pub async fn build_acme_manager(
  config: &ConfigToml,
  runtime_handle: tokio::runtime::Handle,
) -> Result<Option<AcmeManager>, anyhow::Error> {
  let acme_option = config.experimental.as_ref().and_then(|v| v.acme.clone());
  if acme_option.is_none() {
    return Ok(None);
  }
  let acme_option = acme_option.unwrap();

  let domains = config
    .apps
    .as_ref()
    .unwrap()
    .0
    .values()
    .filter_map(|app| {
      //
      if let Some(tls) = app.tls.as_ref() {
        if let Some(true) = tls.acme {
          return Some(app.server_name.as_ref().unwrap().to_owned());
        }
      }
      None
    })
    .collect::<Vec<_>>();

  if domains.is_empty() {
    return Ok(None);
  }

  let acme_manager = AcmeManager::try_new(
    acme_option.dir_url.as_deref(),
    acme_option.registry_path.as_deref(),
    &[acme_option.email],
    domains.as_slice(),
    runtime_handle,
  )?;

  Ok(Some(acme_manager))
}
