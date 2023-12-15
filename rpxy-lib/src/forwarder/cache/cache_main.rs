use super::cache_error::*;
use crate::{
  globals::Globals,
  hyper_ext::body::{full, BoxBody, ResponseBody, UnboundedStreamBody},
  log::*,
};
use base64::{engine::general_purpose, Engine as _};
use bytes::{Buf, Bytes, BytesMut};
use futures::channel::mpsc;
use http::{Request, Response, Uri};
use http_body_util::{BodyExt, StreamBody};
use http_cache_semantics::CachePolicy;
use hyper::body::{Frame, Incoming};
use lru::LruCache;
use sha2::{Digest, Sha256};
use std::{
  path::{Path, PathBuf},
  sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
  },
  time::SystemTime,
};
use tokio::{
  fs::{self, File},
  io::{AsyncReadExt, AsyncWriteExt},
  sync::RwLock,
};

/* ---------------------------------------------- */
#[derive(Clone, Debug)]
/// Cache main manager
pub(crate) struct RpxyCache {
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
  /// Cache directory path
  cache_dir: PathBuf,
}

impl RpxyCache {
  /// Generate cache storage
  pub(crate) async fn new(globals: &Globals) -> Option<Self> {
    if !globals.proxy_config.cache_enabled {
      return None;
    }
    let cache_dir = globals.proxy_config.cache_dir.as_ref().unwrap();
    let file_store = FileStore::new(&globals.runtime_handle).await;
    let inner = LruCacheManager::new(globals.proxy_config.cache_max_entry);

    let max_each_size = globals.proxy_config.cache_max_each_size;
    let mut max_each_size_on_memory = globals.proxy_config.cache_max_each_size_on_memory;
    if max_each_size < max_each_size_on_memory {
      warn!(
        "Maximum size of on memory cache per entry must be smaller than or equal to the maximum of each file cache"
      );
      max_each_size_on_memory = max_each_size;
    }

    if let Err(e) = fs::remove_dir_all(cache_dir).await {
      warn!("Failed to clean up the cache dir: {e}");
    };
    fs::create_dir_all(&cache_dir).await.unwrap();

    Some(Self {
      file_store,
      inner,
      runtime_handle: globals.runtime_handle.clone(),
      max_each_size,
      max_each_size_on_memory,
      cache_dir: cache_dir.clone(),
    })
  }

  /// Count cache entries
  pub(crate) async fn count(&self) -> (usize, usize, usize) {
    let total = self.inner.count();
    let file = self.file_store.count().await;
    let on_memory = total - file;
    (total, on_memory, file)
  }

  /// Put response into the cache
  pub(crate) async fn put(
    &self,
    uri: &hyper::Uri,
    mut body: Incoming,
    policy: &CachePolicy,
  ) -> CacheResult<UnboundedStreamBody> {
    let cache_manager = self.inner.clone();
    let mut file_store = self.file_store.clone();
    let uri = uri.clone();
    let policy_clone = policy.clone();
    let max_each_size = self.max_each_size;
    let max_each_size_on_memory = self.max_each_size_on_memory;
    let cache_dir = self.cache_dir.clone();

    let (body_tx, body_rx) = mpsc::unbounded::<Result<Frame<Bytes>, hyper::Error>>();

    self.runtime_handle.spawn(async move {
      let mut size = 0usize;
      let mut buf = BytesMut::new();

      loop {
        let frame = match body.frame().await {
          Some(frame) => frame,
          None => {
            debug!("Response body finished");
            break;
          }
        };
        let frame_size = frame.as_ref().map(|f| {
          if f.is_data() {
            f.data_ref().map(|bytes| bytes.remaining()).unwrap_or_default()
          } else {
            0
          }
        });
        size += frame_size.unwrap_or_default();

        // check size
        if size > max_each_size {
          warn!("Too large to cache");
          return Err(CacheError::TooLargeToCache);
        }
        frame
          .as_ref()
          .map(|f| {
            if f.is_data() {
              let data_bytes = f.data_ref().unwrap().clone();
              debug!("cache data bytes of {} bytes", data_bytes.len());
              // We do not use stream-type buffering since it needs to lock file during operation.
              buf.extend(data_bytes.as_ref());
            }
          })
          .map_err(|e| CacheError::FailedToCacheBytes(e.to_string()))?;

        // send data to use response downstream
        body_tx
          .unbounded_send(frame)
          .map_err(|e| CacheError::FailedToSendFrameToCache(e.to_string()))?;
      }

      let buf = buf.freeze();
      // Calculate hash of the cached data, after all data is received.
      // In-operation calculation is possible but it blocks sending data.
      let mut hasher = Sha256::new();
      hasher.update(buf.as_ref());
      let hash_bytes = Bytes::copy_from_slice(hasher.finalize().as_ref());
      debug!("Cached data: {} bytes, hash = {:?}", size, hash_bytes);

      // Create cache object
      let cache_key = derive_cache_key_from_uri(&uri);
      let cache_object = CacheObject {
        policy: policy_clone,
        target: CacheFileOrOnMemory::build(&cache_dir, &uri, &buf, max_each_size_on_memory),
        hash: hash_bytes,
      };

      if let Some((k, v)) = cache_manager.push(&cache_key, &cache_object)? {
        if k != cache_key {
          info!("Over the cache capacity. Evict least recent used entry");
          if let CacheFileOrOnMemory::File(path) = v.target {
            file_store.evict(&path).await;
          }
        }
      }
      // store cache object to file
      if let CacheFileOrOnMemory::File(_) = cache_object.target {
        file_store.create(&cache_object, &buf).await?;
      }

      Ok(()) as CacheResult<()>
    });

    let stream_body = StreamBody::new(body_rx);

    Ok(stream_body)
  }

  /// Get cached response
  pub(crate) async fn get<R>(&self, req: &Request<R>) -> Option<Response<ResponseBody>> {
    debug!(
      "Current cache status: (total, on-memory, file) = {:?}",
      self.count().await
    );
    let cache_key = derive_cache_key_from_uri(req.uri());

    // First check cache chance
    let Ok(Some(cached_object)) = self.inner.get(&cache_key) else {
      return None;
    };

    // Secondly check the cache freshness as an HTTP message
    let now = SystemTime::now();
    let http_cache_semantics::BeforeRequest::Fresh(res_parts) = cached_object.policy.before_request(req, now) else {
      // Evict stale cache entry.
      // This might be okay to keep as is since it would be updated later.
      // However, there is no guarantee that newly got objects will be still cacheable.
      // So, we have to evict stale cache entries and cache file objects if found.
      debug!("Stale cache entry: {cache_key}");
      let _evicted_entry = self.inner.evict(&cache_key);
      // For cache file
      if let CacheFileOrOnMemory::File(path) = &cached_object.target {
        self.file_store.evict(&path).await;
      }
      return None;
    };

    // Finally retrieve the file/on-memory object
    let response_body = match cached_object.target {
      CacheFileOrOnMemory::File(path) => {
        let stream_body = match self.file_store.read(path.clone(), &cached_object.hash).await {
          Ok(s) => s,
          Err(e) => {
            warn!("Failed to read from file cache: {e}");
            let _evicted_entry = self.inner.evict(&cache_key);
            self.file_store.evict(path).await;
            return None;
          }
        };
        debug!("Cache hit from file: {cache_key}");
        ResponseBody::Streamed(stream_body)
      }
      CacheFileOrOnMemory::OnMemory(object) => {
        debug!("Cache hit from on memory: {cache_key}");
        let mut hasher = Sha256::new();
        hasher.update(object.as_ref());
        let hash_bytes = Bytes::copy_from_slice(hasher.finalize().as_ref());
        if hash_bytes != cached_object.hash {
          warn!("Hash mismatched. Cache object is corrupted");
          let _evicted_entry = self.inner.evict(&cache_key);
          return None;
        }
        ResponseBody::Boxed(BoxBody::new(full(object)))
      }
    };
    Some(Response::from_parts(res_parts, response_body))
  }
}

/* ---------------------------------------------- */
#[derive(Debug, Clone)]
/// Cache file manager outer that is responsible to handle `RwLock`
struct FileStore {
  /// Inner file store main object
  inner: Arc<RwLock<FileStoreInner>>,
}
impl FileStore {
  /// Build manager
  async fn new(runtime_handle: &tokio::runtime::Handle) -> Self {
    Self {
      inner: Arc::new(RwLock::new(FileStoreInner::new(runtime_handle).await)),
    }
  }

  /// Count file cache entries
  async fn count(&self) -> usize {
    let inner = self.inner.read().await;
    inner.cnt
  }
  /// Create a temporary file cache
  async fn create(&mut self, cache_object: &CacheObject, body_bytes: &Bytes) -> CacheResult<()> {
    let mut inner = self.inner.write().await;
    inner.create(cache_object, body_bytes).await
  }
  /// Evict a temporary file cache
  async fn evict(&self, path: impl AsRef<Path>) {
    // Acquire the write lock
    let mut inner = self.inner.write().await;
    if let Err(e) = inner.remove(path).await {
      warn!("Eviction failed during file object removal: {:?}", e);
    };
  }
  /// Read a temporary file cache
  async fn read(
    &self,
    path: impl AsRef<Path> + Send + Sync + 'static,
    hash: &Bytes,
  ) -> CacheResult<UnboundedStreamBody> {
    let inner = self.inner.read().await;
    inner.read(path, hash).await
  }
}

#[derive(Debug, Clone)]
/// Manager inner for cache on file system
struct FileStoreInner {
  /// Counter of current cached files
  cnt: usize,
  /// Async runtime
  runtime_handle: tokio::runtime::Handle,
}

impl FileStoreInner {
  /// Build new cache file manager.
  /// This first creates cache file dir if not exists, and cleans up the file inside the directory.
  /// TODO: Persistent cache is really difficult. `sqlite` or something like that is needed.
  async fn new(runtime_handle: &tokio::runtime::Handle) -> Self {
    Self {
      cnt: 0,
      runtime_handle: runtime_handle.clone(),
    }
  }

  /// Create a new temporary file cache
  async fn create(&mut self, cache_object: &CacheObject, body_bytes: &Bytes) -> CacheResult<()> {
    let cache_filepath = match cache_object.target {
      CacheFileOrOnMemory::File(ref path) => path.clone(),
      CacheFileOrOnMemory::OnMemory(_) => {
        return Err(CacheError::InvalidCacheTarget);
      }
    };
    let Ok(mut file) = File::create(&cache_filepath).await else {
      return Err(CacheError::FailedToCreateFileCache);
    };
    let mut bytes_clone = body_bytes.clone();
    while bytes_clone.has_remaining() {
      if let Err(e) = file.write_buf(&mut bytes_clone).await {
        error!("Failed to write file cache: {e}");
        return Err(CacheError::FailedToWriteFileCache);
      };
    }
    self.cnt += 1;
    Ok(())
  }

  /// Retrieve a stored temporary file cache
  async fn read(
    &self,
    path: impl AsRef<Path> + Send + Sync + 'static,
    hash: &Bytes,
  ) -> CacheResult<UnboundedStreamBody> {
    let Ok(mut file) = File::open(&path).await else {
      warn!("Cache file object cannot be opened");
      return Err(CacheError::FailedToOpenCacheFile);
    };
    let hash_clone = hash.clone();
    let mut self_clone = self.clone();

    let (body_tx, body_rx) = mpsc::unbounded::<Result<Frame<Bytes>, hyper::Error>>();

    self.runtime_handle.spawn(async move {
      let mut hasher = Sha256::new();
      let mut buf = BytesMut::new();
      loop {
        match file.read_buf(&mut buf).await {
          Ok(0) => break,
          Ok(_) => {
            let bytes = buf.copy_to_bytes(buf.remaining());
            hasher.update(bytes.as_ref());
            body_tx
              .unbounded_send(Ok(Frame::data(bytes)))
              .map_err(|e| CacheError::FailedToSendFrameFromCache(e.to_string()))?
          }
          Err(_) => break,
        };
      }
      let hash_bytes = Bytes::copy_from_slice(hasher.finalize().as_ref());
      if hash_bytes != hash_clone {
        warn!("Hash mismatched. Cache object is corrupted. Force to remove the cache file.");
        // only file can be evicted
        let _evicted_entry = self_clone.remove(&path).await;
        return Err(CacheError::HashMismatchedInCacheFile);
      }
      Ok(()) as CacheResult<()>
    });

    let stream_body = StreamBody::new(body_rx);

    Ok(stream_body)
  }

  /// Remove file
  async fn remove(&mut self, path: impl AsRef<Path>) -> CacheResult<()> {
    fs::remove_file(path.as_ref())
      .await
      .map_err(|e| CacheError::FailedToRemoveCacheFile(e.to_string()))?;
    self.cnt -= 1;
    debug!("Removed a cache file at {:?} (file count: {})", path.as_ref(), self.cnt);

    Ok(())
  }
}

/* ---------------------------------------------- */

#[derive(Clone, Debug)]
/// Cache target in hybrid manner of on-memory and file system
pub(crate) enum CacheFileOrOnMemory {
  /// Pointer to the temporary cache file
  File(PathBuf),
  /// Cached body itself
  OnMemory(Bytes),
}

impl CacheFileOrOnMemory {
  /// Get cache object target
  fn build(cache_dir: &Path, uri: &Uri, object: &Bytes, max_each_size_on_memory: usize) -> Self {
    if object.len() > max_each_size_on_memory {
      let cache_filename = derive_filename_from_uri(uri);
      let cache_filepath = cache_dir.join(cache_filename);
      CacheFileOrOnMemory::File(cache_filepath)
    } else {
      CacheFileOrOnMemory::OnMemory(object.clone())
    }
  }
}

#[derive(Clone, Debug)]
/// Cache object definition
struct CacheObject {
  /// Cache policy to determine if the stored cache can be used as a response to a new incoming request
  policy: CachePolicy,
  /// Cache target: on-memory object or temporary file
  target: CacheFileOrOnMemory,
  /// SHA256 hash of target to strongly bind the cache metadata (this object) and file target
  hash: Bytes,
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
  fn push(&self, cache_key: &str, cache_object: &CacheObject) -> CacheResult<Option<(String, CacheObject)>> {
    let Ok(mut lock) = self.inner.lock() else {
      error!("Failed to acquire mutex lock for writing cache entry");
      return Err(CacheError::FailedToAcquiredMutexLockForCache);
    };
    let res = Ok(lock.push(cache_key.to_string(), cache_object.clone()));
    // This may be inconsistent with the actual number of entries
    self.cnt.store(lock.len(), Ordering::Relaxed);
    res
  }

  /// Get an entry
  fn get(&self, cache_key: &str) -> CacheResult<Option<CacheObject>> {
    let Ok(mut lock) = self.inner.lock() else {
      error!("Mutex can't be locked for checking cache entry");
      return Err(CacheError::FailedToAcquiredMutexLockForCheck);
    };
    let Some(cached_object) = lock.get(cache_key) else {
      return Ok(None);
    };
    Ok(Some(cached_object.clone()))
  }
}

/* ---------------------------------------------- */
/// Generate cache policy if the response is cacheable
pub(crate) fn get_policy_if_cacheable<B1, B2>(
  req: Option<&Request<B1>>,
  res: Option<&Response<B2>>,
) -> CacheResult<Option<CachePolicy>>
// where
//   B1: core::fmt::Debug,
{
  // deduce cache policy from req and res
  let (Some(req), Some(res)) = (req, res) else {
    return Err(CacheError::NullRequestOrResponse);
  };

  let new_policy = CachePolicy::new(req, res);
  if new_policy.is_storable() {
    // debug!("Response is cacheable: {:?}\n{:?}", req, res.headers());
    Ok(Some(new_policy))
  } else {
    Ok(None)
  }
}

fn derive_filename_from_uri(uri: &hyper::Uri) -> String {
  let mut hasher = Sha256::new();
  hasher.update(uri.to_string());
  let digest = hasher.finalize();
  general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn derive_cache_key_from_uri(uri: &hyper::Uri) -> String {
  uri.to_string()
}
