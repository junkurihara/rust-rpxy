mod common;
mod forwarding;
mod hop;
#[cfg(feature = "sticky-cookie")]
mod sticky_cookie;
mod upstream;

pub(super) use common::{add_header_entry_overwrite_if_exist, host_from_uri_or_host_header};
pub(super) use forwarding::add_forwarding_header;
pub(super) use hop::{extract_upgrade, remove_connection_header, remove_hop_header};
#[cfg(feature = "sticky-cookie")]
pub(super) use sticky_cookie::{set_sticky_cookie_lb_context, takeout_sticky_cookie_lb_context};
pub(super) use upstream::{apply_default_app_fallback_rewrite, apply_upstream_options_to_header};
