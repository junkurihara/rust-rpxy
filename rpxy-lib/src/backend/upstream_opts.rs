use crate::error::*;

/// Options for request message to be sent to upstream.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub enum UpstreamOption {
  /// Keep original host header, which is prioritized over SetUpstreamHost
  KeepOriginalHost,
  /// Overwrite host header with upstream hostname
  SetUpstreamHost,
  /// Add upgrade-insecure-requests header
  UpgradeInsecureRequests,
  /// Force HTTP/1.1 upstream
  ForceHttp11Upstream,
  /// Force HTTP/2 upstream
  ForceHttp2Upstream,
  // TODO: Adds more options for heder override
}
impl TryFrom<&str> for UpstreamOption {
  type Error = RpxyError;
  fn try_from(val: &str) -> RpxyResult<Self> {
    match val {
      "keep_original_host" => Ok(Self::KeepOriginalHost),
      "set_upstream_host" => Ok(Self::SetUpstreamHost),
      "upgrade_insecure_requests" => Ok(Self::UpgradeInsecureRequests),
      "force_http11_upstream" => Ok(Self::ForceHttp11Upstream),
      "force_http2_upstream" => Ok(Self::ForceHttp2Upstream),
      _ => Err(RpxyError::UnsupportedUpstreamOption),
    }
  }
}
