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
  sync::{Arc, Mutex},
  time::SystemTime,
};
use tokio::{
  fs::{self, File},
  io::{AsyncReadExt, AsyncWriteExt},
  sync::RwLock,
};

#[derive(Clone, Debug)]
struct CacheObject {
  pub policy: CachePolicy,
  pub target: PathBuf,
}

#[derive(Debug)]
struct CacheFileManager {
  cache_dir: PathBuf,
  cnt: usize,
  runtime_handle: tokio::runtime::Handle,
}

impl CacheFileManager {
  async fn new(path: &PathBuf, runtime_handle: &tokio::runtime::Handle) -> Self {
    // Create cache file dir
    // Clean up the file dir before init
    // TODO: Persistent cache is really difficult. maybe SQLite is needed.
    if let Err(e) = fs::remove_dir_all(path).await {
      warn!("Failed to clean up the cache dir: {e}");
    };
    fs::create_dir_all(path).await.unwrap();
    Self {
      cache_dir: path.clone(),
      cnt: 0,
      runtime_handle: runtime_handle.clone(),
    }
  }

  async fn create(&mut self, cache_filename: &str, body_bytes: &Bytes, policy: &CachePolicy) -> Result<CacheObject> {
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
    Ok(CacheObject {
      policy: policy.clone(),
      target: cache_filepath,
    })
  }

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

  async fn remove(&mut self, path: impl AsRef<Path>) -> Result<()> {
    fs::remove_file(path.as_ref()).await?;
    self.cnt -= 1;
    debug!("Removed a cache file at {:?} (file count: {})", path.as_ref(), self.cnt);

    Ok(())
  }
}

#[derive(Clone, Debug)]
pub struct RpxyCache {
  /// Managing cache file objects through RwLock's lock mechanism for file lock
  cache_file_manager: Arc<RwLock<CacheFileManager>>,
  /// Lru cache storing http message caching policy
  inner: Arc<Mutex<LruCache<String, CacheObject>>>, // TODO: keyはstring urlでいいのか疑問。全requestに対してcheckすることになりそう
  /// Async runtime
  runtime_handle: tokio::runtime::Handle,
  /// Maximum size of each cache file object
  max_each_size: usize,
}

impl RpxyCache {
  /// Generate cache storage
  pub async fn new<T: CryptoSource>(globals: &Globals<T>) -> Option<Self> {
    if !globals.proxy_config.cache_enabled {
      return None;
    }

    let path = globals.proxy_config.cache_dir.as_ref().unwrap();
    let cache_file_manager = Arc::new(RwLock::new(CacheFileManager::new(path, &globals.runtime_handle).await));
    let inner = Arc::new(Mutex::new(LruCache::new(
      std::num::NonZeroUsize::new(globals.proxy_config.cache_max_entry.unwrap()).unwrap(),
    )));

    Some(Self {
      cache_file_manager,
      inner,
      runtime_handle: globals.runtime_handle.clone(),
      max_each_size: globals.proxy_config.cache_max_each_size.unwrap(),
    })
  }

  fn evict_cache_entry(&self, cache_key: &str) -> Option<(String, CacheObject)> {
    let Ok(mut lock) = self.inner.lock() else {
        error!("Mutex can't be locked to evict a cache entry");
        return None;
      };
    lock.pop_entry(cache_key)
  }

  async fn evict_cache_file(&self, filepath: impl AsRef<Path>) {
    // Acquire the write lock
    let mut mgr = self.cache_file_manager.write().await;
    if let Err(e) = mgr.remove(filepath).await {
      warn!("Eviction failed during file object removal: {:?}", e);
    };
  }

  /// Get cached response
  pub async fn get<R>(&self, req: &Request<R>) -> Option<Response<Body>> {
    debug!("Current cache entries: {:?}", self.inner);
    let cache_key = req.uri().to_string();

    // First check cache chance
    let cached_object = {
      let Ok(mut lock) = self.inner.lock() else {
        error!("Mutex can't be locked for checking cache entry");
        return None;
      };
      let Some(cached_object) = lock.get(&cache_key) else {
        return None;
      };
      cached_object.clone()
    };

    // Secondly check the cache freshness as an HTTP message
    let now = SystemTime::now();
    let http_cache_semantics::BeforeRequest::Fresh(res_parts) = cached_object.policy.before_request(req, now) else {
      // Evict stale cache entry.
      // This might be okay to keep as is since it would be updated later.
      // However, there is no guarantee that newly got objects will be still cacheable.
      // So, we have to evict stale cache entries and cache file objects if found.
      debug!("Stale cache entry and file object: {cache_key}");
      let _evicted_entry = self.evict_cache_entry(&cache_key);
      self.evict_cache_file(&cached_object.target).await;
      return None;
    };

    // Finally retrieve the file object
    let mgr = self.cache_file_manager.read().await;
    let res_body = match mgr.read(&cached_object.target).await {
      Ok(res_body) => res_body,
      Err(e) => {
        warn!("Failed to read from file cache: {e}");
        let _evicted_entry = self.evict_cache_entry(&cache_key);
        self.evict_cache_file(&cached_object.target).await;
        return None;
      }
    };

    debug!("Cache hit: {cache_key}");
    Some(Response::from_parts(res_parts, res_body))
  }

  pub async fn put(&self, uri: &hyper::Uri, body_bytes: &Bytes, policy: &CachePolicy) -> Result<()> {
    let my_cache = self.inner.clone();
    let mgr = self.cache_file_manager.clone();
    let uri = uri.clone();
    let bytes_clone = body_bytes.clone();
    let policy_clone = policy.clone();
    let max_each_size = self.max_each_size;

    self.runtime_handle.spawn(async move {
      if bytes_clone.len() > max_each_size {
        warn!("Too large to cache");
        return Err(RpxyError::Cache("Too large to cache"));
      }
      let cache_key = derive_cache_key_from_uri(&uri);
      let cache_filename = derive_filename_from_uri(&uri);

      debug!("Cache file of {:?} bytes to be written", bytes_clone.len());

      let mut mgr = mgr.write().await;
      let Ok(cache_object) = mgr.create(&cache_filename, &bytes_clone, &policy_clone).await else {
        error!("Failed to put the body into the file object or cache entry");
        return Err(RpxyError::Cache("Failed to put the body into the file object or cache entry"));
      };
      let push_opt = {
        let Ok(mut lock) = my_cache.lock() else {
          error!("Failed to acquire mutex lock for writing cache entry");
          return Err(RpxyError::Cache("Failed to acquire mutex lock for writing cache entry"));
        };
        lock.push(cache_key.clone(), cache_object)
      };
      if let Some((k, v)) = push_opt {
        if k != cache_key {
          info!("Over the cache capacity. Evict least recent used entry");
          if let Err(e) = mgr.remove(&v.target).await {
            warn!("Eviction failed during file object removal over the capacity: {:?}", e);
          };
        }
      }

      debug!("Cached a new file: {} - {}", cache_key, cache_filename);
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
    debug!("Response is cacheable: {:?}\n{:?}", req, res.headers());
    Ok(Some(new_policy))
  } else {
    Ok(None)
  }
}
