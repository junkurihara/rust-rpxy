use std::borrow::Cow;

/// Server name (hostname or ip address) representation in bytes-based struct
/// for searching hashmap or key list by exact or longest-prefix matching
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct ServerName {
  inner: Vec<u8>, // lowercase ascii bytes
}
impl From<&str> for ServerName {
  fn from(s: &str) -> Self {
    let name = s.bytes().collect::<Vec<u8>>().to_ascii_lowercase();
    Self { inner: name }
  }
}
impl From<&[u8]> for ServerName {
  fn from(b: &[u8]) -> Self {
    Self {
      inner: b.to_ascii_lowercase(),
    }
  }
}
impl TryInto<String> for &ServerName {
  type Error = anyhow::Error;
  fn try_into(self) -> Result<String, Self::Error> {
    let s = std::str::from_utf8(&self.inner)?;
    Ok(s.to_string())
  }
}
impl AsRef<[u8]> for ServerName {
  fn as_ref(&self) -> &[u8] {
    self.inner.as_ref()
  }
}

/// Path name, like "/path/ok", represented in bytes-based struct
/// for searching hashmap or key list by exact or longest-prefix matching
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct PathName {
  inner: Vec<u8>, // lowercase ascii bytes
}
impl From<&str> for PathName {
  fn from(s: &str) -> Self {
    let name = s.bytes().collect::<Vec<u8>>().to_ascii_lowercase();
    Self { inner: name }
  }
}
impl From<&[u8]> for PathName {
  fn from(b: &[u8]) -> Self {
    Self {
      inner: b.to_ascii_lowercase(),
    }
  }
}
impl TryInto<String> for &PathName {
  type Error = anyhow::Error;
  fn try_into(self) -> Result<String, Self::Error> {
    let s = std::str::from_utf8(&self.inner)?;
    Ok(s.to_string())
  }
}
impl AsRef<[u8]> for PathName {
  fn as_ref(&self) -> &[u8] {
    self.inner.as_ref()
  }
}
impl PathName {
  pub fn len(&self) -> usize {
    self.inner.len()
  }
  pub fn is_empty(&self) -> bool {
    self.inner.len() == 0
  }
  pub fn get<I>(&self, index: I) -> Option<&I::Output>
  where
    I: std::slice::SliceIndex<[u8]>,
  {
    self.inner.get(index)
  }
  pub fn starts_with(&self, needle: &Self) -> bool {
    self.inner.starts_with(&needle.inner)
  }
}

/// Trait to express names in ascii-lowercased bytes
pub trait ByteName {
  type OutputServer: Send + Sync + 'static;
  type OutputPath;
  fn to_server_name(self) -> Self::OutputServer;
  fn to_path_name(self) -> Self::OutputPath;
}

impl<'a, T: Into<Cow<'a, str>>> ByteName for T {
  type OutputServer = ServerName;
  type OutputPath = PathName;

  fn to_server_name(self) -> Self::OutputServer {
    ServerName::from(self.into().as_ref())
  }

  fn to_path_name(self) -> Self::OutputPath {
    PathName::from(self.into().as_ref())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn bytes_name_str_works() {
    let s = "OK_string";
    let bn = s.to_path_name();
    let bn_lc = s.to_server_name();

    assert_eq!("ok_string".as_bytes(), bn.as_ref());
    assert_eq!("ok_string".as_bytes(), bn_lc.as_ref());
  }

  #[test]
  fn from_works() {
    let s = "OK_string".to_server_name();
    let m = ServerName::from("OK_strinG".as_bytes());
    assert_eq!(s, m);
    assert_eq!(s.as_ref(), "ok_string".as_bytes());
    assert_eq!(m.as_ref(), "ok_string".as_bytes());
  }

  #[test]
  fn get_works() {
    let s = "OK_str".to_path_name();
    let i = s.get(0);
    assert_eq!(Some(&"o".as_bytes()[0]), i);
    let i = s.get(1);
    assert_eq!(Some(&"k".as_bytes()[0]), i);
    let i = s.get(2);
    assert_eq!(Some(&"_".as_bytes()[0]), i);
    let i = s.get(3);
    assert_eq!(Some(&"s".as_bytes()[0]), i);
    let i = s.get(4);
    assert_eq!(Some(&"t".as_bytes()[0]), i);
    let i = s.get(5);
    assert_eq!(Some(&"r".as_bytes()[0]), i);
    let i = s.get(6);
    assert_eq!(None, i);
  }

  #[test]
  fn start_with_works() {
    let s = "OK_str".to_path_name();
    let correct = "OK".to_path_name();
    let incorrect = "KO".to_path_name();
    assert!(s.starts_with(&correct));
    assert!(!s.starts_with(&incorrect));
  }

  #[test]
  fn as_ref_works() {
    let s = "OK_str".to_path_name();
    assert_eq!(s.as_ref(), "ok_str".as_bytes());
  }
}
