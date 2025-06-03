use super::{LoadBalanceError, LoadBalanceResult};
use chrono::{TimeZone, Utc};
use derive_builder::Builder;
use std::borrow::Cow;

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
    if !value.starts_with(expected_name) {
      return Err(LoadBalanceError::FailedToConversionStickyCookie);
    };
    let kv = value.split('=').map(|v| v.trim()).collect::<Vec<&str>>();
    if kv.len() != 2 {
      return Err(LoadBalanceError::InvalidStickyCookieStructure);
    };
    if kv[1].is_empty() {
      return Err(LoadBalanceError::NoStickyCookieValue);
    }
    Ok(StickyCookieValue {
      name: expected_name.to_string(),
      value: kv[1].to_string(),
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
    self.path = Some(v.into().to_ascii_lowercase());
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

impl TryInto<String> for StickyCookie {
  type Error = LoadBalanceError;

  fn try_into(self) -> LoadBalanceResult<String> {
    if self.info.is_none() {
      return Err(LoadBalanceError::NoStickyCookieNoMetaInfo);
    }
    let info = self.info.unwrap();
    let chrono::LocalResult::Single(expires_timestamp) = Utc.timestamp_opt(info.expires, 0) else {
      return Err(LoadBalanceError::FailedToConversionStickyCookie);
    };
    let exp_str = expires_timestamp.format("%a, %d-%b-%Y %T GMT").to_string();
    let max_age = info.expires - Utc::now().timestamp();

    Ok(format!(
      "{}={}; expires={}; Max-Age={}; path={}; domain={}",
      self.value.name, self.value.value, exp_str, max_age, info.path, info.domain
    ))
  }
}

#[derive(Debug, Clone)]
/// Configuration to serve incoming requests in the manner of "sticky cookie".
/// Including a dictionary to map Ids included in cookie and upstream destinations,
/// and expiration of cookie.
/// "domain" and "path" in the cookie will be the same as the reverse proxy options.
pub struct StickyCookieConfig {
  pub name: String,
  pub domain: String,
  pub path: String,
  pub duration: i64,
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
    let config = StickyCookieConfig {
      name: STICKY_COOKIE_NAME.to_string(),
      domain: "example.com".to_string(),
      path: "/path".to_string(),
      duration: 100,
    };
    let expires_unix = Utc::now().timestamp() + 100;
    let sc_string: LoadBalanceResult<String> = config.build_sticky_cookie("test_value").unwrap().try_into();
    let expires_date_string = Utc
      .timestamp_opt(expires_unix, 0)
      .unwrap()
      .format("%a, %d-%b-%Y %T GMT")
      .to_string();
    assert_eq!(
      sc_string.unwrap(),
      format!(
        "{}=test_value; expires={}; Max-Age={}; path=/path; domain=example.com",
        STICKY_COOKIE_NAME, expires_date_string, 100
      )
    );
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
    let sc_string: LoadBalanceResult<String> = sc.try_into();
    let max_age = 1686221173i64 - Utc::now().timestamp();
    assert!(sc_string.is_ok());
    assert_eq!(
      sc_string.unwrap(),
      format!(
        "{}=test_value; expires=Thu, 08-Jun-2023 10:46:13 GMT; Max-Age={}; path=/path; domain=example.com",
        STICKY_COOKIE_NAME, max_age
      )
    );
  }
}
