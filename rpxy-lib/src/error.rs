pub use anyhow::{anyhow, bail, ensure, Context};
use std::io;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, RpxyError>;

/// Describes things that can go wrong in the Rpxy
#[derive(Debug, Error)]
pub enum RpxyError {
  #[error("Proxy build error: {0}")]
  ProxyBuild(#[from] crate::proxy::ProxyBuilderError),

  #[error("Backend build error: {0}")]
  BackendBuild(#[from] crate::backend::BackendBuilderError),

  #[error("MessageHandler build error: {0}")]
  HandlerBuild(#[from] crate::handler::HttpMessageHandlerBuilderError),

  #[error("Config builder error: {0}")]
  ConfigBuild(&'static str),

  #[error("Http Message Handler Error: {0}")]
  Handler(&'static str),

  #[error("Cache Error: {0}")]
  Cache(&'static str),

  #[error("Http Request Message Error: {0}")]
  Request(&'static str),

  #[error("TCP/UDP Proxy Layer Error: {0}")]
  Proxy(String),

  #[allow(unused)]
  #[error("LoadBalance Layer Error: {0}")]
  LoadBalance(String),

  #[error("I/O Error: {0}")]
  Io(#[from] io::Error),

  // #[error("Toml Deserialization Error")]
  // TomlDe(#[from] toml::de::Error),
  #[cfg(feature = "http3-quinn")]
  #[error("Quic Connection Error [quinn]: {0}")]
  QuicConn(#[from] quinn::ConnectionError),

  #[cfg(feature = "http3-s2n")]
  #[error("Quic Connection Error [s2n-quic]: {0}")]
  QUicConn(#[from] s2n_quic::connection::Error),

  #[cfg(feature = "http3-quinn")]
  #[error("H3 Error [quinn]: {0}")]
  H3(#[from] h3::Error),

  #[cfg(feature = "http3-s2n")]
  #[error("H3 Error [s2n-quic]: {0}")]
  H3(#[from] s2n_quic_h3::h3::Error),

  #[error("rustls Connection Error: {0}")]
  Rustls(#[from] rustls::Error),

  #[error("Hyper Error: {0}")]
  Hyper(#[from] hyper::Error),

  #[error("Hyper Http Error: {0}")]
  HyperHttp(#[from] hyper::http::Error),

  #[error("Hyper Http HeaderValue Error: {0}")]
  HyperHeaderValue(#[from] hyper::header::InvalidHeaderValue),

  #[error("Hyper Http HeaderName Error: {0}")]
  HyperHeaderName(#[from] hyper::header::InvalidHeaderName),

  #[error(transparent)]
  Other(#[from] anyhow::Error),
}

#[allow(dead_code)]
#[derive(Debug, Error, Clone)]
pub enum ClientCertsError {
  #[error("TLS Client Certificate is Required for Given SNI: {0}")]
  ClientCertRequired(String),

  #[error("Inconsistent TLS Client Certificate for Given SNI: {0}")]
  InconsistentClientCert(String),
}
