use thiserror::Error;

pub type CacheResult<T> = std::result::Result<T, CacheError>;

/// Describes things that can go wrong in the Rpxy
#[derive(Debug, Error)]
pub enum CacheError {
  // Cache errors,
  #[error("Invalid null request and/or response")]
  NullRequestOrResponse,

  #[error("Failed to write byte buffer")]
  FailedToWriteByteBufferForCache,

  #[error("Failed to acquire mutex lock for cache")]
  FailedToAcquiredMutexLockForCache,

  #[error("Failed to acquire mutex lock for check")]
  FailedToAcquiredMutexLockForCheck,

  #[error("Failed to create file cache")]
  FailedToCreateFileCache,

  #[error("Failed to write file cache")]
  FailedToWriteFileCache,

  #[error("Failed to open cache file")]
  FailedToOpenCacheFile,

  #[error("Too large to cache")]
  TooLargeToCache,

  #[error("Failed to cache bytes: {0}")]
  FailedToCacheBytes(String),

  #[error("Failed to send frame to cache {0}")]
  FailedToSendFrameToCache(String),

  #[error("Failed to send frame from file cache {0}")]
  FailedToSendFrameFromCache(String),

  #[error("Failed to remove cache file: {0}")]
  FailedToRemoveCacheFile(String),

  #[error("Invalid cache target")]
  InvalidCacheTarget,
}
