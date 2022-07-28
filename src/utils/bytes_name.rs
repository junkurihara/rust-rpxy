use bytes::{Buf, Bytes};

pub trait BytesName {
  type Output: Buf;
  fn to_bytes(self) -> Self::Output;
  fn to_ascii_lowercase_bytes(self) -> Self::Output;
}

impl<T: Into<String>> BytesName for T {
  type Output = Bytes;

  fn to_bytes(self) -> Self::Output {
    let b = self.into().bytes().collect::<Vec<u8>>();
    Bytes::from(b)
  }

  fn to_ascii_lowercase_bytes(self) -> Self::Output {
    let b = self.into().bytes().collect::<Vec<u8>>().to_ascii_lowercase();
    Bytes::from(b)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn bytes_name_str_works() {
    let s = "OK_string";
    let bn = s.to_bytes();
    let bn_lc = s.to_ascii_lowercase_bytes();

    assert_eq!(Bytes::from(s.as_bytes()), bn);
    assert_eq!(Bytes::from("ok_string"), bn_lc);
  }
}
