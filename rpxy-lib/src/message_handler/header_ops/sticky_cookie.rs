use anyhow::Result;
use http::{HeaderMap, HeaderValue, header};

use crate::backend::{LoadBalanceContext, StickyCookie, StickyCookieValue};

/// Take sticky cookie header value from request header,
/// and returns LoadBalanceContext to be forwarded to LB if exist and if needed.
/// Removing sticky cookie is needed and it must not be passed to the upstream.
pub(crate) fn takeout_sticky_cookie_lb_context(
  headers: &mut HeaderMap,
  expected_cookie_name: &str,
) -> Result<Option<LoadBalanceContext>> {
  let mut headers_clone = headers.clone();

  match headers_clone.entry(header::COOKIE) {
    header::Entry::Vacant(_) => Ok(None),
    header::Entry::Occupied(entry) => {
      let cookies_iter = entry
        .iter()
        .flat_map(|v| v.to_str().unwrap_or("").split(';').map(|v| v.trim()));
      let (sticky_cookies, without_sticky_cookies): (Vec<_>, Vec<_>) =
        cookies_iter.into_iter().partition(|v| v.starts_with(expected_cookie_name));
      if sticky_cookies.is_empty() {
        return Ok(None);
      }
      anyhow::ensure!(sticky_cookies.len() == 1, "Invalid cookie: Multiple sticky cookie values");

      let cookies_passed_to_upstream = without_sticky_cookies.join("; ");
      let cookie_passed_to_lb = sticky_cookies.first().unwrap();
      headers.remove(header::COOKIE);
      headers.insert(header::COOKIE, cookies_passed_to_upstream.parse()?);

      let sticky_cookie = StickyCookie {
        value: StickyCookieValue::try_from(cookie_passed_to_lb, expected_cookie_name)?,
        info: None,
      };
      Ok(Some(LoadBalanceContext { sticky_cookie }))
    }
  }
}

/// Set-Cookie if LB Sticky is enabled and if cookie is newly created/updated.
/// Set-Cookie response header could be in multiple lines.
/// https://developer.mozilla.org/ja/docs/Web/HTTP/Headers/Set-Cookie
pub(crate) fn set_sticky_cookie_lb_context(headers: &mut HeaderMap, context_from_lb: &LoadBalanceContext) -> Result<()> {
  let sticky_cookie_string: String = context_from_lb.sticky_cookie.clone().try_into()?;
  let new_header_val: HeaderValue = sticky_cookie_string.parse()?;
  let expected_cookie_name = &context_from_lb.sticky_cookie.value.name;
  match headers.entry(header::SET_COOKIE) {
    header::Entry::Vacant(entry) => {
      entry.insert(new_header_val);
    }
    header::Entry::Occupied(mut entry) => {
      let mut flag = false;
      for e in entry.iter_mut() {
        if e.to_str().unwrap_or("").starts_with(expected_cookie_name) {
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
