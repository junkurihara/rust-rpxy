use super::toml::ConfigToml;
use async_trait::async_trait;
use hot_reload::{Reload, ReloaderError};

#[derive(Clone)]
pub struct ConfigTomlReloader {
  pub config_path: String,
}

#[async_trait]
impl Reload<ConfigToml, String> for ConfigTomlReloader {
  type Source = String;
  async fn new(source: &Self::Source) -> Result<Self, ReloaderError<ConfigToml, String>> {
    Ok(Self {
      config_path: source.clone(),
    })
  }

  async fn reload(&self) -> Result<Option<ConfigToml>, ReloaderError<ConfigToml, String>> {
    let conf = ConfigToml::new(&self.config_path).map_err(|e| ReloaderError::<ConfigToml, String>::Reload(e.to_string()))?;
    Ok(Some(conf))
  }
}
