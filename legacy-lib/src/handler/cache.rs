use crate::{error::*, globals::Globals, log::*, CryptoSource};
use base64::{engine::general_purpose, Engine as _};
use bytes::{Buf, Bytes, BytesMut};
use http_cache_semantics::CachePolicy;
use hyper::{
  http::{Request, Response},
  Body,
};
use lru::LruCache;
use sha2::{Digest, Sha256};
use std::{
  fmt::Debug,
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
}

#[derive(Debug)]
/// Manager inner for cache on file system
struct CacheFileManagerInner {
  /// Directory of temporary files
  cache_dir: PathBuf,
  /// Counter of current cached files
  cnt: usize,
  /// Async runtime
  runtime_handle: tokio::runtime::Handle,
}

impl CacheFileManagerInner {
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
  async fn create(&mut self, cache_filename: &str, body_bytes: &Bytes) -> Result<CacheFileOrOnMemory> {
    let cache_filepath = self.cache_dir.join(cache_filename);
    let Ok(mut file) = File::create(&cache_filepath).await else {
      return Err(RpxyError::Cache("Failed to create file"));
    };
    let mut bytes_clone = body_bytes.clone();
    while bytes_clone.has_remaining() {
      if let Err(e) = file.write_buf(&mut bytes_clone).await {
        error!("Failed to write file cache: {e}");
        return Err(RpxyError::Cache("Failed to write file cache: {e}"));
      };
    }
    self.cnt += 1;
    Ok(CacheFileOrOnMemory::File(cache_filepath))
  }

  /// Retrieve a stored temporary file cache
  async fn read(&self, path: impl AsRef<Path>) -> Result<Body> {
    let Ok(mut file) = File::open(&path).await else {
      warn!("Cache file object cannot be opened");
      return Err(RpxyError::Cache("Cache file object cannot be opened"));
    };
    let (body_sender, res_body) = Body::channel();
    self.runtime_handle.spawn(async move {
      let mut sender = body_sender;
      let mut buf = BytesMut::new();
      loop {
        match file.read_buf(&mut buf).await {
          Ok(0) => break,
          Ok(_) => sender.send_data(buf.copy_to_bytes(buf.remaining())).await?,
          Err(_) => break,
        };
      }
      Ok(()) as Result<()>
    });

    Ok(res_body)
  }

  /// Remove file
  async fn remove(&mut self, path: impl AsRef<Path>) -> Result<()> {
    fs::remove_file(path.as_ref()).await?;
    self.cnt -= 1;
    debug!("Removed a cache file at {:?} (file count: {})", path.as_ref(), self.cnt);

    Ok(())
  }
}

#[derive(Debug, Clone)]
/// Cache file manager outer that is responsible to handle `RwLock`
struct CacheFileManager {
  inner: Arc<RwLock<CacheFileManagerInner>>,
}

impl CacheFileManager {
  /// Build manager
  async fn new(path: impl AsRef<Path>, runtime_handle: &tokio::runtime::Handle) -> Self {
    Self {
      inner: Arc::new(RwLock::new(CacheFileManagerInner::new(path, runtime_handle).await)),
    }
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
  async fn read(&self, path: impl AsRef<Path>) -> Result<Body> {
    let mgr = self.inner.read().await;
    mgr.read(&path).await
  }
  /// Create a temporary file cache
  async fn create(&mut self, cache_filename: &str, body_bytes: &Bytes) -> Result<CacheFileOrOnMemory> {
    let mut mgr = self.inner.write().await;
    mgr.create(cache_filename, body_bytes).await
  }
  async fn count(&self) -> usize {
    let mgr = self.inner.read().await;
    mgr.cnt
  }
}

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
    self.cnt.store(lock.len(), Ordering::Relaxed);
    res
  }
  /// Get an entry
  fn get(&self, cache_key: &str) -> Result<Option<CacheObject>> {
    let Ok(mut lock) = self.inner.lock() else {
      error!("Mutex can't be locked for checking cache entry");
      return Err(RpxyError::Cache("Mutex can't be locked for checking cache entry"));
    };
    let Some(cached_object) = lock.get(cache_key) else {
      return Ok(None);
    };
    Ok(Some(cached_object.clone()))
  }
  /// Push an entry
  fn push(&self, cache_key: &str, cache_object: CacheObject) -> Result<Option<(String, CacheObject)>> {
    let Ok(mut lock) = self.inner.lock() else {
      error!("Failed to acquire mutex lock for writing cache entry");
      return Err(RpxyError::Cache("Failed to acquire mutex lock for writing cache entry"));
    };
    let res = Ok(lock.push(cache_key.to_string(), cache_object));
    self.cnt.store(lock.len(), Ordering::Relaxed);
    res
  }
}

#[derive(Clone, Debug)]
pub struct RpxyCache {
  /// Managing cache file objects through RwLock's lock mechanism for file lock
  cache_file_manager: CacheFileManager,
  /// Lru cache storing http message caching policy
  inner: LruCacheManager,
  /// Async runtime
  runtime_handle: tokio::runtime::Handle,
  /// Maximum size of each cache file object
  max_each_size: usize,
  /// Maximum size of cache object on memory
  max_each_size_on_memory: usize,
}

impl RpxyCache {
  /// Generate cache storage
  pub async fn new<T: CryptoSource>(globals: &Globals<T>) -> Option<Self> {
    if !globals.proxy_config.cache_enabled {
      return None;
    }

    let path = globals.proxy_config.cache_dir.as_ref().unwrap();
    let cache_file_manager = CacheFileManager::new(path, &globals.runtime_handle).await;
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
      cache_file_manager,
      inner,
      runtime_handle: globals.runtime_handle.clone(),
      max_each_size,
      max_each_size_on_memory,
    })
  }

  /// Count cache entries
  pub async fn count(&self) -> (usize, usize, usize) {
    let total = self.inner.count();
    let file = self.cache_file_manager.count().await;
    let on_memory = total - file;
    (total, on_memory, file)
  }

  /// Get cached response
  pub async fn get<R>(&self, req: &Request<R>) -> Option<Response<Body>> {
    debug!(
      "Current cache status: (total, on-memory, file) = {:?}",
      self.count().await
    );
    let cache_key = req.uri().to_string();

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
        self.cache_file_manager.evict(&path).await;
      }
      return None;
    };

    // Finally retrieve the file/on-memory object
    match cached_object.target {
      CacheFileOrOnMemory::File(path) => {
        let res_body = match self.cache_file_manager.read(&path).await {
          Ok(res_body) => res_body,
          Err(e) => {
            warn!("Failed to read from file cache: {e}");
            let _evicted_entry = self.inner.evict(&cache_key);
            self.cache_file_manager.evict(&path).await;
            return None;
          }
        };

        debug!("Cache hit from file: {cache_key}");
        Some(Response::from_parts(res_parts, res_body))
      }
      CacheFileOrOnMemory::OnMemory(object) => {
        debug!("Cache hit from on memory: {cache_key}");
        Some(Response::from_parts(res_parts, Body::from(object)))
      }
    }
  }

  /// Put response into the cache
  pub async fn put(&self, uri: &hyper::Uri, body_bytes: &Bytes, policy: &CachePolicy) -> Result<()> {
    let my_cache = self.inner.clone();
    let mut mgr = self.cache_file_manager.clone();
    let uri = uri.clone();
    let bytes_clone = body_bytes.clone();
    let policy_clone = policy.clone();
    let max_each_size = self.max_each_size;
    let max_each_size_on_memory = self.max_each_size_on_memory;

    self.runtime_handle.spawn(async move {
      if bytes_clone.len() > max_each_size {
        warn!("Too large to cache");
        return Err(RpxyError::Cache("Too large to cache"));
      }
      let cache_key = derive_cache_key_from_uri(&uri);

      debug!("Object of size {:?} bytes to be cached", bytes_clone.len());

      let cache_object = if bytes_clone.len() > max_each_size_on_memory {
        let cache_filename = derive_filename_from_uri(&uri);
        let target = mgr.create(&cache_filename, &bytes_clone).await?;
        debug!("Cached a new cache file: {} - {}", cache_key, cache_filename);
        CacheObject {
          policy: policy_clone,
          target,
        }
      } else {
        debug!("Cached a new object on memory: {}", cache_key);
        CacheObject {
          policy: policy_clone,
          target: CacheFileOrOnMemory::OnMemory(bytes_clone.to_vec()),
        }
      };

      if let Some((k, v)) = my_cache.push(&cache_key, cache_object)? {
        if k != cache_key {
          info!("Over the cache capacity. Evict least recent used entry");
          if let CacheFileOrOnMemory::File(path) = v.target {
            mgr.evict(&path).await;
          }
        }
      }
      Ok(())
    });

    Ok(())
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

pub fn get_policy_if_cacheable<R>(req: Option<&Request<R>>, res: Option<&Response<Body>>) -> Result<Option<CachePolicy>>
where
  R: Debug,
{
  // deduce cache policy from req and res
  let (Some(req), Some(res)) = (req, res) else {
      return Err(RpxyError::Cache("Invalid null request and/or response"));
    };

  let new_policy = CachePolicy::new(req, res);
  if new_policy.is_storable() {
    // debug!("Response is cacheable: {:?}\n{:?}", req, res.headers());
    Ok(Some(new_policy))
  } else {
    Ok(None)
  }
}
