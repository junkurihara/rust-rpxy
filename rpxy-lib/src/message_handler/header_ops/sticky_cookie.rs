use aes_gcm::Aes256Gcm;
use anyhow::Result;
use http::{HeaderMap, HeaderValue, header};

use crate::{
  backend::{
    LoadBalanceContext, StickyCookie, StickyCookieConfig, StickyCookieValue, build_sticky_cookie_aad, open_server_id,
    seal_server_id,
  },
  log::*,
};

/// Take sticky cookie header value from request header,
/// and returns LoadBalanceContext to be forwarded to LB if exist and if needed.
/// Removing sticky cookie is needed and it must not be passed to the upstream.
pub(crate) fn takeout_sticky_cookie_lb_context(
  headers: &mut HeaderMap,
  sticky_config: &StickyCookieConfig,
  cipher: &Aes256Gcm,
) -> Result<Option<LoadBalanceContext>> {
  let expected_cookie_name = &sticky_config.name;
  let mut headers_clone = headers.clone();

  match headers_clone.entry(header::COOKIE) {
    header::Entry::Vacant(_) => Ok(None),
    header::Entry::Occupied(entry) => {
      let sticky_cookie_prefix = format!("{expected_cookie_name}=");
      let cookies_iter = entry
        .iter()
        .flat_map(|v| v.to_str().unwrap_or("").split(';').map(|v| v.trim()));
      let (sticky_cookies, without_sticky_cookies): (Vec<_>, Vec<_>) =
        cookies_iter.into_iter().partition(|v| v.starts_with(&sticky_cookie_prefix));
      if sticky_cookies.is_empty() {
        return Ok(None);
      }
      anyhow::ensure!(sticky_cookies.len() == 1, "Invalid cookie: Multiple sticky cookie values");

      let cookies_passed_to_upstream = without_sticky_cookies.join("; ");
      let cookie_passed_to_lb = sticky_cookies.first().unwrap();
      headers.remove(header::COOKIE);
      if !cookies_passed_to_upstream.is_empty() {
        headers.insert(header::COOKIE, cookies_passed_to_upstream.parse()?);
      }

      let raw_sticky_cookie = StickyCookieValue::try_from(cookie_passed_to_lb, expected_cookie_name)?;
      let aad = build_sticky_cookie_aad(sticky_config)?;
      let Some(server_id) = open_server_id(cipher, &aad, &raw_sticky_cookie.value) else {
        debug!("Ignoring invalid sticky cookie value");
        return Ok(None);
      };
      let sticky_cookie = StickyCookie {
        value: StickyCookieValue {
          name: expected_cookie_name.to_string(),
          value: server_id,
        },
        info: None,
      };
      Ok(Some(LoadBalanceContext { sticky_cookie }))
    }
  }
}

/// Set-Cookie if LB Sticky is enabled and if cookie is newly created/updated.
/// Set-Cookie response header could be in multiple lines.
/// https://developer.mozilla.org/ja/docs/Web/HTTP/Headers/Set-Cookie
///
/// `secure` controls whether the `Secure` attribute is appended; the caller is expected
/// to derive it from the client-visible request scheme via `client_visible_secure()`.
pub(crate) fn set_sticky_cookie_lb_context(
  headers: &mut HeaderMap,
  context_from_lb: &LoadBalanceContext,
  sticky_config: &StickyCookieConfig,
  secure: bool,
  cipher: &Aes256Gcm,
) -> Result<()> {
  let aad = build_sticky_cookie_aad(sticky_config)?;
  let Some(cookie_info) = context_from_lb.sticky_cookie.info.as_ref() else {
    anyhow::bail!("sticky cookie metadata is missing");
  };
  let sealed_value = seal_server_id(cipher, &aad, &context_from_lb.sticky_cookie.value.value, cookie_info.expires)?;
  let sticky_cookie_string = context_from_lb
    .sticky_cookie
    .to_set_cookie_value_with_value(secure, &sealed_value)?;
  let new_header_val: HeaderValue = sticky_cookie_string.parse()?;
  let expected_cookie_name = &sticky_config.name;
  let expected_cookie_prefix = format!("{expected_cookie_name}=");
  match headers.entry(header::SET_COOKIE) {
    header::Entry::Vacant(entry) => {
      entry.insert(new_header_val);
    }
    header::Entry::Occupied(mut entry) => {
      let mut flag = false;
      for e in entry.iter_mut() {
        if e.to_str().unwrap_or("").starts_with(&expected_cookie_prefix) {
          *e = new_header_val.clone();
          flag = true;
        }
      }
      if !flag {
        entry.append(new_header_val);
      }
    }
  };
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{
    backend::{StickyCookieConfig, StickyCookieSecret, build_sticky_cookie_cipher},
    constants::STICKY_COOKIE_NAME,
  };
  use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

  fn cipher() -> aes_gcm::Aes256Gcm {
    let encoded = URL_SAFE_NO_PAD.encode([7u8; 32]);
    let secret = StickyCookieSecret::try_from_config_value(&encoded).unwrap();
    build_sticky_cookie_cipher(&secret).unwrap().as_ref().clone()
  }

  fn sticky_config(domain: &str) -> StickyCookieConfig {
    StickyCookieConfig {
      name: STICKY_COOKIE_NAME.to_string(),
      domain: domain.to_string(),
      path: "/".to_string(),
      duration: 300,
    }
  }

  #[test]
  fn set_then_takeout_roundtrips_opaque_cookie_value() {
    let cipher = cipher();
    let config = sticky_config("example.com");
    let context = LoadBalanceContext {
      sticky_cookie: config.build_sticky_cookie("backend-a").unwrap(),
    };
    let mut res_headers = HeaderMap::new();
    set_sticky_cookie_lb_context(&mut res_headers, &context, &config, true, &cipher).unwrap();

    let set_cookie = res_headers.get(header::SET_COOKIE).unwrap().to_str().unwrap();
    assert!(!set_cookie.contains("backend-a"));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Lax"));
    assert!(set_cookie.contains("Secure"));

    let cookie_pair = set_cookie.split(';').next().unwrap();
    let mut req_headers = HeaderMap::new();
    req_headers.insert(header::COOKIE, cookie_pair.parse().unwrap());

    let recovered = takeout_sticky_cookie_lb_context(&mut req_headers, &config, &cipher)
      .unwrap()
      .unwrap();
    assert_eq!(recovered.sticky_cookie.value.value, "backend-a");
    assert!(!req_headers.contains_key(header::COOKIE));
  }

  #[test]
  fn takeout_rejects_cross_app_aad_mismatch() {
    let cipher = cipher();
    let config_a = sticky_config("a.example.com");
    let config_b = sticky_config("b.example.com");
    let context = LoadBalanceContext {
      sticky_cookie: config_a.build_sticky_cookie("backend-a").unwrap(),
    };
    let mut res_headers = HeaderMap::new();
    set_sticky_cookie_lb_context(&mut res_headers, &context, &config_a, false, &cipher).unwrap();
    let cookie_pair = res_headers
      .get(header::SET_COOKIE)
      .unwrap()
      .to_str()
      .unwrap()
      .split(';')
      .next()
      .unwrap()
      .to_string();

    let mut req_headers = HeaderMap::new();
    req_headers.insert(header::COOKIE, cookie_pair.parse().unwrap());
    assert!(
      takeout_sticky_cookie_lb_context(&mut req_headers, &config_b, &cipher)
        .unwrap()
        .is_none()
    );
  }

  #[test]
  fn takeout_rejects_old_plaintext_cookie() {
    let cipher = cipher();
    let config = sticky_config("example.com");
    let mut req_headers = HeaderMap::new();
    req_headers.insert(header::COOKIE, format!("{}=backend-a", STICKY_COOKIE_NAME).parse().unwrap());
    assert!(
      takeout_sticky_cookie_lb_context(&mut req_headers, &config, &cipher)
        .unwrap()
        .is_none()
    );
  }

  #[test]
  fn takeout_rejects_expired_sealed_cookie() {
    let cipher = cipher();
    let config = sticky_config("example.com");
    let aad = build_sticky_cookie_aad(&config).unwrap();
    let expired_value = seal_server_id(&cipher, &aad, "backend-a", 0).unwrap();
    let mut req_headers = HeaderMap::new();
    req_headers.insert(
      header::COOKIE,
      format!("{}={expired_value}", STICKY_COOKIE_NAME).parse().unwrap(),
    );

    assert!(
      takeout_sticky_cookie_lb_context(&mut req_headers, &config, &cipher)
        .unwrap()
        .is_none()
    );
  }

  #[test]
  fn takeout_ignores_cookie_names_with_sticky_prefix_only() {
    let cipher = cipher();
    let config = sticky_config("example.com");
    let mut req_headers = HeaderMap::new();
    req_headers.insert(
      header::COOKIE,
      format!("{}_shadow=anything; session=keep", STICKY_COOKIE_NAME)
        .parse()
        .unwrap(),
    );

    assert!(
      takeout_sticky_cookie_lb_context(&mut req_headers, &config, &cipher)
        .unwrap()
        .is_none()
    );
    assert_eq!(
      req_headers.get(header::COOKIE).unwrap().to_str().unwrap(),
      format!("{}_shadow=anything; session=keep", STICKY_COOKIE_NAME)
    );
  }
}
