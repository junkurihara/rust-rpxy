mod handler_main;
mod utils_headers;
mod utils_request;
mod utils_synth_response;

pub use handler_main::{HttpMessageHandler, HttpMessageHandlerBuilder, HttpMessageHandlerBuilderError};

use crate::backend::LbContext;

#[derive(Debug)]
struct HandlerContext {
  context_lb: Option<LbContext>,
}
