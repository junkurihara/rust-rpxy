mod client;

use crate::hyper_ext::body::{IncomingLike, IncomingOr};
use hyper_tls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
pub type Forwarder = client::Forwarder<HttpsConnector<HttpConnector>, IncomingOr<IncomingLike>>;

pub use client::ForwardRequest;
