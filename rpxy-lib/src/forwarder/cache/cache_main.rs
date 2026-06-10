use super::cache_error::*;
use crate::{
  globals::Globals,
  hyper_ext::body::{BoxBody, ResponseBody, UnboundedStreamBody, full},
  log::*,
};
use base64::{Engine as _, engine::general_purpose};
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
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
  },
  time::SystemTime,
};
use tokio::{
  fs::{self, File},
  io::{AsyncReadExt, AsyncWriteExt},
  sync::RwLock,
};

/// File-cache read chunk size: large enough that a typical cached object is read in one or a
/// few iterations (vs the ~64 B that `BytesMut` auto-grows per `read_buf`). Each read fills at
/// most one chunk-sized buffer, so we never load the whole object into a single `BytesMut`
/// (matters when `max_each_size` is configured large). This bounds the per-read buffer, not
/// total live memory: the `mpsc::unbounded` stream can still queue chunks if the downstream is
/// slow (a separate concern).
const FILE_CACHE_READ_CHUNK: usize = 64 * 1024;

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
  #[allow(unused)]
  /// Generate cache storage
  pub(crate) async fn new(globals: &Globals) -> Option<Self> {
    if !globals.proxy_config.cache_enabled {
      return None;
    }
    let cache_dir = match globals.proxy_config.cache_dir.as_ref() {
      Some(dir) => dir,
      None => {
        warn!("Cache directory not set in proxy config");
        return None;
      }
    };
    let file_store = FileStore::new(&globals.runtime_handle).await;
    let inner = LruCacheManager::new(globals.proxy_config.cache_max_entry);

    let max_each_size = globals.proxy_config.cache_max_each_size;
    let mut max_each_size_on_memory = globals.proxy_config.cache_max_each_size_on_memory;
    if max_each_size < max_each_size_on_memory {
      warn!("Maximum size of on-memory cache per entry must be smaller than or equal to the maximum of each file cache");
      max_each_size_on_memory = max_each_size;
    }

    if let Err(e) = fs::remove_dir_all(cache_dir).await {
      warn!("Failed to clean up the cache dir: {e}");
    }
    if let Err(e) = fs::create_dir_all(&cache_dir).await {
      error!("Failed to create cache dir: {e}");
      return None;
    }

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
  pub(crate) async fn put(&self, uri: &hyper::Uri, body: Incoming, policy: &CachePolicy) -> CacheResult<UnboundedStreamBody> {
    let cache_manager = self.inner.clone();
    let mut file_store = self.file_store.clone();
    let uri = uri.clone();
    let policy_clone = policy.clone();
    let max_each_size = self.max_each_size;
    let max_each_size_on_memory = self.max_each_size_on_memory;
    let cache_dir = self.cache_dir.clone();

    let (body_tx, body_rx) = mpsc::unbounded::<Result<Frame<Bytes>, hyper::Error>>();

    self.runtime_handle.spawn(async move {
      // Forward the whole response body downstream while buffering up to `max_each_size`
      // for caching. `body_tx` is moved into `spool_body` and dropped there, so an
      // over-limit (or upstream-errored) response is delivered to the client in full and
      // simply not cached - the cache layer never truncates it.
      let Some(buf) = spool_body(body, body_tx, max_each_size).await else {
        return Ok(()) as CacheResult<()>;
      };

      // Calculate hash of the cached data, after all data is received.
      // In-operation calculation is possible but it blocks sending data.
      let mut hasher = Sha256::new();
      hasher.update(buf.as_ref());
      let hash_bytes = Bytes::copy_from_slice(hasher.finalize().as_ref());
      trace!("Cached data: {} bytes, hash = {:?}", buf.len(), hash_bytes);

      // Create cache object
      let cache_key = derive_cache_key_from_uri(&uri);
      let cache_object = CacheObject {
        policy: policy_clone,
        target: CacheFileOrOnMemory::build(&cache_dir, &uri, &buf, max_each_size_on_memory),
        hash: hash_bytes,
      };

      if let Some((k, v)) = cache_manager.push(&cache_key, &cache_object)?
        && k != cache_key
      {
        info!("Over the cache capacity. Evict least recent used entry");
        if let CacheFileOrOnMemory::File(path) = v.target {
          file_store.evict(&path).await;
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
    trace!("Current cache status: (total, on-memory, file) = {:?}", self.count().await);
    let cache_key = derive_cache_key_from_uri(req.uri());

    // First check cache chance
    let cached_object = self.inner.get(&cache_key).ok()??;

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
/// Stream `body` to `body_tx` while buffering up to `max_each_size` bytes for caching.
///
/// Every frame (data, trailers, and any error frame) is forwarded downstream unchanged, so
/// the response reaches the client in full regardless of the cache decision. Returns
/// `Some(buf)` with the fully buffered body when it stayed within `max_each_size` (and may
/// therefore be cached); returns `None` when the object is too large, the upstream body
/// errored, or the downstream receiver went away. In every `None` case the frames seen so
/// far have already been forwarded, so the cache layer never truncates the response.
///
/// `body_tx` is taken by value and dropped on return, so `body_rx` reaches a clean EOF as
/// soon as streaming finishes.
async fn spool_body<B, E>(
  mut body: B,
  body_tx: mpsc::UnboundedSender<Result<Frame<Bytes>, E>>,
  max_each_size: usize,
) -> Option<Bytes>
where
  B: hyper::body::Body<Data = Bytes, Error = E> + Unpin,
{
  let mut buf = BytesMut::new();
  let mut cacheable = true;

  while let Some(frame) = body.frame().await {
    if cacheable {
      match frame.as_ref() {
        // Data frames are buffered up to the limit; non-data frames (e.g. trailers) carry
        // no data and are forwarded only. `data_ref()` is `None` exactly for non-data frames,
        // so this also avoids panicking on an unexpected frame shape.
        Ok(f) => {
          if let Some(data) = f.data_ref() {
            // `saturating_add` keeps the size check correct even against a pathologically
            // large frame length, so the limit can never be bypassed by integer overflow.
            if buf.len().saturating_add(data.len()) > max_each_size {
              debug!("Response exceeds max_each_size ({max_each_size} bytes); forwarding without caching");
              cacheable = false;
              buf = BytesMut::new(); // release buffered bytes; this object will not be cached
            } else {
              buf.extend_from_slice(data.as_ref());
            }
          }
        }
        // Upstream body error: a complete object cannot be cached. The error frame is still
        // forwarded below so the downstream consumer observes it instead of a silent EOF.
        Err(_) => {
          cacheable = false;
          buf = BytesMut::new();
        }
      }
    }

    // Always forward the frame downstream, regardless of the cache decision.
    if body_tx.unbounded_send(frame).is_err() {
      // Downstream receiver is gone; nothing left to forward or cache.
      return None;
    }
  }

  cacheable.then(|| buf.freeze())
}

/* ---------------------------------------------- */
#[derive(Debug, Clone)]
/// Cache file manager outer that is responsible to handle `RwLock`
struct FileStore {
  /// Inner file store main object
  inner: Arc<RwLock<FileStoreInner>>,
}
impl FileStore {
  #[allow(unused)]
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
  /// Create a temporary file cache, returns error if file cannot be created or written
  async fn create(&mut self, cache_object: &CacheObject, body_bytes: &Bytes) -> CacheResult<()> {
    let mut inner = self.inner.write().await;
    inner.create(cache_object, body_bytes).await
  }
  /// Evict a temporary file cache, logs warning if removal fails
  async fn evict(&self, path: impl AsRef<Path>) {
    let mut inner = self.inner.write().await;
    if let Err(e) = inner.remove(path).await {
      warn!("Eviction failed during file object removal: {:?}", e);
    }
  }
  /// Read a temporary file cache, returns error if file cannot be opened or hash mismatches
  async fn read(&self, path: impl AsRef<Path> + Send + Sync + 'static, hash: &Bytes) -> CacheResult<UnboundedStreamBody> {
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
  #[allow(unused)]
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
    let mut file = File::create(&cache_filepath)
      .await
      .map_err(|_| CacheError::FailedToCreateFileCache)?;
    let mut bytes_clone = body_bytes.clone();
    while bytes_clone.has_remaining() {
      file.write_buf(&mut bytes_clone).await.map_err(|e| {
        error!("Failed to write file cache: {e}");
        CacheError::FailedToWriteFileCache
      })?;
    }
    self.cnt += 1;
    Ok(())
  }

  /// Retrieve a stored temporary file cache
  async fn read(&self, path: impl AsRef<Path> + Send + Sync + 'static, hash: &Bytes) -> CacheResult<UnboundedStreamBody> {
    let Ok(mut file) = File::open(&path).await else {
      warn!("Cache file object cannot be opened");
      return Err(CacheError::FailedToOpenCacheFile);
    };
    let hash_clone = hash.clone();
    let mut self_clone = self.clone();

    let (body_tx, body_rx) = mpsc::unbounded::<Result<Frame<Bytes>, hyper::Error>>();

    self.runtime_handle.spawn(async move {
      let mut hasher = Sha256::new();
      let mut buf = BytesMut::with_capacity(FILE_CACHE_READ_CHUNK);
      loop {
        // Reserve a fresh chunk only when the spare capacity is exhausted. After `split()` the
        // buffer keeps whatever spare it had, so a small object is read into the initial
        // capacity and the EOF-confirming read reuses the leftover spare without allocating.
        if buf.capacity() == buf.len() {
          buf.reserve(FILE_CACHE_READ_CHUNK);
        }
        match file.read_buf(&mut buf).await {
          Ok(0) => break,
          Ok(_) => {
            // Hand the filled bytes off zero-copy; `buf` keeps the remaining spare capacity.
            let bytes = buf.split().freeze();
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
  #[allow(unused)]
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

  /// Evict an entry from the LRU cache, logs error if mutex cannot be acquired
  fn evict(&self, cache_key: &str) -> Option<(String, CacheObject)> {
    let mut lock = match self.inner.lock() {
      Ok(lock) => lock,
      Err(_) => {
        error!("Mutex can't be locked to evict a cache entry");
        return None;
      }
    };
    let res = lock.pop_entry(cache_key);
    // This may be inconsistent with the actual number of entries
    self.cnt.store(lock.len(), Ordering::Relaxed);
    res
  }

  /// Push an entry into the LRU cache, returns error if mutex cannot be acquired
  fn push(&self, cache_key: &str, cache_object: &CacheObject) -> CacheResult<Option<(String, CacheObject)>> {
    let mut lock = self.inner.lock().map_err(|_| {
      error!("Failed to acquire mutex lock for writing cache entry");
      CacheError::FailedToAcquiredMutexLockForCache
    })?;
    let res = Ok(lock.push(cache_key.to_string(), cache_object.clone()));
    // This may be inconsistent with the actual number of entries
    self.cnt.store(lock.len(), Ordering::Relaxed);
    res
  }

  /// Get an entry from the LRU cache, returns error if mutex cannot be acquired
  fn get(&self, cache_key: &str) -> CacheResult<Option<CacheObject>> {
    let mut lock = self.inner.lock().map_err(|_| {
      error!("Mutex can't be locked for checking cache entry");
      CacheError::FailedToAcquiredMutexLockForCheck
    })?;
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

#[cfg(test)]
mod tests {
  use super::*;
  use futures::{StreamExt, stream};

  /// Build an `Ok` data frame from a static byte slice.
  fn data_frame(bytes: &'static [u8]) -> Result<Frame<Bytes>, hyper::Error> {
    Ok(Frame::data(Bytes::from_static(bytes)))
  }

  /// Build a test body from a list of frames. Only `Ok` frames are constructed, so no
  /// `hyper::Error` needs to be built.
  fn body_from(
    frames: Vec<Result<Frame<Bytes>, hyper::Error>>,
  ) -> impl hyper::body::Body<Data = Bytes, Error = hyper::Error> + Unpin {
    StreamBody::new(stream::iter(frames))
  }

  /// Concatenate the data bytes of all forwarded frames in order.
  fn forwarded_data(frames: Vec<Result<Frame<Bytes>, hyper::Error>>) -> Vec<u8> {
    frames
      .into_iter()
      .filter_map(|f| f.ok())
      .filter_map(|f| f.into_data().ok())
      .flat_map(|b| b.to_vec())
      .collect()
  }

  #[tokio::test]
  async fn within_limit_caches_and_forwards_all() {
    let (tx, rx) = mpsc::unbounded::<Result<Frame<Bytes>, hyper::Error>>();
    let body = body_from(vec![data_frame(b"hello"), data_frame(b"world")]);
    let cached = spool_body(body, tx, 1024).await;
    assert_eq!(cached.as_deref(), Some(&b"helloworld"[..]));
    assert_eq!(forwarded_data(rx.collect::<Vec<_>>().await), b"helloworld");
  }

  /// Regression test for the truncation bug: an over-limit cacheable response must still be
  /// forwarded to the client in full, just not cached.
  #[tokio::test]
  async fn over_limit_forwards_all_but_does_not_cache() {
    let (tx, rx) = mpsc::unbounded::<Result<Frame<Bytes>, hyper::Error>>();
    // three 5-byte frames = 15 bytes total, over the 8-byte limit
    let body = body_from(vec![data_frame(b"aaaaa"), data_frame(b"bbbbb"), data_frame(b"ccccc")]);
    let cached = spool_body(body, tx, 8).await;
    assert!(cached.is_none(), "over-limit object must not be cached");
    assert_eq!(
      forwarded_data(rx.collect::<Vec<_>>().await),
      b"aaaaabbbbbccccc",
      "all frames must be forwarded, not truncated"
    );
  }

  #[tokio::test]
  async fn boundary_exactly_max_is_cached() {
    let (tx, rx) = mpsc::unbounded::<Result<Frame<Bytes>, hyper::Error>>();
    // 5 + 3 = 8 == limit (matches the original `size > max_each_size` boundary)
    let body = body_from(vec![data_frame(b"aaaaa"), data_frame(b"bbb")]);
    let cached = spool_body(body, tx, 8).await;
    assert_eq!(cached.as_deref(), Some(&b"aaaaabbb"[..]));
    assert_eq!(forwarded_data(rx.collect::<Vec<_>>().await), b"aaaaabbb");
  }

  /// This only pins down trailer *forwarding* and that `buf` holds data bytes only. It does
  /// not assert that trailer-bearing responses are cacheable as a spec (a cache hit does not
  /// reproduce trailers); that is pre-existing behaviour and out of scope (design doc 3/8).
  #[tokio::test]
  async fn forwards_trailers_without_buffering_them() {
    let (tx, rx) = mpsc::unbounded::<Result<Frame<Bytes>, hyper::Error>>();
    let mut trailers = http::HeaderMap::new();
    trailers.insert("x-trailer", http::HeaderValue::from_static("v"));
    let body = body_from(vec![data_frame(b"data"), Ok(Frame::trailers(trailers))]);
    let cached = spool_body(body, tx, 1024).await;
    assert_eq!(cached.as_deref(), Some(&b"data"[..]));
    let forwarded = rx.collect::<Vec<_>>().await;
    assert_eq!(forwarded.len(), 2);
    assert!(forwarded[1].as_ref().unwrap().is_trailers());
  }

  #[tokio::test]
  async fn returns_none_when_downstream_dropped() {
    let (tx, rx) = mpsc::unbounded::<Result<Frame<Bytes>, hyper::Error>>();
    drop(rx);
    let body = body_from(vec![data_frame(b"a"), data_frame(b"b")]);
    let cached = spool_body(body, tx, 1024).await;
    assert!(cached.is_none());
  }

  /// Stand-in body error type. `hyper::Error` has no public constructor, so the error path is
  /// exercised with a body whose `Error` is this type; `spool_body` is generic over the error.
  #[derive(Debug)]
  struct TestBodyError;

  /// Regression test for the upstream-error path: an error frame must be propagated downstream
  /// (not masked as a clean EOF), and the object must not be cached.
  #[tokio::test]
  async fn upstream_error_is_propagated_and_not_cached() {
    let (tx, rx) = mpsc::unbounded::<Result<Frame<Bytes>, TestBodyError>>();
    let frames: Vec<Result<Frame<Bytes>, TestBodyError>> =
      vec![Ok(Frame::data(Bytes::from_static(b"partial"))), Err(TestBodyError)];
    let body = StreamBody::new(stream::iter(frames));
    let cached = spool_body(body, tx, 1024).await;
    assert!(cached.is_none(), "an errored upstream body must not be cached");
    let forwarded = rx.collect::<Vec<_>>().await;
    assert_eq!(forwarded.len(), 2, "the data frame and the error frame are both forwarded");
    assert!(forwarded[0].is_ok());
    assert!(forwarded[1].is_err(), "the upstream error must be propagated downstream");
  }

  /// Unique temp path for a file-cache test object.
  fn temp_cache_path(tag: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("rpxy-cache-test-{tag}-{}-{nanos}", std::process::id()))
  }

  /// A cached file larger than `FILE_CACHE_READ_CHUNK` must be streamed back intact, exercising
  /// the multi-chunk read path. Guards correct reassembly across chunk boundaries.
  #[tokio::test]
  async fn file_store_read_streams_object_across_chunks() {
    let path = temp_cache_path("ok");
    // ~200 KB so the read spans several FILE_CACHE_READ_CHUNK (64 KiB) iterations.
    let content: Vec<u8> = (0..200_000usize).map(|i| (i % 251) as u8).collect();
    fs::write(&path, &content).await.unwrap();
    let hash = Bytes::copy_from_slice(Sha256::digest(&content).as_ref());

    let file_store = FileStoreInner {
      cnt: 0,
      runtime_handle: tokio::runtime::Handle::current(),
    };
    let body = file_store.read(path.clone(), &hash).await.unwrap();
    let got = BodyExt::collect(body).await.unwrap().to_bytes();
    assert_eq!(got.as_ref(), content.as_slice());
    // The happy path leaves the file in place.
    assert!(fs::metadata(&path).await.is_ok());
    let _ = fs::remove_file(&path).await;
  }

  /// On a hash mismatch the file is evicted. `read()` returns the stream immediately and the
  /// integrity check + removal run at the end of the spawned task, so the stream is drained to
  /// EOF before asserting the file is gone (draining to EOF implies the task finished, since it
  /// awaits the removal before dropping the sender that closes the stream).
  #[tokio::test]
  async fn file_store_read_evicts_on_hash_mismatch() {
    let path = temp_cache_path("bad");
    let content = b"some cached bytes".to_vec();
    fs::write(&path, &content).await.unwrap();
    let wrong_hash = Bytes::from_static(&[0u8; 32]);

    let file_store = FileStoreInner {
      cnt: 1, // removal decrements the counter; start at 1 to avoid an underflow in the test
      runtime_handle: tokio::runtime::Handle::current(),
    };
    let body = file_store.read(path.clone(), &wrong_hash).await.unwrap();
    let _ = BodyExt::collect(body).await; // drain to EOF; data frames are all Ok, the mismatch is internal
    assert!(
      fs::metadata(&path).await.is_err(),
      "a corrupted cache file must be removed on hash mismatch"
    );
  }
}
