use super::StickyCookieConfig;
use crate::error::{RpxyError, RpxyResult};
use aes_gcm::{
  Aes256Gcm,
  aead::{Aead, Generate, KeyInit, Nonce, Payload},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::Utc;
use secrecy::{ExposeSecret, SecretBox};
use std::sync::Arc;

pub(crate) const VERSION_V1: u8 = 0x01;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const EXPIRES_LEN: usize = 8;
const MAX_SERVER_ID_LEN: usize = 256;
const MIN_SERVER_ID_LEN: usize = 1;
const MIN_PLAINTEXT_LEN: usize = EXPIRES_LEN + MIN_SERVER_ID_LEN;
const MAX_PLAINTEXT_LEN: usize = EXPIRES_LEN + MAX_SERVER_ID_LEN;
const MIN_BLOB_LEN: usize = 1 + NONCE_LEN + TAG_LEN + MIN_PLAINTEXT_LEN;
const MAX_BLOB_LEN: usize = 1 + NONCE_LEN + TAG_LEN + MAX_PLAINTEXT_LEN;

/// Operator-supplied sticky-cookie AEAD secret.
///
/// The inner bytes are deliberately opaque outside this module. `rpxy-bin`
/// validates TOML through `try_from_config_value`, then passes this newtype
/// through to `rpxy-lib` runtime construction.
pub struct StickyCookieSecret(SecretBox<[u8; 32]>);

impl StickyCookieSecret {
  pub fn try_from_config_value(s: &str) -> RpxyResult<Self> {
    if s.as_bytes().iter().any(|b| b.is_ascii_whitespace()) {
      return Err(RpxyError::InvalidStickyCookieSecret(
        "sticky_cookie_secret must not contain embedded whitespace".to_string(),
      ));
    }

    let mut bytes = [0u8; 32];
    let decoded_len = URL_SAFE_NO_PAD.decode_slice(s, &mut bytes).map_err(|e| {
      RpxyError::InvalidStickyCookieSecret(format!(
        "sticky_cookie_secret must be base64url without padding and decode to exactly 32 bytes: {e}",
      ))
    })?;

    if decoded_len != bytes.len() {
      return Err(RpxyError::InvalidStickyCookieSecret(format!(
        "sticky_cookie_secret must decode to exactly 32 bytes, got {decoded_len} bytes",
      )));
    }

    Ok(Self(SecretBox::new(Box::new(bytes))))
  }

  pub(crate) fn expose(&self) -> &[u8; 32] {
    self.0.expose_secret()
  }
}

pub(crate) fn build_sticky_cookie_cipher(secret: &StickyCookieSecret) -> RpxyResult<Arc<Aes256Gcm>> {
  let cipher = Aes256Gcm::new_from_slice(secret.expose())
    .map_err(|e| RpxyError::InvalidStickyCookieSecret(format!("failed to initialize sticky-cookie AES-256-GCM cipher: {e}")))?;
  Ok(Arc::new(cipher))
}

pub fn validate_sticky_cookie_aad_component(component: &str, value: &str) -> RpxyResult<()> {
  if value.as_bytes().contains(&0) {
    return Err(RpxyError::InvalidStickyCookieAad(format!(
      "sticky-cookie AAD component {component} must not contain NUL bytes",
    )));
  }
  Ok(())
}

pub(crate) fn build_sticky_cookie_aad(config: &StickyCookieConfig) -> RpxyResult<Vec<u8>> {
  validate_sticky_cookie_aad_component("name", &config.name)?;
  validate_sticky_cookie_aad_component("domain", &config.domain)?;
  validate_sticky_cookie_aad_component("path", &config.path)?;

  let mut aad =
    Vec::with_capacity(b"rpxy-sticky-v1".len() + 1 + config.name.len() + 1 + config.domain.len() + 1 + config.path.len() + 1);
  aad.extend_from_slice(b"rpxy-sticky-v1");
  aad.push(0);
  aad.extend_from_slice(config.name.as_bytes());
  aad.push(0);
  aad.extend_from_slice(config.domain.as_bytes());
  aad.push(0);
  aad.extend_from_slice(config.path.as_bytes());
  aad.push(0);
  Ok(aad)
}

pub(crate) fn seal_server_id(cipher: &Aes256Gcm, aad: &[u8], server_id: &str, expires_unix: i64) -> RpxyResult<String> {
  if server_id.is_empty() || server_id.len() > MAX_SERVER_ID_LEN {
    return Err(RpxyError::FailedToSealStickyCookie);
  }

  let mut plaintext = Vec::with_capacity(EXPIRES_LEN + server_id.len());
  plaintext.extend_from_slice(&expires_unix.to_be_bytes());
  plaintext.extend_from_slice(server_id.as_bytes());

  let nonce = Nonce::<Aes256Gcm>::generate();
  let ct_with_tag = cipher
    .encrypt(&nonce, Payload { msg: &plaintext, aad })
    .map_err(|_| RpxyError::FailedToSealStickyCookie)?;

  let mut blob = Vec::with_capacity(1 + NONCE_LEN + ct_with_tag.len());
  blob.push(VERSION_V1);
  blob.extend_from_slice(nonce.as_slice());
  blob.extend_from_slice(&ct_with_tag);
  Ok(URL_SAFE_NO_PAD.encode(blob))
}

pub(crate) fn open_server_id(cipher: &Aes256Gcm, aad: &[u8], blob_b64: &str) -> Option<String> {
  open_server_id_at(cipher, aad, blob_b64, Utc::now().timestamp())
}

fn open_server_id_at(cipher: &Aes256Gcm, aad: &[u8], blob_b64: &str, now_unix: i64) -> Option<String> {
  let blob = URL_SAFE_NO_PAD.decode(blob_b64).ok()?;
  if !(MIN_BLOB_LEN..=MAX_BLOB_LEN).contains(&blob.len()) {
    return None;
  }
  if blob[0] != VERSION_V1 {
    return None;
  }

  let (nonce_bytes, ct_with_tag) = blob[1..].split_at(NONCE_LEN);
  let nonce = Nonce::<Aes256Gcm>::try_from(nonce_bytes).ok()?;
  let plaintext = cipher.decrypt(&nonce, Payload { msg: ct_with_tag, aad }).ok()?;
  if !(MIN_PLAINTEXT_LEN..=MAX_PLAINTEXT_LEN).contains(&plaintext.len()) {
    return None;
  }

  let expires_unix = i64::from_be_bytes(plaintext[..EXPIRES_LEN].try_into().ok()?);
  if expires_unix <= now_unix {
    return None;
  }

  String::from_utf8(plaintext[EXPIRES_LEN..].to_vec()).ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn secret(seed: u8) -> StickyCookieSecret {
    let encoded = URL_SAFE_NO_PAD.encode([seed; 32]);
    StickyCookieSecret::try_from_config_value(&encoded).unwrap()
  }

  fn cipher(seed: u8) -> Arc<Aes256Gcm> {
    build_sticky_cookie_cipher(&secret(seed)).unwrap()
  }

  fn aad(label: &str) -> Vec<u8> {
    format!("rpxy-test\0{label}\0").into_bytes()
  }

  #[test]
  fn secret_accepts_valid_32_byte_base64url() {
    assert!(StickyCookieSecret::try_from_config_value(&URL_SAFE_NO_PAD.encode([42u8; 32])).is_ok());
  }

  #[test]
  fn secret_rejects_wrong_length() {
    assert!(StickyCookieSecret::try_from_config_value(&URL_SAFE_NO_PAD.encode([42u8; 31])).is_err());
  }

  #[test]
  fn secret_rejects_malformed_base64() {
    assert!(StickyCookieSecret::try_from_config_value("not*base64url").is_err());
  }

  #[test]
  fn secret_rejects_embedded_whitespace() {
    let valid = URL_SAFE_NO_PAD.encode([42u8; 32]);
    assert!(StickyCookieSecret::try_from_config_value(&format!("{valid}\n")).is_err());
  }

  #[test]
  fn aad_component_rejects_nul_with_dedicated_error() {
    let err = validate_sticky_cookie_aad_component("path", "/bad\0path").unwrap_err();
    assert!(matches!(err, crate::error::RpxyError::InvalidStickyCookieAad(_)));
  }

  #[test]
  fn seal_open_roundtrip() {
    let cipher = cipher(1);
    let aad = aad("app-a");
    let sealed = seal_server_id(&cipher, &aad, "backend-a", 2_000).unwrap();
    assert_eq!(open_server_id_at(&cipher, &aad, &sealed, 1_000).as_deref(), Some("backend-a"));
  }

  #[test]
  fn expired_token_is_rejected() {
    let cipher = cipher(1);
    let aad = aad("app-a");
    let sealed = seal_server_id(&cipher, &aad, "backend-a", 1_000).unwrap();
    assert_eq!(open_server_id_at(&cipher, &aad, &sealed, 1_000), None);
  }

  #[test]
  fn tampered_ciphertext_is_rejected() {
    let cipher = cipher(1);
    let aad = aad("app-a");
    let sealed = seal_server_id(&cipher, &aad, "backend-a", 2_000).unwrap();
    let mut blob = URL_SAFE_NO_PAD.decode(sealed).unwrap();
    let last = blob.len() - 1;
    blob[last] ^= 0x01;
    assert_eq!(open_server_id_at(&cipher, &aad, &URL_SAFE_NO_PAD.encode(blob), 1_000), None);
  }

  #[test]
  fn tampered_version_is_rejected() {
    let cipher = cipher(1);
    let aad = aad("app-a");
    let sealed = seal_server_id(&cipher, &aad, "backend-a", 2_000).unwrap();
    let mut blob = URL_SAFE_NO_PAD.decode(sealed).unwrap();
    blob[0] = 0x02;
    assert_eq!(open_server_id_at(&cipher, &aad, &URL_SAFE_NO_PAD.encode(blob), 1_000), None);
  }

  #[test]
  fn tampered_nonce_is_rejected() {
    let cipher = cipher(1);
    let aad = aad("app-a");
    let sealed = seal_server_id(&cipher, &aad, "backend-a", 2_000).unwrap();
    let mut blob = URL_SAFE_NO_PAD.decode(sealed).unwrap();
    blob[1] ^= 0x01;
    assert_eq!(open_server_id_at(&cipher, &aad, &URL_SAFE_NO_PAD.encode(blob), 1_000), None);
  }

  #[test]
  fn wrong_key_is_rejected() {
    let cipher_a = cipher(1);
    let cipher_b = cipher(2);
    let aad = aad("app-a");
    let sealed = seal_server_id(&cipher_a, &aad, "backend-a", 2_000).unwrap();
    assert_eq!(open_server_id_at(&cipher_b, &aad, &sealed, 1_000), None);
  }

  #[test]
  fn aad_mismatch_is_rejected() {
    let cipher = cipher(1);
    let sealed = seal_server_id(&cipher, &aad("app-a"), "backend-a", 2_000).unwrap();
    assert_eq!(open_server_id_at(&cipher, &aad("app-b"), &sealed, 1_000), None);
  }

  #[test]
  fn old_plaintext_cookie_is_rejected() {
    let cipher = cipher(1);
    assert_eq!(open_server_id(&cipher, &aad("app-a"), "backend1"), None);
  }

  #[test]
  fn truncated_input_is_rejected() {
    let cipher = cipher(1);
    let blob = vec![VERSION_V1; MIN_BLOB_LEN - 1];
    assert_eq!(open_server_id(&cipher, &aad("app-a"), &URL_SAFE_NO_PAD.encode(blob)), None);
  }

  #[test]
  fn oversized_plaintext_is_rejected_at_encrypt() {
    let cipher = cipher(1);
    let too_long = "a".repeat(MAX_SERVER_ID_LEN + 1);
    assert!(seal_server_id(&cipher, &aad("app-a"), &too_long, 2_000).is_err());
  }

  #[test]
  fn non_utf8_plaintext_is_rejected() {
    let cipher = cipher(1);
    let aad = aad("app-a");
    let nonce = Nonce::<Aes256Gcm>::generate();
    let mut plaintext = Vec::from(2_000i64.to_be_bytes());
    plaintext.extend_from_slice(&[0xff, 0xfe]);
    let ct_with_tag = cipher
      .encrypt(
        &nonce,
        Payload {
          msg: &plaintext,
          aad: &aad,
        },
      )
      .unwrap();
    let mut blob = Vec::with_capacity(1 + NONCE_LEN + ct_with_tag.len());
    blob.push(VERSION_V1);
    blob.extend_from_slice(nonce.as_slice());
    blob.extend_from_slice(&ct_with_tag);
    assert_eq!(open_server_id_at(&cipher, &aad, &URL_SAFE_NO_PAD.encode(blob), 1_000), None);
  }
}
