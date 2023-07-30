pub use anyhow::{anyhow, bail, ensure, Context};
use std::io;
use thiserror::Error;

pub type Result<T> = std::result::Result<T, RpxyError>;

/// Describes things that can go wrong in the Rpxy
#[derive(Debug, Error)]
pub enum RpxyError {
  #[error("Proxy build error")]
  ProxyBuild(#[from] crate::proxy::ProxyBuilderError),

  #[error("Backend build error")]
  BackendBuild(#[from] crate::backend::BackendBuilderError),

  #[error("MessageHandler build error")]
  HandlerBuild(#[from] crate::handler::HttpMessageHandlerBuilderError),

  #[error("Config builder error: {0}")]
  ConfigBuild(&'static str),

  #[error("Http Message Handler Error: {0}")]
  Handler(&'static str),

  #[error("Http Request Message Error: {0}")]
  Request(&'static str),

  #[error("TCP/UDP Proxy Layer Error: {0}")]
  Proxy(String),

  #[allow(unused)]
  #[error("LoadBalance Layer Error: {0}")]
  LoadBalance(String),

  #[error("I/O Error")]
  Io(#[from] io::Error),

  // #[error("Toml Deserialization Error")]
  // TomlDe(#[from] toml::de::Error),
  #[cfg(feature = "http3-quinn")]
  #[error("Quic Connection Error")]
  QuicConn(#[from] quinn::ConnectionError),

  #[cfg(feature = "http3-s2n")]
  #[error("Quic Connection Error [s2n-quic]")]
  QUicConn(#[from] s2n_quic::connection::Error),

  #[cfg(feature = "http3-quinn")]
  #[error("H3 Error")]
  H3(#[from] h3::Error),

  #[cfg(feature = "http3-s2n")]
  #[error("H3 Error [s2n-quic]")]
  H3(#[from] s2n_quic_h3::h3::Error),

  #[error("rustls Connection Error")]
  Rustls(#[from] rustls::Error),

  #[error("Hyper Error")]
  Hyper(#[from] hyper::Error),

  #[error("Hyper Http Error")]
  HyperHttp(#[from] hyper::http::Error),

  #[error("Hyper Http HeaderValue Error")]
  HyperHeaderValue(#[from] hyper::header::InvalidHeaderValue),

  #[error("Hyper Http HeaderName Error")]
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
