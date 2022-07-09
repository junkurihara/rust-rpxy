use crate::error::*;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum UpstreamOption {
  OverrideHost,
  // TODO: Adds more options for heder override
}
impl TryFrom<&str> for UpstreamOption {
  type Error = anyhow::Error;
  fn try_from(val: &str) -> Result<Self> {
    match val {
      "override_host" => Ok(Self::OverrideHost),
      _ => Err(anyhow!("Unsupported header option")),
    }
  }
}
