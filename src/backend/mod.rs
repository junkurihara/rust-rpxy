mod load_balance;
#[cfg(feature = "sticky-cookie")]
mod load_balance_sticky;
#[cfg(feature = "sticky-cookie")]
mod sticky_cookie;
mod upstream;
mod upstream_opts;

#[cfg(feature = "sticky-cookie")]
pub use self::sticky_cookie::{StickyCookie, StickyCookieValue};
pub use self::{
  load_balance::{LbContext, LoadBalance},
  upstream::{ReverseProxy, Upstream, UpstreamGroup, UpstreamGroupBuilder},
  upstream_opts::UpstreamOption,
};
use crate::{
  certs::CryptoSource,
  utils::{BytesName, PathNameBytesExp, ServerNameBytesExp},
};
use derive_builder::Builder;
use rustc_hash::FxHashMap as HashMap;
use std::borrow::Cow;

/// Struct serving information to route incoming connections, like server name to be handled and tls certs/keys settings.
#[derive(Builder)]
pub struct Backend<T>
where
  T: CryptoSource,
{
  #[builder(setter(into))]
  /// backend application name, e.g., app1
  pub app_name: String,
  #[builder(setter(custom))]
  /// server name, e.g., example.com, in String ascii lower case
  pub server_name: String,
  /// struct of reverse proxy serving incoming request
  pub reverse_proxy: ReverseProxy,

  /// tls settings: https redirection with 30x
  #[builder(default)]
  pub https_redirection: Option<bool>,

  /// TLS settings: source meta for server cert, key, client ca cert
  #[builder(default)]
  pub crypto_source: Option<T>,
}
impl<'a, T> BackendBuilder<T>
where
  T: CryptoSource,
{
  pub fn server_name(&mut self, server_name: impl Into<Cow<'a, str>>) -> &mut Self {
    self.server_name = Some(server_name.into().to_ascii_lowercase());
    self
  }
}

/// HashMap and some meta information for multiple Backend structs.
pub struct Backends<T>
where
  T: CryptoSource,
{
  pub apps: HashMap<ServerNameBytesExp, Backend<T>>, // hyper::uriで抜いたhostで引っ掛ける
  pub default_server_name_bytes: Option<ServerNameBytesExp>, // for plaintext http
}

impl<T> Backends<T>
where
  T: CryptoSource,
{
  pub fn new() -> Self {
    Backends {
      apps: HashMap::<ServerNameBytesExp, Backend<T>>::default(),
      default_server_name_bytes: None,
    }
  }
}
