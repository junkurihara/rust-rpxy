use crate::{
  certs::CryptoSource,
  error::*,
  log::*,
  name_exp::{ByteName, ServerName},
  AppConfig, AppConfigList,
};
use derive_builder::Builder;
use rustc_hash::FxHashMap as HashMap;
use std::borrow::Cow;

use super::upstream::PathManager;

/// Struct serving information to route incoming connections, like server name to be handled and tls certs/keys settings.
#[derive(Builder)]
pub struct BackendApp<T>
where
  T: CryptoSource,
{
  #[builder(setter(into))]
  /// backend application name, e.g., app1
  pub app_name: String,
  #[builder(setter(custom))]
  /// server name, e.g., example.com, in [[ServerName]] object
  pub server_name: ServerName,
  /// struct of reverse proxy serving incoming request
  pub path_manager: PathManager,
  /// tls settings: https redirection with 30x
  #[builder(default)]
  pub https_redirection: Option<bool>,
  /// TLS settings: source meta for server cert, key, client ca cert
  #[builder(default)]
  pub crypto_source: Option<T>,
}
impl<'a, T> BackendAppBuilder<T>
where
  T: CryptoSource,
{
  pub fn server_name(&mut self, server_name: impl Into<Cow<'a, str>>) -> &mut Self {
    self.server_name = Some(server_name.to_server_name());
    self
  }
}

/// HashMap and some meta information for multiple Backend structs.
pub struct BackendAppManager<T>
where
  T: CryptoSource,
{
  /// HashMap of Backend structs, key is server name
  pub apps: HashMap<ServerName, BackendApp<T>>,
  /// for plaintext http
  pub default_server_name: Option<ServerName>,
}

impl<T> Default for BackendAppManager<T>
where
  T: CryptoSource,
{
  fn default() -> Self {
    Self {
      apps: HashMap::<ServerName, BackendApp<T>>::default(),
      default_server_name: None,
    }
  }
}

impl<T> TryFrom<&AppConfig<T>> for BackendApp<T>
where
  T: CryptoSource + Clone,
{
  type Error = RpxyError;

  fn try_from(app_config: &AppConfig<T>) -> Result<Self, Self::Error> {
    let mut backend_builder = BackendAppBuilder::default();
    let path_manager = PathManager::try_from(app_config)?;
    backend_builder
      .app_name(app_config.app_name.clone())
      .server_name(app_config.server_name.clone())
      .path_manager(path_manager);
    // TLS settings and build backend instance
    let backend = if app_config.tls.is_none() {
      backend_builder.build()?
    } else {
      let tls = app_config.tls.as_ref().unwrap();
      backend_builder
        .https_redirection(Some(tls.https_redirection))
        .crypto_source(Some(tls.inner.clone()))
        .build()?
    };
    Ok(backend)
  }
}

impl<T> TryFrom<&AppConfigList<T>> for BackendAppManager<T>
where
  T: CryptoSource + Clone,
{
  type Error = RpxyError;

  fn try_from(config_list: &AppConfigList<T>) -> Result<Self, Self::Error> {
    let mut manager = Self::default();
    for app_config in config_list.inner.iter() {
      let backend: BackendApp<T> = BackendApp::try_from(app_config)?;
      manager
        .apps
        .insert(app_config.server_name.clone().to_server_name(), backend);

      info!(
        "Registering application {} ({})",
        &app_config.server_name, &app_config.app_name
      );
    }

    // default backend application for plaintext http requests
    if let Some(default_app_name) = &config_list.default_app {
      let default_server_name = manager
        .apps
        .iter()
        .filter(|(_k, v)| &v.app_name == default_app_name)
        .map(|(_, v)| v.server_name.clone())
        .collect::<Vec<_>>();

      if !default_server_name.is_empty() {
        info!(
          "Serving plaintext http for requests to unconfigured server_name by app {} (server_name: {}).",
          &default_app_name,
          (&default_server_name[0]).try_into().unwrap_or_else(|_| "".to_string())
        );

        manager.default_server_name = Some(default_server_name[0].clone());
      }
    }
    Ok(manager)
  }
}
