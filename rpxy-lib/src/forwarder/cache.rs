use crate::{error::*, globals::Globals, log::*};
use bytes::{Buf, Bytes, BytesMut};
use http::{Request, Response};
use http_body_util::StreamBody;
use http_cache_semantics::CachePolicy;
use lru::LruCache;
use std::{
  convert::Infallible,
  path::{Path, PathBuf},
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
  },
};
use tokio::{
  fs::{self, File},
  io::{AsyncReadExt, AsyncWriteExt},
  sync::RwLock,
};

/* ---------------------------------------------- */
#[derive(Clone, Debug)]
/// Cache main manager
pub struct RpxyCache {
  /// Inner lru cache manager storing http message caching policy
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

  /// Count cache entries
  pub async fn count(&self) -> (usize, usize, usize) {
    let total = self.inner.count();
    let file = self.file_store.count().await;
    let on_memory = total - file;
    (total, on_memory, file)
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

impl FileStore {
  /// Count file cache entries
  async fn count(&self) -> usize {
    let inner = self.inner.read().await;
    inner.cnt
  }
  /// Create a temporary file cache
  async fn create(&mut self, cache_filename: &str, body_bytes: &Bytes) -> RpxyResult<CacheFileOrOnMemory> {
    let mut inner = self.inner.write().await;
    inner.create(cache_filename, body_bytes).await
  }
  // /// Evict a temporary file cache
  // async fn evict(&self, path: impl AsRef<Path>) {
  //   // Acquire the write lock
  //   let mut inner = self.inner.write().await;
  //   if let Err(e) = inner.remove(path).await {
  //     warn!("Eviction failed during file object removal: {:?}", e);
  //   };
  // }
  // /// Read a temporary file cache
  // async fn read(&self, path: impl AsRef<Path>) -> RpxyResult<Bytes> {
  //   let inner = self.inner.read().await;
  //   inner.read(&path).await
  // }
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

  /// Create a new temporary file cache
  async fn create(&mut self, cache_filename: &str, body_bytes: &Bytes) -> RpxyResult<CacheFileOrOnMemory> {
    let cache_filepath = self.cache_dir.join(cache_filename);
    let Ok(mut file) = File::create(&cache_filepath).await else {
      return Err(RpxyError::FailedToCreateFileCache);
    };
    let mut bytes_clone = body_bytes.clone();
    while bytes_clone.has_remaining() {
      if let Err(e) = file.write_buf(&mut bytes_clone).await {
        error!("Failed to write file cache: {e}");
        return Err(RpxyError::FailedToWriteFileCache);
      };
    }
    self.cnt += 1;
    Ok(CacheFileOrOnMemory::File(cache_filepath))
  }

  /// Retrieve a stored temporary file cache
  async fn read(&self, path: impl AsRef<Path>) -> RpxyResult<()> {
    let Ok(mut file) = File::open(&path).await else {
      warn!("Cache file object cannot be opened");
      return Err(RpxyError::FailedToOpenCacheFile);
    };

    /* ----------------------------- */
    // PoC for streaming body
    use futures::channel::mpsc;
    let (tx, rx) = mpsc::unbounded::<Result<hyper::body::Frame<bytes::Bytes>, Infallible>>();

    // let (body_sender, res_body) = Body::channel();
    self.runtime_handle.spawn(async move {
      //   let mut sender = body_sender;
      let mut buf = BytesMut::new();
      loop {
        match file.read_buf(&mut buf).await {
          Ok(0) => break,
          Ok(_) => tx
            .unbounded_send(Ok(hyper::body::Frame::data(buf.copy_to_bytes(buf.remaining()))))
            .map_err(|e| anyhow::anyhow!("Failed to read cache file: {e}"))?,
          //sender.send_data(buf.copy_to_bytes(buf.remaining())).await?,
          Err(_) => break,
        };
      }
      Ok(()) as anyhow::Result<()>
    });

    let mut rx = http_body_util::StreamBody::new(rx);
    // TODO: 結局incominglikeなbodystreamを定義することになる。これだったらh3と合わせて自分で定義した方が良さそう。
    // typeが長すぎるのでwrapperを作った方がいい。
    // let response = Response::builder()
    //   .status(200)
    //   .header("content-type", "application/octet-stream")
    //   .body(rx)
    //   .unwrap();

    todo!()
    /* ----------------------------- */

    // Ok(res_body)
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
  /// Inner lru cache manager main object
  inner: Arc<Mutex<LruCache<String, CacheObject>>>, // TODO: keyはstring urlでいいのか疑問。全requestに対してcheckすることになりそう
  /// Counter of current cached object (total)
  cnt: Arc<AtomicUsize>,
}

impl LruCacheManager {
  /// Build LruCache
  fn new(cache_max_entry: usize) -> Self {
    Self {
      inner: Arc::new(Mutex::new(LruCache::new(
        std::num::NonZeroUsize::new(cache_max_entry).unwrap(),
      ))),
      cnt: Default::default(),
    }
  }

  /// Count entries
  fn count(&self) -> usize {
    self.cnt.load(Ordering::Relaxed)
  }

  /// Evict an entry
  fn evict(&self, cache_key: &str) -> Option<(String, CacheObject)> {
    let Ok(mut lock) = self.inner.lock() else {
      error!("Mutex can't be locked to evict a cache entry");
      return None;
    };
    let res = lock.pop_entry(cache_key);
    // This may be inconsistent with the actual number of entries
    self.cnt.store(lock.len(), Ordering::Relaxed);
    res
  }

  /// Push an entry
  fn push(&self, cache_key: &str, cache_object: CacheObject) -> RpxyResult<Option<(String, CacheObject)>> {
    let Ok(mut lock) = self.inner.lock() else {
      error!("Failed to acquire mutex lock for writing cache entry");
      return Err(RpxyError::FailedToAcquiredMutexLockForCache);
    };
    let res = Ok(lock.push(cache_key.to_string(), cache_object));
    // This may be inconsistent with the actual number of entries
    self.cnt.store(lock.len(), Ordering::Relaxed);
    res
  }
}

/* ---------------------------------------------- */
/// Generate cache policy if the response is cacheable
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
