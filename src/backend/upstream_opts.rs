use crate::error::*;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum UpstreamOption {
  OverrideHost,
  UpgradeInsecureRequests,
  ConvertToHttp11,
  ConvertToHttp2,
  // TODO: Adds more options for heder override
}
impl TryFrom<&str> for UpstreamOption {
  type Error = RpxyError;
  fn try_from(val: &str) -> Result<Self> {
    match val {
      "override_host" => Ok(Self::OverrideHost),
      "upgrade_insecure_requests" => Ok(Self::UpgradeInsecureRequests),
      "convert_to_http11" => Ok(Self::ConvertToHttp11),
      "convert_to_http2" => Ok(Self::ConvertToHttp2),
      _ => Err(RpxyError::Other(anyhow!("Unsupported header option"))),
    }
  }
}
