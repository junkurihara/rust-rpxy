use super::{LoadBalanceError, LoadBalanceResult, sticky_cookie_seal::build_sticky_cookie_aad};
use crate::error::{RpxyError, RpxyResult};
use chrono::{TimeZone, Utc};
use derive_builder::Builder;
use std::{borrow::Cow, sync::Arc};

#[derive(Debug, Clone, Builder)]
/// Cookie value only, used for COOKIE in req
pub struct StickyCookieValue {
  #[builder(setter(custom))]
  /// Field name indicating sticky cookie
  pub name: String,
  #[builder(setter(custom))]
  /// Upstream server_id
  pub value: String,
}
impl<'a> StickyCookieValueBuilder {
  pub fn name(&mut self, v: impl Into<Cow<'a, str>>) -> &mut Self {
    self.name = Some(v.into().to_ascii_lowercase());
    self
  }
  pub fn value(&mut self, v: impl Into<Cow<'a, str>>) -> &mut Self {
    self.value = Some(v.into().to_string());
    self
  }
}
impl StickyCookieValue {
  pub fn try_from(value: &str, expected_name: &str) -> LoadBalanceResult<Self> {
    // Split on the first '=' only: a cookie `name=value` pair is defined by the first '=' and
    // the value may contain further '=' bytes. The wire value is a base64 NO_PAD AEAD blob and
    // never contains '=' today; a malformed multi-'=' token is still ignored downstream
    // (length / NO_PAD decode / AEAD verification in `open_server_id`).
    let Some((name, val)) = value.split_once('=') else {
      return Err(LoadBalanceError::InvalidStickyCookieStructure);
    };
    let (name, val) = (name.trim(), val.trim());
    if name != expected_name {
      return Err(LoadBalanceError::FailedToConversionStickyCookie);
    }
    if val.is_empty() {
      return Err(LoadBalanceError::NoStickyCookieValue);
    }
    Ok(StickyCookieValue {
      name: expected_name.to_string(),
      value: val.to_string(),
    })
  }
}

#[derive(Debug, Clone, Builder)]
/// Struct describing sticky cookie meta information used for SET-COOKIE in res
pub struct StickyCookieInfo {
  #[builder(setter(custom))]
  /// Unix time
  pub expires: i64,

  #[builder(setter(custom))]
  /// Domain
  pub domain: String,

  #[builder(setter(custom))]
  /// Path
  pub path: String,
}
impl<'a> StickyCookieInfoBuilder {
  pub fn domain(&mut self, v: impl Into<Cow<'a, str>>) -> &mut Self {
    self.domain = Some(v.into().to_ascii_lowercase());
    self
  }
  pub fn path(&mut self, v: impl Into<Cow<'a, str>>) -> &mut Self {
    // Do not lowercase: paths are case-sensitive and must match the route's case.
    self.path = Some(v.into().into_owned());
    self
  }
  pub fn expires(&mut self, duration_secs: i64) -> &mut Self {
    let current = Utc::now().timestamp();
    self.expires = Some(current + duration_secs);
    self
  }
}

#[derive(Debug, Clone, Builder)]
/// Struct describing sticky cookie
pub struct StickyCookie {
  #[builder(setter(custom))]
  /// Upstream server_id
  pub value: StickyCookieValue,
  #[builder(setter(custom), default)]
  /// Upstream server_id
  pub info: Option<StickyCookieInfo>,
}

impl<'a> StickyCookieBuilder {
  /// Set the value of sticky cookie
  pub fn value(&mut self, n: impl Into<Cow<'a, str>>, v: impl Into<Cow<'a, str>>) -> &mut Self {
    self.value = Some(StickyCookieValueBuilder::default().name(n).value(v).build().unwrap());
    self
  }
  /// Set the meta information of sticky cookie
  pub fn info(&mut self, domain: impl Into<Cow<'a, str>>, path: impl Into<Cow<'a, str>>, duration_secs: i64) -> &mut Self {
    let info = StickyCookieInfoBuilder::default()
      .domain(domain)
      .path(path)
      .expires(duration_secs)
      .build()
      .unwrap();
    self.info = Some(Some(info));
    self
  }
}

impl StickyCookie {
  /// Serialize the sticky cookie with a caller-supplied cookie value.
  ///
  /// Seals the internal plaintext `server_id` at the HTTP handler boundary.
  /// This method keeps the LB-owned metadata while allowing the wire value to be
  /// an opaque AEAD blob.
  pub fn to_set_cookie_value_with_value(&self, secure: bool, cookie_value: &str) -> LoadBalanceResult<String> {
    self.to_set_cookie_value_with_value_at(secure, cookie_value, Utc::now().timestamp())
  }

  /// Test-only serializer that keeps `Max-Age` deterministic against a fixed "now".
  #[cfg(test)]
  fn to_set_cookie_value_at(&self, secure: bool, now_ts: i64) -> LoadBalanceResult<String> {
    self.to_set_cookie_value_with_value_at(secure, &self.value.value, now_ts)
  }

  fn to_set_cookie_value_with_value_at(&self, secure: bool, cookie_value: &str, now_ts: i64) -> LoadBalanceResult<String> {
    let Some(info) = self.info.as_ref() else {
      return Err(LoadBalanceError::NoStickyCookieNoMetaInfo);
    };
    let chrono::LocalResult::Single(expires_timestamp) = Utc.timestamp_opt(info.expires, 0) else {
      return Err(LoadBalanceError::FailedToConversionStickyCookie);
    };
    let exp_str = expires_timestamp.format("%a, %d-%b-%Y %T GMT").to_string();
    let max_age = info.expires - now_ts;

    let mut s = format!(
      "{}={}; expires={}; Max-Age={}; path={}; domain={}; HttpOnly; SameSite=Lax",
      self.value.name, cookie_value, exp_str, max_age, info.path, info.domain
    );
    if secure {
      s.push_str("; Secure");
    }
    Ok(s)
  }
}

#[derive(Debug, Clone)]
/// Configuration to serve incoming requests in the manner of "sticky cookie".
/// Including a dictionary to map Ids included in cookie and upstream destinations,
/// and expiration of cookie.
/// "domain" and "path" in the cookie will be the same as the reverse proxy options.
pub struct StickyCookieConfig {
  /// All fields are private on purpose: `try_new` is the only construction path and the
  /// components are read-only afterwards (getters below), so a config can never carry - or be
  /// mutated into carrying - an AAD inconsistent with its name/domain/path.
  name: String,
  /// Precomputed `"{name}="` cookie prefix used to locate/strip the sticky cookie per request.
  name_prefix: String,
  domain: String,
  path: String,
  duration: i64,
  /// Precomputed AEAD AAD framing name/domain/path.
  aad: Arc<[u8]>,
}

impl StickyCookieConfig {
  /// Build a validated config. The domain (server name) is lowercased here, the path defaults to
  /// "/" but is otherwise kept verbatim (route matching is case-sensitive), and the AEAD AAD is
  /// validated and precomputed once - per-request paths reuse it via `aad()` instead of
  /// re-validating and re-allocating it on every request. An invalid component (NUL byte) is thus
  /// rejected when the backend is built (startup/config reload), not on each request.
  pub fn try_new(name: &str, server_name: &str, path_opt: &Option<String>, duration: i64) -> RpxyResult<Self> {
    if duration <= 0 {
      return Err(RpxyError::InvalidStickyCookieConfig(format!(
        "sticky_cookie_duration must be positive, got {duration}"
      )));
    }
    let name = name.to_string();
    let name_prefix = format!("{name}=");
    let domain = server_name.to_ascii_lowercase();
    let path = path_opt.as_deref().unwrap_or("/").to_string();
    let aad: Arc<[u8]> = build_sticky_cookie_aad(&name, &domain, &path)?.into();
    Ok(Self {
      name,
      name_prefix,
      domain,
      path,
      duration,
      aad,
    })
  }

  /// Precomputed AEAD AAD for sealing/opening sticky cookie values under this config.
  pub fn aad(&self) -> &[u8] {
    &self.aad
  }

  /// Sticky cookie name.
  pub fn name(&self) -> &str {
    &self.name
  }

  /// Precomputed `"{name}="` prefix for locating the sticky cookie token.
  pub fn name_prefix(&self) -> &str {
    &self.name_prefix
  }

  #[allow(unused)]
  /// Cookie domain (lowercased server name).
  pub fn domain(&self) -> &str {
    &self.domain
  }

  #[allow(unused)]
  /// Cookie path (verbatim; route matching is case-sensitive).
  pub fn path(&self) -> &str {
    &self.path
  }
}

impl<'a> StickyCookieConfig {
  pub fn build_sticky_cookie(&self, v: impl Into<Cow<'a, str>>) -> LoadBalanceResult<StickyCookie> {
    StickyCookieBuilder::default()
      .value(self.name.clone(), v)
      .info(&self.domain, &self.path, self.duration)
      .build()
      .map_err(|_| LoadBalanceError::FailedToBuildStickyCookie)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::constants::STICKY_COOKIE_NAME;

  #[test]
  fn config_works() {
    let config = StickyCookieConfig::try_new(STICKY_COOKIE_NAME, "example.com", &Some("/path".to_string()), 100).unwrap();
    let cookie = config.build_sticky_cookie("test_value").unwrap();

    // Pin both `expires` (read from the built cookie) and `now_ts` so Max-Age is
    // deterministic and independent of how many seconds have elapsed during the test.
    let actual_expires = cookie.info.as_ref().unwrap().expires;
    let now_ts = actual_expires - 100; // duration_secs at build time was 100
    let expires_date_string = Utc
      .timestamp_opt(actual_expires, 0)
      .unwrap()
      .format("%a, %d-%b-%Y %T GMT")
      .to_string();

    let secure_string = cookie.to_set_cookie_value_at(true, now_ts).unwrap();
    assert_eq!(
      secure_string,
      format!(
        "{}=test_value; expires={}; Max-Age=100; path=/path; domain=example.com; HttpOnly; SameSite=Lax; Secure",
        STICKY_COOKIE_NAME, expires_date_string
      )
    );

    let insecure_string = cookie.to_set_cookie_value_at(false, now_ts).unwrap();
    assert_eq!(
      insecure_string,
      format!(
        "{}=test_value; expires={}; Max-Age=100; path=/path; domain=example.com; HttpOnly; SameSite=Lax",
        STICKY_COOKIE_NAME, expires_date_string
      )
    );
  }

  #[test]
  fn try_new_rejects_non_positive_duration() {
    for duration in [0, -1] {
      let err = StickyCookieConfig::try_new(STICKY_COOKIE_NAME, "example.com", &Some("/path".to_string()), duration).unwrap_err();
      assert!(matches!(err, RpxyError::InvalidStickyCookieConfig(_)));
      assert!(err.to_string().contains("sticky_cookie_duration must be positive"));
    }
  }

  #[test]
  fn to_string_works() {
    let sc = StickyCookie {
      value: StickyCookieValue {
        name: STICKY_COOKIE_NAME.to_string(),
        value: "test_value".to_string(),
      },
      info: Some(StickyCookieInfo {
        expires: 1686221173i64,
        domain: "example.com".to_string(),
        path: "/path".to_string(),
      }),
    };
    // Fixed `now_ts` so Max-Age is a stable literal that does not depend on wall-clock.
    let now_ts = 1686221000i64;
    let max_age = 173; // 1686221173 - 1686221000

    let secure_string = sc.to_set_cookie_value_at(true, now_ts).unwrap();
    assert_eq!(
      secure_string,
      format!(
        "{}=test_value; expires=Thu, 08-Jun-2023 10:46:13 GMT; Max-Age={}; path=/path; domain=example.com; HttpOnly; SameSite=Lax; Secure",
        STICKY_COOKIE_NAME, max_age
      )
    );

    let insecure_string = sc.to_set_cookie_value_at(false, now_ts).unwrap();
    assert_eq!(
      insecure_string,
      format!(
        "{}=test_value; expires=Thu, 08-Jun-2023 10:46:13 GMT; Max-Age={}; path=/path; domain=example.com; HttpOnly; SameSite=Lax",
        STICKY_COOKIE_NAME, max_age
      )
    );
  }

  #[test]
  fn sticky_cookie_value_requires_exact_cookie_name() {
    assert!(StickyCookieValue::try_from(&format!("{STICKY_COOKIE_NAME}=value"), STICKY_COOKIE_NAME).is_ok());
    assert!(StickyCookieValue::try_from(&format!("{STICKY_COOKIE_NAME}_shadow=value"), STICKY_COOKIE_NAME).is_err());
  }

  /// Empty-value tokens (`name=`) must still be rejected with `NoStickyCookieValue`. Pins the
  /// branch that is easy to lose when switching from `split('=')` + `len!=2` to `split_once`.
  #[test]
  fn try_from_rejects_empty_value() {
    let result = StickyCookieValue::try_from(&format!("{STICKY_COOKIE_NAME}="), STICKY_COOKIE_NAME);
    assert!(matches!(result, Err(LoadBalanceError::NoStickyCookieValue)));
  }

  /// A token without `=` is structurally malformed and must be rejected with
  /// `InvalidStickyCookieStructure`.
  #[test]
  fn try_from_rejects_missing_eq() {
    let result = StickyCookieValue::try_from(STICKY_COOKIE_NAME, STICKY_COOKIE_NAME);
    assert!(matches!(result, Err(LoadBalanceError::InvalidStickyCookieStructure)));
  }

  /// `split_once('=')` keeps the value intact even when it itself contains `=`. The wire value is
  /// a base64 NO_PAD blob that never carries `=` today, so this only documents the accepted
  /// semantics; the downstream length/NO_PAD/AEAD checks still reject such tokens end-to-end.
  #[test]
  fn try_from_preserves_eq_in_value() {
    let result = StickyCookieValue::try_from(&format!("{STICKY_COOKIE_NAME}=AA=BB"), STICKY_COOKIE_NAME).unwrap();
    assert_eq!(result.value, "AA=BB");
  }

  /// The precomputed AAD must be byte-identical to a fresh build from the same components, so
  /// cookies sealed before this change still open after it.
  #[test]
  fn try_new_precomputes_identical_aad() {
    let config = StickyCookieConfig::try_new(STICKY_COOKIE_NAME, "Example.COM", &Some("/App".to_string()), 300).unwrap();
    let fresh = build_sticky_cookie_aad(&config.name, &config.domain, &config.path).unwrap();
    assert_eq!(config.aad(), fresh.as_slice());
  }

  /// The precomputed name prefix must equal a fresh `"{name}="`, so per-request cookie matching is
  /// byte-identical to the previous inline `format!`.
  #[test]
  fn try_new_precomputes_identical_name_prefix() {
    let config = StickyCookieConfig::try_new(STICKY_COOKIE_NAME, "Example.COM", &Some("/App".to_string()), 300).unwrap();
    assert_eq!(config.name_prefix(), format!("{}=", config.name()).as_str());
  }

  /// Validation moved to construction time, not lost: a NUL byte in any component is rejected by
  /// `try_new` (i.e. at backend build / config reload), instead of failing every request.
  #[test]
  fn try_new_rejects_nul_components() {
    assert!(StickyCookieConfig::try_new("na\0me", "example.com", &None, 300).is_err());
    assert!(StickyCookieConfig::try_new(STICKY_COOKIE_NAME, "exa\0mple.com", &None, 300).is_err());
    assert!(StickyCookieConfig::try_new(STICKY_COOKIE_NAME, "example.com", &Some("/pa\0th".to_string()), 300).is_err());
  }

  #[test]
  fn sticky_cookie_path_preserves_case_domain_lowercased() {
    let config = StickyCookieConfig::try_new(STICKY_COOKIE_NAME, "Example.COM", &Some("/App/Sub".to_string()), 100).unwrap();
    assert_eq!(config.domain, "example.com", "try_new must lowercase the domain");
    assert_eq!(config.path, "/App/Sub", "try_new must keep the path verbatim");
    let cookie = config.build_sticky_cookie("v").unwrap();
    let info = cookie.info.as_ref().unwrap();
    assert_eq!(info.path, "/App/Sub", "path case must be preserved");
    assert_eq!(info.domain, "example.com", "domain must be lowercased");

    let now_ts = info.expires - 100;
    let serialized = cookie.to_set_cookie_value_at(false, now_ts).unwrap();
    assert!(serialized.contains("path=/App/Sub"), "got: {serialized}");
    assert!(serialized.contains("domain=example.com"), "got: {serialized}");
  }
}
