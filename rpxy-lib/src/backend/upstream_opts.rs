use crate::error::*;

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum UpstreamOption {
  OverrideHost,
  UpgradeInsecureRequests,
  ForceHttp11Upstream,
  ForceHttp2Upstream,
  // TODO: Adds more options for heder override
}
impl TryFrom<&str> for UpstreamOption {
  type Error = RpxyError;
  fn try_from(val: &str) -> RpxyResult<Self> {
    match val {
      "override_host" => Ok(Self::OverrideHost),
      "upgrade_insecure_requests" => Ok(Self::UpgradeInsecureRequests),
      "force_http11_upstream" => Ok(Self::ForceHttp11Upstream),
      "force_http2_upstream" => Ok(Self::ForceHttp2Upstream),
      _ => Err(RpxyError::UnsupportedUpstreamOption),
    }
  }
}
