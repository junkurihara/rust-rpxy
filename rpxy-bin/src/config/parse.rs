use super::toml::ConfigToml;
use crate::{
  cert_file_reader::CryptoFileSource,
  error::{anyhow, ensure},
};
use clap::{Arg, ArgAction};
use rpxy_lib::{AppConfig, AppConfigList, ProxyConfig};

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

  Ok(Opts {
    config_file_path,
    watch,
  })
}

pub fn build_settings(
  config: &ConfigToml,
) -> std::result::Result<(ProxyConfig, AppConfigList<CryptoFileSource>), anyhow::Error> {
  ///////////////////////////////////
  // build proxy config
  let proxy_config: ProxyConfig = config.try_into()?;

  ///////////////////////////////////
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
  let mut app_config_list_inner = Vec::<AppConfig<CryptoFileSource>>::new();

  // let mut backends = Backends::new();
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
