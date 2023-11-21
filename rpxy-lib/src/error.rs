pub use anyhow::{anyhow, bail, ensure, Context};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, RpxyError>;

/// Describes things that can go wrong in the Rpxy
#[derive(Debug, Error)]
pub enum RpxyError {}
