use crate::globals::Globals;

#[cfg(feature = "tls")]
use std::path::PathBuf;

pub fn parse_opts(globals: &mut Globals) {
  #[cfg(feature = "tls")]
  {
    // TODO:
    globals.tls_cert_path = Some(PathBuf::from(r"localhost.pem"));
    globals.tls_cert_key_path = Some(PathBuf::from(r"localhost.pem"));
  }
}
