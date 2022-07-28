// Server name (hostname or ip address) and path name representation in backends
// For searching hashmap or key list by exact or longest-prefix matching
pub type ServerNameBytesExp = Vec<u8>; // lowercase ascii bytes

// #[derive(Clone, Debug)]
// pub struct ServerNameBytesExp(Vec<u8>);

pub type PathNameBytesExp = Vec<u8>; // lowercase ascii bytes

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
    name
  }

  fn to_path_name_vec(self) -> Self::OutputPath {
    let name = self.into().bytes().collect::<Vec<u8>>().to_ascii_lowercase();
    name
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

    assert_eq!(Vec::from(s.as_bytes()), bn);
    assert_eq!(Vec::from(s.as_bytes()), bn_lc);
  }
}
