use crate::{error::*, globals::Globals, log::*};
use http::{Request, Response};
use http_cache_semantics::CachePolicy;
use lru::LruCache;
use std::{
  path::{Path, PathBuf},
  sync::{atomic::AtomicUsize, Arc, Mutex},
};
use tokio::{fs, sync::RwLock};

/* ---------------------------------------------- */
#[derive(Clone, Debug)]
pub struct RpxyCache {
  /// Lru cache storing http message caching policy
  inner: LruCacheManager,
  /// Managing cache file objects through RwLock's lock mechanism for file lock
  file_store: FileStore,
  /// Async runtime
  runtime_handle: tokio::runtime::Handle,
  /// Maximum size of each cache file object
  max_each_size: usize,
  /// Maximum size of cache object on memory
  max_each_size_on_memory: usize,
}

impl RpxyCache {
  /// Generate cache storage
  pub async fn new(globals: &Globals) -> Option<Self> {
    if !globals.proxy_config.cache_enabled {
      return None;
    }
    let path = globals.proxy_config.cache_dir.as_ref().unwrap();
    let file_store = FileStore::new(path, &globals.runtime_handle).await;
    let inner = LruCacheManager::new(globals.proxy_config.cache_max_entry);

    let max_each_size = globals.proxy_config.cache_max_each_size;
    let mut max_each_size_on_memory = globals.proxy_config.cache_max_each_size_on_memory;
    if max_each_size < max_each_size_on_memory {
      warn!(
        "Maximum size of on memory cache per entry must be smaller than or equal to the maximum of each file cache"
      );
      max_each_size_on_memory = max_each_size;
    }

    Some(Self {
      file_store,
      inner,
      runtime_handle: globals.runtime_handle.clone(),
      max_each_size,
      max_each_size_on_memory,
    })
  }
}

/* ---------------------------------------------- */
#[derive(Debug, Clone)]
/// Cache file manager outer that is responsible to handle `RwLock`
struct FileStore {
  inner: Arc<RwLock<FileStoreInner>>,
}
impl FileStore {
  /// Build manager
  async fn new(path: impl AsRef<Path>, runtime_handle: &tokio::runtime::Handle) -> Self {
    Self {
      inner: Arc::new(RwLock::new(FileStoreInner::new(path, runtime_handle).await)),
    }
  }
}

#[derive(Debug)]
/// Manager inner for cache on file system
struct FileStoreInner {
  /// Directory of temporary files
  cache_dir: PathBuf,
  /// Counter of current cached files
  cnt: usize,
  /// Async runtime
  runtime_handle: tokio::runtime::Handle,
}

impl FileStoreInner {
  /// Build new cache file manager.
  /// This first creates cache file dir if not exists, and cleans up the file inside the directory.
  /// TODO: Persistent cache is really difficult. `sqlite` or something like that is needed.
  async fn new(path: impl AsRef<Path>, runtime_handle: &tokio::runtime::Handle) -> Self {
    let path_buf = path.as_ref().to_path_buf();
    if let Err(e) = fs::remove_dir_all(path).await {
      warn!("Failed to clean up the cache dir: {e}");
    };
    fs::create_dir_all(&path_buf).await.unwrap();
    Self {
      cache_dir: path_buf.clone(),
      cnt: 0,
      runtime_handle: runtime_handle.clone(),
    }
  }
}

/* ---------------------------------------------- */

#[derive(Clone, Debug)]
/// Cache target in hybrid manner of on-memory and file system
pub enum CacheFileOrOnMemory {
  /// Pointer to the temporary cache file
  File(PathBuf),
  /// Cached body itself
  OnMemory(Vec<u8>),
}

#[derive(Clone, Debug)]
/// Cache object definition
struct CacheObject {
  /// Cache policy to determine if the stored cache can be used as a response to a new incoming request
  pub policy: CachePolicy,
  /// Cache target: on-memory object or temporary file
  pub target: CacheFileOrOnMemory,
  /// SHA256 hash of target to strongly bind the cache metadata (this object) and file target
  pub hash: Vec<u8>,
}

/* ---------------------------------------------- */
#[derive(Debug, Clone)]
/// Lru cache manager that is responsible to handle `Mutex` as an outer of `LruCache`
struct LruCacheManager {
  inner: Arc<Mutex<LruCache<String, CacheObject>>>, // TODO: keyはstring urlでいいのか疑問。全requestに対してcheckすることになりそう
  cnt: Arc<AtomicUsize>,
}

impl LruCacheManager {
  /// Build LruCache
  fn new(cache_max_entry: usize) -> Self {
    Self {
      inner: Arc::new(Mutex::new(LruCache::new(
        std::num::NonZeroUsize::new(cache_max_entry).unwrap(),
      ))),
      cnt: Arc::new(AtomicUsize::default()),
    }
  }
}

/* ---------------------------------------------- */
pub fn get_policy_if_cacheable<B1, B2>(
  req: Option<&Request<B1>>,
  res: Option<&Response<B2>>,
) -> RpxyResult<Option<CachePolicy>>
// where
//   B1: core::fmt::Debug,
{
  // deduce cache policy from req and res
  let (Some(req), Some(res)) = (req, res) else {
    return Err(RpxyError::NullRequestOrResponse);
  };

  let new_policy = CachePolicy::new(req, res);
  if new_policy.is_storable() {
    // debug!("Response is cacheable: {:?}\n{:?}", req, res.headers());
    Ok(Some(new_policy))
  } else {
    Ok(None)
  }
}
