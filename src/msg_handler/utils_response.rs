use crate::{log::*, utils::*};
use hyper::Response;
use std::fmt::Display;

////////////////////////////////////////////////////
// Functions of utils for request messages
pub trait ResLog {
  fn log<T1: Display, T2: Display + ToCanonical>(
    self,
    server_name: &T1,
    client_addr: &T2,
    extra: Option<&str>,
  );
}
impl<B> ResLog for &Response<B> {
  fn log<T1: Display, T2: Display + ToCanonical>(
    self,
    server_name: &T1,
    client_addr: &T2,
    extra: Option<&str>,
  ) {
    let canonical_client_addr = client_addr.to_canonical();
    info!(
      "{} <- {} -- {} {:?} {:?} {}",
      canonical_client_addr,
      server_name,
      self.status(),
      self.version(),
      self.headers(),
      extra.map_or_else(|| "", |v| v)
    );
  }
}
