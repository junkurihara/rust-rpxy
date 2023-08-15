use crate::{constants::MAX_CACHE_ENTRY, error::*, globals::Globals, log::*, CryptoSource};
use base64::{engine::general_purpose, Engine as _};
use bytes::{Buf, Bytes, BytesMut};
use http_cache_semantics::CachePolicy;
use hyper::{
  http::{Request, Response},
  Body,
};
use moka::future::Cache as MokaCache;
use sha2::{Digest, Sha256};
use std::{fmt::Debug, path::PathBuf, time::SystemTime};
use tokio::{
  fs::{self, File},
  io::{AsyncReadExt, AsyncWriteExt},
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
pub struct CacheObject {
  pub policy: CachePolicy,
  pub target: Option<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct RpxyCache {
  cache_dir: PathBuf,
  inner: MokaCache<String, CacheObject>, // TODO: keyはstring urlでいいのか疑問。全requestに対してcheckすることになりそう
  runtime_handle: tokio::runtime::Handle,
}

impl RpxyCache {
  /// Generate cache storage
  pub async fn new<T: CryptoSource>(globals: &Globals<T>) -> Option<Self> {
    if !globals.proxy_config.cache_enabled {
      return None;
    }
    let runtime_handle = globals.runtime_handle.clone();
    let runtime_handle_clone = globals.runtime_handle.clone();
    let eviction_listener = move |k, v: CacheObject, cause| {
      debug!("Cache entry is being evicted : {k} {:?}", cause);
      runtime_handle.block_on(async {
        if let Some(filepath) = v.target {
          debug!("Evict file object: {k}");
          if let Err(e) = fs::remove_file(filepath).await {
            warn!("Eviction failed during file object removal: {:?}", e);
          };
        }
      })
    };

    // Create cache file dir
    // Clean up the file dir before init
    // TODO: Persistent cache is really difficult. maybe SQLite is needed.
    let path = globals.proxy_config.cache_dir.as_ref().unwrap();
    if let Err(e) = fs::remove_dir_all(path).await {
      warn!("Failed to clean up the cache dir: {e}");
    };
    fs::create_dir_all(path).await.unwrap();

    Some(Self {
      cache_dir: path.clone(),
      inner: MokaCache::builder()
        .max_capacity(MAX_CACHE_ENTRY)
        .eviction_listener_with_queued_delivery_mode(eviction_listener)
        .build(), // TODO: make this configurable, and along with size
      runtime_handle: runtime_handle_clone,
    })
  }

  /// Get cached response
  pub async fn get<R>(&self, req: &Request<R>) -> Option<Response<Body>> {
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

      let Ok(mut file) = File::open(&filepath.clone()).await else {
        warn!("Cache file doesn't exist. Remove pointer cache.");
        let my_cache = self.inner.clone();
        self.runtime_handle.spawn(async move {
          my_cache.invalidate(&moka_key).await;
        });
        return None;
      };
      let (body_sender, res_body) = Body::channel();
      self.runtime_handle.spawn(async move {
        let mut sender = body_sender;
        // let mut size = 0usize;
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

      let res = Response::from_parts(res_parts, res_body);
      debug!("Cache hit: {moka_key}");
      Some(res)
    } else {
      // Evict stale cache entry here
      debug!("Evict stale cache entry and file object: {moka_key}");
      let my_cache = self.inner.clone();
      self.runtime_handle.spawn(async move {
        // eviction listener will be activated during invalidation.
        my_cache.invalidate(&moka_key).await;
      });
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

  pub async fn put(&self, uri: &hyper::Uri, body_bytes: &Bytes, policy: CachePolicy) -> Result<()> {
    let my_cache = self.inner.clone();
    let uri = uri.clone();
    let cache_dir = self.cache_dir.clone();
    let mut bytes_clone = body_bytes.clone();

    self.runtime_handle.spawn(async move {
      let moka_key = derive_moka_key_from_uri(&uri);
      let cache_filename = derive_filename_from_uri(&uri);
      let cache_filepath = cache_dir.join(cache_filename);

      let _x = my_cache
        .get_with(moka_key, async {
          let mut file = File::create(&cache_filepath).await.unwrap();
          while bytes_clone.has_remaining() {
            if let Err(e) = file.write_buf(&mut bytes_clone).await {
              error!("Failed to write file cache: {e}");
              return CacheObject { policy, target: None };
            };
          }
          CacheObject {
            policy,
            target: Some(cache_filepath),
          }
        })
        .await;

      debug!("Current cache entries: {}", my_cache.entry_count());
    });

    Ok(())
  }
}
