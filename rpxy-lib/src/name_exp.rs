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
impl From<Vec<u8>> for ServerName {
  /// Owning conversion: lowercases in place instead of allocating a lowercased copy of bytes the
  /// caller already owns (the per-request host parsing path). Result bytes are identical to the
  /// borrowing `From<&[u8]>` conversion for every input.
  fn from(mut b: Vec<u8>) -> Self {
    b.make_ascii_lowercase();
    Self { inner: b }
  }
}
impl TryInto<String> for &ServerName {
  type Error = anyhow::Error;
  fn try_into(self) -> Result<String, Self::Error> {
    let s = std::str::from_utf8(&self.inner)?;
    Ok(s.to_string())
  }
}
impl std::fmt::Display for ServerName {
  /// On the normal request path, `ServerName` carries ASCII-lowercase hostname/IP bytes (the
  /// `&str` constructor lowercases ASCII; the parser-fed `&[u8]` / `Vec<u8>` constructors
  /// receive host bytes that are ASCII in practice). The byte constructors are public and only
  /// lowercase, however, so they can hold arbitrary bytes; `from_utf8_lossy` keeps the
  /// formatter total - it borrows the underlying bytes when they are valid UTF-8 (the normal
  /// case, zero allocation) and substitutes U+FFFD for invalid sequences instead of dropping
  /// the host entirely, which is strictly more useful than an `unwrap_or_default` that would
  /// log an empty string.
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&String::from_utf8_lossy(&self.inner))
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
    //let name = s.bytes().collect::<Vec<u8>>().to_ascii_lowercase();
    let name = s.bytes().collect::<Vec<u8>>();
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

    assert_eq!("OK_string".as_bytes(), bn.as_ref());
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

  /// The owning conversion must produce bytes identical to the borrowing one for every input
  /// shape the host-parsing path can hand over: mixed-case hostname, already-lowercase hostname,
  /// and v6-address-shaped bytes (which the parser does not pre-lowercase).
  #[test]
  fn from_owned_vec_matches_borrowed_slice() {
    for input in ["MiXeD.ExAmPle.COM", "already.lower.example.com", "2001:DB8::1", "127.0.0.1"] {
      let owned = ServerName::from(input.as_bytes().to_vec());
      let borrowed = ServerName::from(input.as_bytes());
      assert_eq!(owned, borrowed, "owned and borrowed conversions must agree for {input}");
    }
    assert_eq!(
      ServerName::from("MiXeD.ExAmPle.COM".as_bytes().to_vec()).as_ref(),
      "mixed.example.com".as_bytes()
    );
  }

  #[test]
  fn get_works() {
    let s = "OK_str".to_path_name();
    let i = s.get(0);
    assert_eq!(Some(&"O".as_bytes()[0]), i);
    let i = s.get(1);
    assert_eq!(Some(&"K".as_bytes()[0]), i);
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
    assert_eq!(s.as_ref(), "OK_str".as_bytes());
  }

  /// `Display` renders the lowercased ASCII hostname form the request flow normally produces.
  #[test]
  fn display_renders_ascii_hostname() {
    let s = ServerName::from("Example.COM");
    assert_eq!(format!("{s}"), "example.com");
  }

  /// `Display` renders IPv6-shaped byte input through the same lossy UTF-8 path; the bytes are
  /// valid UTF-8, so this exercises the zero-allocation borrow branch.
  #[test]
  fn display_renders_v6_address_bytes() {
    let s = ServerName::from("2001:DB8::1".as_bytes().to_vec());
    assert_eq!(format!("{s}"), "2001:db8::1");
  }

  /// The byte constructors are public and only lowercase, so they can carry non-UTF-8 bytes.
  /// `Display` substitutes U+FFFD for invalid sequences instead of dropping the host entirely
  /// (which is what the previous `TryInto<String>` + `unwrap_or_default()` did).
  #[test]
  fn display_substitutes_replacement_for_non_utf8_bytes() {
    let s = ServerName::from(vec![b'a', 0xFF, 0x80, b'z']);
    assert_eq!(format!("{s}"), format!("a{0}{0}z", char::REPLACEMENT_CHARACTER));
  }
}
