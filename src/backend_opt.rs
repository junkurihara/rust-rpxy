use crate::error::*;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum UpstreamOption {
  OverrideHost,
  UpgradeInsecureRequests,
  // TODO: Adds more options for heder override
}
impl TryFrom<&str> for UpstreamOption {
  type Error = RpxyError;
  fn try_from(val: &str) -> Result<Self> {
    match val {
      "override_host" => Ok(Self::OverrideHost),
      "upgrade_insecure_requests" => Ok(Self::UpgradeInsecureRequests),
      _ => Err(RpxyError::Other(anyhow!("Unsupported header option"))),
    }
  }
}
