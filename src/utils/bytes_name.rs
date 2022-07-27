pub trait BytesName {
  type Output;
  fn to_byte_name(self) -> Self::Output;
  fn to_ascii_lowercase_byte_name(self) -> Self::Output;
}

impl<T: Into<String>> BytesName for T {
  type Output = Vec<u8>;

  fn to_byte_name(self) -> Self::Output {
    self.into().bytes().collect::<Vec<u8>>()
    // Bytes::from(b)
  }

  fn to_ascii_lowercase_byte_name(self) -> Self::Output {
    self.into().bytes().collect::<Vec<u8>>().to_ascii_lowercase()
    // Bytes::from(b)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn bytes_name_str_works() {
    let s = "OK_string";
    let bn = s.to_byte_name();
    let bn_lc = s.to_ascii_lowercase_byte_name();

    assert_eq!(Vec::from(s.as_bytes()), bn);
    assert_eq!(Vec::from("ok_string"), bn_lc);
  }
}
