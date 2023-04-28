/// Server name (hostname or ip address) representation in bytes-based struct
/// for searching hashmap or key list by exact or longest-prefix matching
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct ServerNameBytesExp(pub Vec<u8>); // lowercase ascii bytes
impl From<&[u8]> for ServerNameBytesExp {
  fn from(b: &[u8]) -> Self {
    Self(b.to_ascii_lowercase())
  }
}

/// Path name, like "/path/ok", represented in bytes-based struct
/// for searching hashmap or key list by exact or longest-prefix matching
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct PathNameBytesExp(pub Vec<u8>); // lowercase ascii bytes
impl PathNameBytesExp {
  pub fn len(&self) -> usize {
    self.0.len()
  }
  pub fn get<I>(&self, index: I) -> Option<&I::Output>
  where
    I: std::slice::SliceIndex<[u8]>,
  {
    self.0.get(index)
  }
  pub fn starts_with(&self, needle: &Self) -> bool {
    self.0.starts_with(&needle.0)
  }
}
impl AsRef<[u8]> for PathNameBytesExp {
  fn as_ref(&self) -> &[u8] {
    self.0.as_ref()
  }
}

/// Trait to express names in ascii-lowercased bytes
pub trait BytesName {
  type OutputSv: Send + Sync + 'static;
  type OutputPath;
  fn to_server_name_vec(self) -> Self::OutputSv;
  fn to_path_name_vec(self) -> Self::OutputPath;
}

impl<'a, T: Into<std::borrow::Cow<'a, str>>> BytesName for T {
  type OutputSv = ServerNameBytesExp;
  type OutputPath = PathNameBytesExp;

  fn to_server_name_vec(self) -> Self::OutputSv {
    let name = self.into().bytes().collect::<Vec<u8>>().to_ascii_lowercase();
    ServerNameBytesExp(name)
  }

  fn to_path_name_vec(self) -> Self::OutputPath {
    let name = self.into().bytes().collect::<Vec<u8>>().to_ascii_lowercase();
    PathNameBytesExp(name)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn bytes_name_str_works() {
    let s = "OK_string";
    let bn = s.to_path_name_vec();
    let bn_lc = s.to_server_name_vec();

    assert_eq!(Vec::from("ok_string".as_bytes()), bn.0);
    assert_eq!(Vec::from("ok_string".as_bytes()), bn_lc.0);
  }

  #[test]
  fn from_works() {
    let s = "OK_string".to_server_name_vec();
    let m = ServerNameBytesExp::from("OK_strinG".as_bytes());
    assert_eq!(s, m);
    assert_eq!(s.0, "ok_string".as_bytes().to_vec());
    assert_eq!(m.0, "ok_string".as_bytes().to_vec());
  }

  #[test]
  fn get_works() {
    let s = "OK_str".to_path_name_vec();
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
    let s = "OK_str".to_path_name_vec();
    let correct = "OK".to_path_name_vec();
    let incorrect = "KO".to_path_name_vec();
    assert!(s.starts_with(&correct));
    assert!(!s.starts_with(&incorrect));
  }

  #[test]
  fn as_ref_works() {
    let s = "OK_str".to_path_name_vec();
    assert_eq!(s.as_ref(), "ok_str".as_bytes());
  }
}
