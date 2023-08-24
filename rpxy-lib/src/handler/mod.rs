#[cfg(feature = "cache")]
mod cache;
mod forwarder;
mod handler_main;
mod utils_headers;
mod utils_request;
mod utils_synth_response;

#[cfg(feature = "sticky-cookie")]
use crate::backend::LbContext;
pub use {
  forwarder::Forwarder,
  handler_main::{HttpMessageHandler, HttpMessageHandlerBuilder, HttpMessageHandlerBuilderError},
};

#[allow(dead_code)]
#[derive(Debug)]
/// Context object to handle sticky cookies at HTTP message handler
struct HandlerContext {
  #[cfg(feature = "sticky-cookie")]
  context_lb: Option<LbContext>,
  #[cfg(not(feature = "sticky-cookie"))]
  context_lb: Option<()>,
}
