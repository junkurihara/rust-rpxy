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
use crate::utils::{BytesName, PathNameBytesExp, ServerNameBytesExp};
use derive_builder::Builder;
use rustc_hash::FxHashMap as HashMap;
use std::{borrow::Cow, path::PathBuf};

/// Struct serving information to route incoming connections, like server name to be handled and tls certs/keys settings.
#[derive(Builder)]
pub struct Backend {
  #[builder(setter(into))]
  /// backend application name, e.g., app1
  pub app_name: String,
  #[builder(setter(custom))]
  /// server name, e.g., example.com, in String ascii lower case
  pub server_name: String,
  /// struct of reverse proxy serving incoming request
  pub reverse_proxy: ReverseProxy,

  /// tls settings
  #[builder(setter(custom), default)]
  pub tls_cert_path: Option<PathBuf>,
  #[builder(setter(custom), default)]
  pub tls_cert_key_path: Option<PathBuf>,
  #[builder(default)]
  pub https_redirection: Option<bool>,
  #[builder(setter(custom), default)]
  pub client_ca_cert_path: Option<PathBuf>,
}
impl<'a> BackendBuilder {
  pub fn server_name(&mut self, server_name: impl Into<Cow<'a, str>>) -> &mut Self {
    self.server_name = Some(server_name.into().to_ascii_lowercase());
    self
  }
  pub fn tls_cert_path(&mut self, v: &Option<String>) -> &mut Self {
    self.tls_cert_path = Some(opt_string_to_opt_pathbuf(v));
    self
  }
  pub fn tls_cert_key_path(&mut self, v: &Option<String>) -> &mut Self {
    self.tls_cert_key_path = Some(opt_string_to_opt_pathbuf(v));
    self
  }
  pub fn client_ca_cert_path(&mut self, v: &Option<String>) -> &mut Self {
    self.client_ca_cert_path = Some(opt_string_to_opt_pathbuf(v));
    self
  }
}

fn opt_string_to_opt_pathbuf(input: &Option<String>) -> Option<PathBuf> {
  input.to_owned().as_ref().map(PathBuf::from)
}

#[derive(Default)]
/// HashMap and some meta information for multiple Backend structs.
pub struct Backends {
  pub apps: HashMap<ServerNameBytesExp, Backend>, // hyper::uriで抜いたhostで引っ掛ける
  pub default_server_name_bytes: Option<ServerNameBytesExp>, // for plaintext http
}
