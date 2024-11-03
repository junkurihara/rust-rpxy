use super::toml::ConfigToml;
use async_trait::async_trait;
use hot_reload::{Reload, ReloaderError};
use tracing::warn;

#[derive(Clone)]
pub struct ConfigTomlReloader {
  pub config_path: String,
}

#[async_trait]
impl Reload<ConfigToml> for ConfigTomlReloader {
  type Source = String;
  async fn new(source: &Self::Source) -> Result<Self, ReloaderError<ConfigToml>> {
    Ok(Self {
      config_path: source.clone(),
    })
  }

  async fn reload(&self) -> Result<Option<ConfigToml>, ReloaderError<ConfigToml>> {
    let conf = ConfigToml::new(&self.config_path).map_err(|e| {
      warn!("Invalid toml file: {e:?}");
      ReloaderError::<ConfigToml>::Reload("Failed to reload config toml")
    })?;
    Ok(Some(conf))
  }
}
