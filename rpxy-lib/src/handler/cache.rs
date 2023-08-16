use crate::{constants::MAX_CACHE_ENTRY, error::*, globals::Globals, log::*, CryptoSource};
use base64::{engine::general_purpose, Engine as _};
use bytes::{Buf, Bytes, BytesMut};
use fs4::tokio::AsyncFileExt;
use http_cache_semantics::CachePolicy;
use hyper::{
  http::{Request, Response},
  Body,
};
use moka::future::Cache as MokaCache;
use sha2::{Digest, Sha256};
use std::{
  fmt::Debug,
  path::{Path, PathBuf},
  sync::Arc,
  time::SystemTime,
};
use tokio::{
  fs::{self, File},
  io::{AsyncReadExt, AsyncWriteExt},
  sync::RwLock,
};

// #[async_trait]
// pub trait CacheTarget {
//   type TargetInput;
//   type TargetOutput;
//   type Error;
//   /// Get target object from somewhere
//   async fn get(&self) -> Self::TargetOutput;
//   /// Write target object into somewhere
//   async fn put(&self, taget: Self::TargetOutput) -> Result<(), Self::Error>;
//   /// Remove target object from somewhere (when evicted self)
//   async fn remove(&self) -> Result<(), Self::Error>;
// }

fn derive_filename_from_uri(uri: &hyper::Uri) -> String {
  let mut hasher = Sha256::new();
  hasher.update(uri.to_string());
  let digest = hasher.finalize();
  general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn derive_moka_key_from_uri(uri: &hyper::Uri) -> String {
  uri.to_string()
}

#[derive(Clone, Debug)]
struct CacheObject {
  pub policy: CachePolicy,
  pub target: Option<PathBuf>,
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

  async fn write(&mut self, cache_filename: &str, body_bytes: &Bytes, policy: &CachePolicy) -> Result<CacheObject> {
    let cache_filepath = self.cache_dir.join(cache_filename);
    let Ok(mut file) = File::create(&cache_filepath).await else {
      return Err(RpxyError::Cache("Failed to create file"));
    };
    // TODO: ここでちゃんと書けないパターンっぽい？あるいは書いた後消されるパターンが起きている模様。
    // evictしたときファイルは消えてentryが残ってるっぽい
    let mut bytes_clone = body_bytes.clone();
    while bytes_clone.has_remaining() {
      warn!("remaining {}", bytes_clone.remaining());
      if let Err(e) = file.write_buf(&mut bytes_clone).await {
        error!("Failed to write file cache: {e}");
        return Err(RpxyError::Cache("Failed to write file cache: {e}"));
      };
    }
    self.cnt += 1;
    Ok(CacheObject {
      policy: policy.clone(),
      target: Some(cache_filepath),
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
  /// Moka's cache storing http message caching policy
  inner: MokaCache<String, CacheObject>, // TODO: keyはstring urlでいいのか疑問。全requestに対してcheckすることになりそう
  /// Async runtime
  runtime_handle: tokio::runtime::Handle,
}

impl RpxyCache {
  /// Generate cache storage
  pub async fn new<T: CryptoSource>(globals: &Globals<T>) -> Option<Self> {
    if !globals.proxy_config.cache_enabled {
      return None;
    }

    let path = globals.proxy_config.cache_dir.as_ref().unwrap();
    let cache_file_manager = Arc::new(RwLock::new(CacheFileManager::new(path, &globals.runtime_handle).await));
    let mgr_clone = cache_file_manager.clone();

    let runtime_handle = globals.runtime_handle.clone();
    let eviction_listener = move |k, v: CacheObject, cause| {
      debug!("Cache entry is being evicted : {k} {:?}", cause);
      runtime_handle.block_on(async {
        if let Some(filepath) = v.target {
          debug!("Evict file object: {k}");
          // Acquire the write lock
          let mut mgr = mgr_clone.write().await;
          if let Err(e) = mgr.remove(filepath).await {
            warn!("Eviction failed during file object removal: {:?}", e);
          };
        }
      })
    };

    Some(Self {
      cache_file_manager,
      inner: MokaCache::builder()
        .max_capacity(MAX_CACHE_ENTRY)
        .eviction_listener_with_queued_delivery_mode(eviction_listener)
        .build(), // TODO: make this configurable, and along with size
      runtime_handle: globals.runtime_handle.clone(),
    })
  }

  /// Get cached response
  pub async fn get<R>(&self, req: &Request<R>) -> Option<Response<Body>> {
    debug!("Current cache entries: {:?}", self.inner);
    let moka_key = req.uri().to_string();

    // First check cache chance
    let Some(cached_object) = self.inner.get(&moka_key) else {
      return None;
    };

    let now = SystemTime::now();
    if let http_cache_semantics::BeforeRequest::Fresh(res_parts) = cached_object.policy.before_request(req, now) {
      let Some(filepath) = cached_object.target else {
        return None;
      };

      let mgr = self.cache_file_manager.read().await;
      let res_body = match mgr.read(&filepath).await {
        Ok(res_body) => res_body,
        Err(e) => {
          warn!("Failed to read from cache: {e}");
          self.inner.invalidate(&moka_key).await;
          return None;
        }
      };
      debug!("Cache hit: {moka_key}");

      Some(Response::from_parts(res_parts, res_body))
    } else {
      // Evict stale cache entry.
      // This might be okay to keep as is since it would be updated later.
      // However, there is no guarantee that newly got objects will be still cacheable.
      // So, we have to evict stale cache entries and cache file objects if found.
      debug!("Stale cache entry and file object: {moka_key}");
      self.inner.invalidate(&moka_key).await;
      // let my_cache = self.inner.clone();
      // self.runtime_handle.spawn(async move {
      // eviction listener will be activated during invalidation.
      // my_cache.invalidate(&moka_key).await;
      // });
      None
    }
  }

  pub fn is_cacheable<R>(&self, req: Option<&Request<R>>, res: Option<&Response<Body>>) -> Result<Option<CachePolicy>>
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

  pub async fn put(&self, uri: &hyper::Uri, body_bytes: &Bytes, policy: &CachePolicy) -> Result<()> {
    let my_cache = self.inner.clone();
    let uri = uri.clone();
    let bytes_clone = body_bytes.clone();
    let policy_clone = policy.clone();
    let mgr_clone = self.cache_file_manager.clone();

    self.runtime_handle.spawn(async move {
      let moka_key = derive_moka_key_from_uri(&uri);
      let cache_filename = derive_filename_from_uri(&uri);

      warn!("{:?} bytes to be written", bytes_clone.len());
      if let Err(e) = my_cache
        .try_get_with(moka_key, async {
          let mut mgr = mgr_clone.write().await;
          mgr.write(&cache_filename, &bytes_clone, &policy_clone).await
        })
        .await
      {
        error!("Failed to put the body into the file object or cache entry: {e}");
      };

      debug!("Current cache entries: {:?}", my_cache);
    });

    Ok(())
  }
}
