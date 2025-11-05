use super::toml::ConfigToml;
use async_trait::async_trait;
use hot_reload::AsyncFileLoad;
use std::path::{Path, PathBuf};

pub type ConfigTomlReloader = hot_reload::file_reloader::FileReloader<ConfigToml>;

impl TryFrom<&PathBuf> for ConfigToml {
  type Error = String;

  fn try_from(path: &PathBuf) -> Result<Self, Self::Error> {
    let config_str = std::fs::read_to_string(path).map_err(|e| format!("Failed to read config file: {}", e))?;
    let config_toml: ConfigToml = toml::from_str(&config_str).map_err(|e| format!("Failed to parse toml config: {}", e))?;
    Ok(config_toml)
  }
}

#[async_trait]
impl AsyncFileLoad for ConfigToml {
  type Error = String;

  async fn async_load_from<T>(path: T) -> Result<Self, Self::Error>
  where
    T: AsRef<Path> + Send,
  {
    let config_str = tokio::fs::read_to_string(path)
      .await
      .map_err(|e| format!("Failed to read config file: {}", e))?;
    let config_toml: ConfigToml = toml::from_str(&config_str).map_err(|e| format!("Failed to parse toml config: {}", e))?;
    Ok(config_toml)
  }
}

// impl From<ConfigToml> for ConfigToml {
//   fn from(val: ConfigToml) -> Self {
//     val
//   }
// }
