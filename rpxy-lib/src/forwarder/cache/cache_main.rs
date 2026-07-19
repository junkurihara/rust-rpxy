use super::cache_error::*;
use crate::{
  globals::Globals,
  hyper_ext::body::{BoundedStreamBody, BoxBody, ResponseBody, full},
  log::*,
};
use base64::{Engine as _, engine::general_purpose};
use bytes::{Bytes, BytesMut};
use futures::{SinkExt, channel::mpsc};
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
    atomic::{AtomicU64, AtomicUsize, Ordering},
  },
  time::SystemTime,
};
use tokio::{
  fs::{self, File, OpenOptions},
  io::{AsyncReadExt, AsyncWriteExt},
};

/// File-cache read chunk size: large enough that a typical cached object is read in one or a
/// few iterations (vs the ~64 B that `BytesMut` auto-grows per `read_buf`). Each read fills at
/// most one chunk-sized buffer, so we never load the whole object into a single `BytesMut`
/// (matters when `max_each_size` is configured large). This bounds the per-read buffer; how many
/// such chunks can queue toward a slow downstream is bounded separately by
/// `CACHE_STREAM_CHANNEL_CAPACITY`.
const FILE_CACHE_READ_CHUNK: usize = 64 * 1024;

/// Capacity of the bounded per-stream channels relaying cache-path bodies downstream (both the
/// file-read hit path and the store/miss path). The producer awaits when the channel is full, so
/// per-stream queued memory is capped at `capacity + 1` frames (futures-mpsc grants each sender
/// one slot beyond the buffer) instead of the whole object when the consumer is slower than the
/// producer. A few frames of slack keep a fast consumer fed without a producer/consumer wakeup
/// ping-pong on every frame.
const CACHE_STREAM_CHANNEL_CAPACITY: usize = 4;

/* ---------------------------------------------- */
#[derive(Clone, Debug)]
/// Cache main manager
pub(crate) struct RpxyCache {
  /// Inner lru cache manager storing http message caching policy
  inner: LruCacheManager,
  /// Managing committed cache file objects (lock-free count; files are immutable once committed)
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
    let cache_dir = match globals.proxy_config.cache_dir.as_ref() {
      Some(dir) => dir,
      None => {
        warn!("Cache directory not set in proxy config");
        return None;
      }
    };
    let file_store = FileStore::new(&globals.runtime_handle).await;
    let max_total_size = globals.proxy_config.cache_max_total_size;
    let inner = LruCacheManager::new(globals.proxy_config.cache_max_entry, max_total_size);

    let (max_each_size, max_each_size_on_memory) = effective_per_entry_limits(
      globals.proxy_config.cache_max_each_size,
      globals.proxy_config.cache_max_each_size_on_memory,
      max_total_size,
    );

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
    // `total` (LRU) and `file` (file store) are tracked under different locks and updated in
    // separate steps while publishing/evicting, so a concurrent store can transiently make
    // `file > total` (a file counted just before its metadata is published). Saturate instead of
    // underflowing; the count is best-effort and converges once the publish completes.
    let on_memory = total.saturating_sub(file);
    (total, on_memory, file)
  }

  /// Put response into the cache
  pub(crate) async fn put(&self, uri: &hyper::Uri, body: Incoming, policy: &CachePolicy) -> CacheResult<BoundedStreamBody> {
    let cache_manager = self.inner.clone();
    let file_store = self.file_store.clone();
    let uri = uri.clone();
    let policy_clone = policy.clone();
    let max_each_size = self.max_each_size;
    let max_each_size_on_memory = self.max_each_size_on_memory;
    let cache_dir = self.cache_dir.clone();

    let (body_tx, body_rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(CACHE_STREAM_CHANNEL_CAPACITY);

    self.runtime_handle.spawn(async move {
      // Forward the whole response body downstream while incrementally hashing it and either
      // buffering it on memory (small objects) or streaming it to a temp file (larger ones).
      // `body_tx` is moved into `spool_and_store` and dropped there, so an over-limit,
      // upstream-errored, or store-failed response is delivered to the client in full and simply
      // not cached - the cache layer never truncates the response on any cache-side failure.
      // The channel is bounded: when the downstream consumer is slower than the upstream, the
      // relay (and hence the upstream read and the store) pauses instead of queueing frames in
      // memory without bound.
      let Some((target, hash, size)) =
        spool_and_store(body, body_tx, max_each_size, max_each_size_on_memory, &cache_dir, &uri).await
      else {
        return;
      };

      let cache_key = derive_cache_key_from_uri(&uri);
      let cache_object = CacheObject::new(policy_clone, target, hash, size);
      // The file (if any) is now fully written and renamed into place, so it is safe to publish
      // the metadata; this also accounts for the file count and evicts any displaced file.
      publish_cache_object(&cache_manager, &file_store, &cache_key, cache_object).await;
    });

    let stream_body = StreamBody::new(body_rx);

    Ok(stream_body)
  }

  /// Get cached response
  pub(crate) async fn get<R>(&self, req: &Request<R>) -> Option<Response<ResponseBody>> {
    trace!(
      "Current cache status: (total, on-memory, file) = {:?}, retained bytes = {}",
      self.count().await,
      self.inner.total_bytes()
    );
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
      // Only evict if this exact generation is still current: a concurrent re-store may have
      // already replaced it with a fresh (live) entry that must not be removed, and whose file the
      // replacing store now owns.
      debug!("Stale cache entry: {cache_key}");
      if self.inner.evict_if_generation(&cache_key, cached_object.generation).is_some()
        && let CacheFileOrOnMemory::File(path) = &cached_object.target
      {
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
            // Conditional eviction: only drop this entry/file if a concurrent re-store has not
            // already replaced it under the same key (the replacement owns its own file).
            if self.inner.evict_if_generation(&cache_key, cached_object.generation).is_some() {
              self.file_store.evict(path).await;
            }
            return None;
          }
        };
        debug!("Cache hit from file: {cache_key}");
        ResponseBody::Streamed(stream_body)
      }
      CacheFileOrOnMemory::OnMemory(object) => {
        // No integrity re-check here, unlike the file target. A file-backed object lives on disk
        // (an external, mutable resource that can be corrupted or overwritten independently), so
        // `FileStore::read` re-verifies its hash on every read. An on-memory object is an
        // immutable `Bytes` held inside the same `CacheObject` as its `hash` and is never mutated
        // after insertion, with no external aliasing. Re-hashing it on every hit only guards
        // against in-RAM corruption, which the stored `hash` itself equally suffers, so it is not
        // worth a full SHA-256 per hit.
        debug!("Cache hit from on memory: {cache_key}");
        ResponseBody::Boxed(BoxBody::new(full(object)))
      }
    };
    Some(Response::from_parts(res_parts, response_body))
  }
}

/* ---------------------------------------------- */
/// Clamp the per-entry cache limits against the total-size ceiling and against each other,
/// logging when a configured value is reduced. Returns `(max_each_size, max_each_size_on_memory)`.
///
/// The per-entry limit is clamped to the total ceiling FIRST, so an object that could never be
/// retained is abandoned early in `spool_and_store` (bounded temp files) instead of being fully
/// spooled and then refused at publish; the on-memory clamp then follows the already-clamped
/// value transitively.
fn effective_per_entry_limits(
  mut max_each_size: usize,
  mut max_each_size_on_memory: usize,
  max_total_size: Option<usize>,
) -> (usize, usize) {
  if let Some(limit) = max_total_size
    && max_each_size > limit
  {
    warn!("Maximum size of each cache object must be smaller than or equal to the total cache size ceiling");
    max_each_size = limit;
  }
  if max_each_size < max_each_size_on_memory {
    warn!("Maximum size of on-memory cache per entry must be smaller than or equal to the maximum of each file cache");
    max_each_size_on_memory = max_each_size;
  }
  (max_each_size, max_each_size_on_memory)
}

/// Monotonic counter making temp/final cache file names process-unique (see `unique_cache_paths`).
static CACHE_FILE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Build a `(temp, final)` path pair with a process-unique name in `cache_dir`. The final name is
/// generation-unique - not merely URI-derived - so concurrent stores of the same URI never collide
/// or clobber each other's file; each cache entry references its own immutable file. The
/// URI-derived prefix is kept only for human debuggability.
fn unique_cache_paths(cache_dir: &Path, uri: &Uri) -> (PathBuf, PathBuf) {
  let base = derive_filename_from_uri(uri);
  let nanos = SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_nanos())
    .unwrap_or(0);
  let seq = CACHE_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
  let unique = format!("{base}-{}-{nanos}-{seq}", std::process::id());
  let final_path = cache_dir.join(&unique);
  let temp_path = cache_dir.join(format!("{unique}.tmp"));
  (temp_path, final_path)
}

/// Unlink a cache file WITHOUT adjusting the file-store count. A missing file is ignored. Used
/// wherever the count must not change: temp files that never reached commit (never counted), and
/// the integrity-check (hash mismatch) removal, where the file IS counted but its LRU metadata
/// still exists - there the count is reconciled later, when that metadata is evicted via the
/// counted `FileStore::evict`/`remove` (which tolerates the already-missing file). Counted files
/// whose metadata is already gone go through `FileStore::evict` directly instead.
async fn remove_uncounted_file(path: &Path) {
  if let Err(e) = fs::remove_file(path).await
    && e.kind() != std::io::ErrorKind::NotFound
  {
    warn!("Failed to remove uncommitted cache file {path:?}: {e}");
  }
}

/// An in-progress file-cache write: data is appended to a temp file that is atomically renamed to
/// its final path on `commit`. The file-store count is intentionally NOT touched here; it is bumped
/// by `publish_cache_object` (just before publishing the metadata).
struct SpillFile {
  file: File,
  temp_path: PathBuf,
  final_path: PathBuf,
}

impl SpillFile {
  /// Create a fresh temp file with a generation-unique name in `cache_dir`. `create_new(true)`
  /// refuses to follow or overwrite an existing file/symlink.
  async fn create(cache_dir: &Path, uri: &Uri) -> CacheResult<Self> {
    let (temp_path, final_path) = unique_cache_paths(cache_dir, uri);
    let file = OpenOptions::new()
      .write(true)
      .create_new(true)
      .open(&temp_path)
      .await
      .map_err(|e| {
        error!("Failed to create temp cache file {temp_path:?}: {e}");
        CacheError::FailedToCreateFileCache
      })?;
    Ok(Self {
      file,
      temp_path,
      final_path,
    })
  }

  /// Append `data` to the temp file.
  async fn write(&mut self, data: &[u8]) -> CacheResult<()> {
    self.file.write_all(data).await.map_err(|e| {
      error!("Failed to write temp cache file {:?}: {e}", self.temp_path);
      CacheError::FailedToWriteFileCache
    })
  }

  /// Flush and atomically rename the temp file to its final path, returning that path. On any
  /// failure the temp file is removed and an error is returned.
  async fn commit(self) -> CacheResult<PathBuf> {
    let SpillFile {
      mut file,
      temp_path,
      final_path,
    } = self;
    if let Err(e) = file.flush().await {
      error!("Failed to flush temp cache file {temp_path:?}: {e}");
      drop(file);
      remove_uncounted_file(&temp_path).await;
      return Err(CacheError::FailedToWriteFileCache);
    }
    drop(file); // close the handle before renaming
    if let Err(e) = fs::rename(&temp_path, &final_path).await {
      error!("Failed to rename cache file {temp_path:?} -> {final_path:?}: {e}");
      remove_uncounted_file(&temp_path).await;
      return Err(CacheError::FailedToRenameCacheFile);
    }
    Ok(final_path)
  }

  /// Discard the in-progress temp file (close + unlink). Does not touch the file-store count.
  async fn abort(self) {
    let SpillFile { file, temp_path, .. } = self;
    drop(file);
    remove_uncounted_file(&temp_path).await;
  }
}

/// Forward `body` downstream frame by frame while attempting to cache it.
///
/// Hard invariant: a cache-side failure - too-large body, upstream body error, or any file I/O
/// failure - must NEVER cut the downstream relay. Every frame is forwarded first; caching is then
/// attempted as a side effect and silently abandoned (cleaning up any temp file) on failure.
///
/// `body_tx` is bounded, so forwarding awaits a free slot when the downstream consumer lags:
/// backpressure pauses the relay (and, transitively, the upstream read and the store) instead of
/// queueing frames in memory without bound. Pausing is not cutting - the send fails only when the
/// receiver is dropped, exactly the case the relay has nothing left to forward to.
///
/// Returns `Some((target, hash, size))` when the object was fully and successfully stored (on
/// memory, or streamed to a committed file); `size` is the object's data length in bytes, used
/// for total-size accounting at publish. Returns `None` otherwise. `body_tx` is taken by value
/// and dropped on return, so `body_rx` reaches a clean EOF as soon as streaming finishes.
async fn spool_and_store<B, E>(
  mut body: B,
  mut body_tx: mpsc::Sender<Result<Frame<Bytes>, E>>,
  max_each_size: usize,
  max_each_size_on_memory: usize,
  cache_dir: &Path,
  uri: &Uri,
) -> Option<(CacheFileOrOnMemory, Bytes, usize)>
where
  B: hyper::body::Body<Data = Bytes, Error = E> + Unpin,
{
  let mut hasher = Sha256::new();
  let mut buf = BytesMut::new(); // Phase M: in-memory buffer until the on-memory threshold
  let mut size: usize = 0;
  let mut cacheable = true;
  let mut spill: Option<SpillFile> = None; // Phase F: present once spilled to a temp file

  while let Some(frame) = body.frame().await {
    // Take the cache-side data handle before the frame is moved into the send. `Bytes` is
    // reference-counted, so this is a cheap Arc bump, not a body copy; `None` for an error item or
    // a non-data frame (e.g. trailers).
    let data = frame.as_ref().ok().and_then(|f| f.data_ref().cloned());
    let was_err = frame.is_err();

    // Forward downstream first; the relay is never cut by cache work. The bounded send awaits a
    // free slot when the consumer lags (backpressure) and errs only on a dropped receiver.
    if body_tx.send(frame).await.is_err() {
      // Downstream receiver is gone; nothing left to forward or cache.
      if let Some(s) = spill.take() {
        s.abort().await;
      }
      return None;
    }

    if !cacheable {
      continue; // keep draining/forwarding, but no longer caching
    }

    // Upstream body error: a complete object cannot be cached. The error frame was already
    // forwarded above so the downstream consumer observes it instead of a silent EOF.
    if was_err {
      cacheable = false;
      if let Some(s) = spill.take() {
        s.abort().await;
      }
      buf = BytesMut::new();
      continue;
    }

    let Some(data) = data else {
      continue; // non-data frame: forward only
    };

    // `saturating_add` keeps the size check correct even against a pathologically large frame
    // length, so the limit can never be bypassed by integer overflow.
    if size.saturating_add(data.len()) > max_each_size {
      debug!("Response exceeds max_each_size ({max_each_size} bytes); forwarding without caching");
      cacheable = false;
      if let Some(s) = spill.take() {
        s.abort().await;
      }
      buf = BytesMut::new();
      continue;
    }
    size = size.saturating_add(data.len());
    hasher.update(data.as_ref());

    if spill.is_some() {
      // Phase F: write straight to the temp file.
      if spill.as_mut().unwrap().write(data.as_ref()).await.is_err() {
        cacheable = false;
        spill.take().unwrap().abort().await;
      }
    } else if buf.len().saturating_add(data.len()) > max_each_size_on_memory {
      // Phase M crossing the on-memory threshold: spill to a temp file. Write the already-buffered
      // bytes and this frame straight to disk rather than first growing `buf` by a potentially
      // large frame, which would defeat the point of bounding store-path memory.
      match SpillFile::create(cache_dir, uri).await {
        Ok(mut s) => {
          if s.write(buf.as_ref()).await.is_err() || s.write(data.as_ref()).await.is_err() {
            cacheable = false;
            s.abort().await;
          } else {
            spill = Some(s);
          }
        }
        Err(_) => {
          // Could not create a temp file; give up caching but keep forwarding.
          cacheable = false;
        }
      }
      buf = BytesMut::new(); // free the in-memory copy regardless of spill outcome
    } else {
      // Phase M still under the on-memory threshold: keep buffering on memory.
      buf.extend_from_slice(data.as_ref());
    }
  }

  if !cacheable {
    return None; // any temp file was already aborted above
  }

  let hash = Bytes::copy_from_slice(hasher.finalize().as_ref());
  match spill {
    // Phase F: commit the temp file to its final path.
    Some(s) => match s.commit().await {
      Ok(final_path) => Some((CacheFileOrOnMemory::File(final_path), hash, size)),
      Err(_) => None, // commit failed and cleaned up its temp; nothing to publish
    },
    // Phase M: small enough to stay on memory.
    None => Some((CacheFileOrOnMemory::OnMemory(buf.freeze()), hash, size)),
  }
}

/// Publish a freshly stored cache object's metadata into the LRU, accounting for the file-store
/// count and evicting every displaced entry's file.
///
/// Ordering matters for correctness: a file is counted (`incr_count`) BEFORE its metadata is
/// published, so a `File` entry visible in the LRU is always already counted and a concurrent
/// eviction can never decrement a not-yet-counted file (the count and the LRU use separate locks).
/// If `push()` fails (poisoned mutex) or refuses the object (total-size preflight), the
/// just-counted file is rolled back (unlink + decrement) via `evict`. Displaced entries' files
/// (same-key replacement, total-size eviction, entry-count capacity eviction) are evicted after
/// a successful `push()` — outside the LRU lock, which `push()` has already released.
async fn publish_cache_object(
  cache_manager: &LruCacheManager,
  file_store: &FileStore,
  cache_key: &str,
  cache_object: CacheObject,
) {
  let new_file_path = match &cache_object.target {
    CacheFileOrOnMemory::File(path) => Some(path.clone()),
    CacheFileOrOnMemory::OnMemory(_) => None,
  };

  // Count a file-backed object BEFORE publishing its metadata. The file count and the LRU map are
  // guarded by different locks, so this ordering upholds the invariant "a File entry visible in the
  // LRU has already been counted": any concurrent eviction that observes the published entry always
  // finds a count it can safely decrement, instead of decrementing a not-yet-counted file (which
  // would underflow). The transient `file > total` this opens (counted, not yet in the LRU) is
  // tolerated by `count()` via `saturating_sub`.
  if new_file_path.is_some() {
    file_store.incr_count().await;
  }

  match cache_manager.push(cache_key, &cache_object) {
    Err(e) => {
      // Metadata could not be published; roll back both the count and the committed-but-unpublished
      // file so neither leaks (the file was already counted just above).
      warn!("Failed to publish cache entry: {e}");
      if let Some(path) = &new_file_path {
        file_store.evict(path).await;
      }
    }
    Ok(outcome) => {
      if !outcome.stored {
        // Refused: by the pre-mutation preflight (no-fit / unrepresentable counter — nothing
        // was evicted or inserted), or because the byte accounting is invalid (any entries
        // already popped are in `displaced` below). Roll back the new file exactly like the
        // Err arm above. Not reachable in normal operation (the constructor clamps
        // max_each_size to the ceiling and invalidation needs a corrupted counter).
        warn!("Cache store refused by the total-size accounting; response served without caching");
        if let Some(path) = &new_file_path {
          file_store.evict(path).await;
        }
      }
      // Evict every displaced entry's file (same-key update, total-size eviction, or capacity
      // eviction), unless it is the very file just published (only possible without
      // generation-unique paths).
      for (_, v) in outcome.displaced {
        if let CacheFileOrOnMemory::File(old_path) = v.target
          && Some(&old_path) != new_file_path.as_ref()
        {
          debug!("Evicting displaced cache file");
          file_store.evict(&old_path).await;
        }
      }
    }
  }
}

/* ---------------------------------------------- */
#[derive(Debug, Clone)]
/// Cache file manager. Lock-free by design: committed cache files are immutable and live at
/// generation-unique paths, so the only shared mutable state is the best-effort count of
/// committed file-cache objects. Keeping that count in an atomic (instead of a lock held across
/// file I/O) means a store's publish can never queue behind another task's unlink or open -
/// under sustained store-and-evict churn a single slow unlink previously serialized every
/// in-flight publish behind one exclusive lock, stalling publication entirely while
/// committed-but-unpublished files accumulated on disk without bound.
struct FileStore {
  /// Approximate count of committed file-cache objects (best-effort by design; see `count`).
  cnt: Arc<AtomicUsize>,
  /// Async runtime
  runtime_handle: tokio::runtime::Handle,
}

impl FileStore {
  /// Build manager
  async fn new(runtime_handle: &tokio::runtime::Handle) -> Self {
    Self {
      cnt: Arc::new(AtomicUsize::new(0)),
      runtime_handle: runtime_handle.clone(),
    }
  }

  /// Count file cache entries
  async fn count(&self) -> usize {
    self.cnt.load(Ordering::Relaxed)
  }

  /// Account for a newly committed file-cache object whose file is already renamed into place.
  /// Must be called BEFORE the corresponding metadata is published into the LRU, so that a visible
  /// File entry is always already counted (see `publish_cache_object`). That invariant is an
  /// ordering property, not a mutual-exclusion one, so a plain atomic increment upholds it.
  async fn incr_count(&self) {
    self.cnt.fetch_add(1, Ordering::Relaxed);
  }

  /// Evict a counted file cache object, logs warning if removal fails
  async fn evict(&self, path: impl AsRef<Path>) {
    if let Err(e) = self.remove(path).await {
      warn!("Eviction failed during file object removal: {:?}", e);
    }
  }

  /// Remove a counted file-cache object.
  ///
  /// The count is decremented **regardless of whether the unlink succeeds**: the caller has decided
  /// to evict this counted file (its LRU metadata is already gone), so it is no longer a live counted
  /// object even if the file was already removed externally or by the integrity-check path. Only a
  /// genuine I/O error (not "already gone") is surfaced. Otherwise the file count leaks above the
  /// number of live entries. No lock is held across the unlink: the file is immutable at a
  /// generation-unique path, so the I/O needs no exclusion and concurrent removals proceed in
  /// parallel instead of queueing publishers behind one another.
  async fn remove(&self, path: impl AsRef<Path>) -> CacheResult<()> {
    // Saturate rather than underflow: the count and the LRU are updated independently, so a
    // pathological concurrent ordering could otherwise drive a `usize` below zero (wraparound).
    // `incr_count`-before-publish makes this unreachable in practice.
    let _ = self
      .cnt
      .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| Some(c.saturating_sub(1)));
    debug!(
      "Removed a cache file at {:?} (file count: {})",
      path.as_ref(),
      self.cnt.load(Ordering::Relaxed)
    );

    match fs::remove_file(path.as_ref()).await {
      Ok(()) => {}
      // Already gone (e.g. removed externally or by the integrity-check path); the count correction
      // above still stands, so this is not an error.
      Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
      Err(e) => return Err(CacheError::FailedToRemoveCacheFile(e.to_string())),
    }

    Ok(())
  }

  /// Read a stored file-cache object, returns error if the file cannot be opened. The integrity
  /// hash is verified incrementally by the producer task and acted on at EOF.
  async fn read(&self, path: impl AsRef<Path> + Send + Sync + 'static, hash: &Bytes) -> CacheResult<BoundedStreamBody> {
    let Ok(mut file) = File::open(&path).await else {
      warn!("Cache file object cannot be opened");
      return Err(CacheError::FailedToOpenCacheFile);
    };
    let hash_clone = hash.clone();

    let (mut body_tx, body_rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(CACHE_STREAM_CHANNEL_CAPACITY);

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
            // The bounded send awaits a free slot when the consumer lags, so a slow client
            // paces the file read instead of queueing the whole object in memory. It errs only
            // when the receiver is dropped; the early return then skips the integrity check
            // below (an incomplete hash proves nothing) and leaves the file in place.
            let bytes = buf.split().freeze();
            hasher.update(bytes.as_ref());
            body_tx
              .send(Ok(Frame::data(bytes)))
              .await
              .map_err(|e| CacheError::FailedToSendFrameFromCache(e.to_string()))?
          }
          Err(_) => break,
        };
      }
      let hash_bytes = Bytes::copy_from_slice(hasher.finalize().as_ref());
      if hash_bytes != hash_clone {
        warn!("Hash mismatched. Cache object is corrupted. Force to remove the cache file.");
        // Unlink WITHOUT touching the count. The LRU entry pointing at this file still exists;
        // the count is reconciled when that entry is evicted through the metadata path, whose
        // counted removal tolerates the already-missing file. A counted removal here would
        // decrement twice for one object. (This matches the previous behavior, where this path
        // operated on a clone holding a copied counter.)
        remove_uncounted_file(path.as_ref()).await;
        return Err(CacheError::HashMismatchedInCacheFile);
      }
      Ok(()) as CacheResult<()>
    });

    let stream_body = StreamBody::new(body_rx);

    Ok(stream_body)
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

/// Monotonic counter assigning each stored `CacheObject` a unique generation (see `CacheObject`).
static CACHE_OBJECT_GEN: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug)]
/// Cache object definition
struct CacheObject {
  /// Cache policy to determine if the stored cache can be used as a response to a new incoming request
  policy: CachePolicy,
  /// Cache target: on-memory object or temporary file
  target: CacheFileOrOnMemory,
  /// SHA256 hash used to verify file-backed cache targets on read; still computed at store time
  /// before the file/on-memory target is selected. Not consulted on on-memory hits (the object is
  /// an immutable in-process `Bytes`, so there is no external mutation to detect).
  hash: Bytes,
  /// Process-unique generation id assigned at store time. Lets an eviction triggered from a stale
  /// snapshot (e.g. a concurrent `get()`) pop the entry only if it is still the same generation,
  /// so it cannot delete a newer live entry that a concurrent re-store published under the same key.
  generation: u64,
  /// Object data length in bytes (the on-memory `Bytes` length or the committed file length),
  /// captured at store time and used for the total-size accounting in `LruCacheManager`.
  size: usize,
}

impl CacheObject {
  /// Build a cache object, assigning it a fresh generation id.
  fn new(policy: CachePolicy, target: CacheFileOrOnMemory, hash: Bytes, size: usize) -> Self {
    Self {
      policy,
      target,
      hash,
      generation: CACHE_OBJECT_GEN.fetch_add(1, Ordering::Relaxed),
      size,
    }
  }
}

/* ---------------------------------------------- */
/// Recompute the byte total from the live entries with a checked fold. Called only when an
/// arithmetic invariant violation has been detected (the counter and the live set disagree);
/// a corrupted counter can be wrong in EITHER direction, so neither keeping it nor clamping
/// it is safe — only a recount restores a trustworthy value. Returns `None` when even the
/// recount overflows, i.e. the live set itself is no longer representable.
fn recompute_total(lru: &LruCache<String, CacheObject>) -> Option<usize> {
  lru.iter().try_fold(0usize, |acc, (_, v)| acc.checked_add(v.size))
}

/// Outcome of `LruCacheManager::push`: whether the new entry was stored, plus every entry
/// displaced under the lock, whose files the caller evicts after the lock is released.
struct PushOutcome {
  /// `false` when the store was refused: by the preflight (no-fit or unrepresentable
  /// counter, in which case `displaced` is empty), or because the byte accounting was
  /// invalidated (possibly mid-operation, in which case `displaced` holds the entries that
  /// were already popped and still need their files evicted).
  stored: bool,
  /// Entries removed under the lock: same-key replacement, total-size eviction, and
  /// entry-count capacity eviction.
  displaced: Vec<(String, CacheObject)>,
}

#[derive(Debug, Clone)]
/// Lru cache manager that is responsible to handle `Mutex` as an outer of `LruCache`
struct LruCacheManager {
  /// Inner lru cache manager main object
  /// TODO: Revisit the string URL key when adding vhost-aware cache keys or
  /// broader request-attribute checks.
  inner: Arc<Mutex<LruCache<String, CacheObject>>>,
  /// Counter of current cached object (total)
  cnt: Arc<AtomicUsize>,
  /// Sum of `CacheObject::size` over the entries currently in the LRU (the logical live set).
  /// Every mutation happens while holding the LRU mutex, so the in-lock view is the single
  /// authoritative state; lock-free reads are advisory snapshots for logging and tests.
  total_bytes: Arc<AtomicUsize>,
  /// Ceiling on `total_bytes` (`None` = unlimited). Bounds the logical live set only —
  /// in-flight stores, committed-but-unpublished files, displaced files awaiting unlink, and
  /// unlink failures live outside it (see the design doc and README).
  max_total_size: Option<usize>,
  /// Set when an arithmetic invariant violation could not be repaired by recounting the live
  /// entries (`recompute_total` overflowed). Once set, `push` refuses every store up front —
  /// the ceiling can no longer be enforced trustworthily — while reads and evictions keep
  /// serving and cleaning up. Cleared only by rebuilding the cache (restart / config reload).
  accounting_invalid: Arc<std::sync::atomic::AtomicBool>,
}

impl LruCacheManager {
  /// Build LruCache
  fn new(cache_max_entry: usize, max_total_size: Option<usize>) -> Self {
    Self {
      inner: Arc::new(Mutex::new(LruCache::new(
        std::num::NonZeroUsize::new(cache_max_entry).unwrap(),
      ))),
      cnt: Default::default(),
      total_bytes: Default::default(),
      max_total_size,
      accounting_invalid: Default::default(),
    }
  }

  /// Repair a detected accounting invariant violation: recount the live entries and restore
  /// the exact total, or — if even the recount overflows — mark the accounting invalid so
  /// every further store is refused (fail-closed). A corrupted counter can err in either
  /// direction, so the previous value is never kept and never clamped (both can under-report
  /// and let the live set breach the ceiling). Must be called with the LRU lock held.
  /// Returns the recovered exact total, or `None` when the manager was invalidated.
  fn recover_or_invalidate(&self, lru: &LruCache<String, CacheObject>, context: &str) -> Option<usize> {
    match recompute_total(lru) {
      Some(total) => {
        error!(
          "Cache byte accounting invariant violated while {context}; counter restored by recounting live entries ({total} bytes)"
        );
        self.total_bytes.store(total, Ordering::Relaxed);
        Some(total)
      }
      None => {
        error!(
          "Cache byte accounting invariant violated while {context} and the live-set recount itself overflowed; \
           refusing all further cache stores (fail-closed)"
        );
        self.accounting_invalid.store(true, Ordering::Relaxed);
        None
      }
    }
  }

  /// Subtract an evicted entry's bytes from the running total. The entry has ALREADY been
  /// popped, so on an impossible subtraction (invariant violation) the recount over the
  /// remaining entries replaces the subtraction outright. `None` = accounting invalidated.
  fn sub_or_recount(&self, lru: &LruCache<String, CacheObject>, total: usize, size: usize, context: &str) -> Option<usize> {
    match total.checked_sub(size) {
      Some(t) => Some(t),
      None => self.recover_or_invalidate(lru, context),
    }
  }

  /// Count entries
  fn count(&self) -> usize {
    self.cnt.load(Ordering::Relaxed)
  }

  /// Advisory snapshot of the retained live-set bytes (authoritative only under the LRU lock).
  fn total_bytes(&self) -> usize {
    self.total_bytes.load(Ordering::Relaxed)
  }

  /// Evict the entry for `cache_key` only if it is still the `generation` the caller observed.
  ///
  /// Eviction is sometimes triggered from a stale snapshot (e.g. a `get()` that cloned the entry,
  /// then found it stale or failed to read its file). A concurrent re-store may have replaced that
  /// entry with a newer live one under the same key in the meantime; popping unconditionally would
  /// delete the live entry (orphaning its file and desyncing the file count). Peeking the current
  /// generation and only popping on a match prevents that. Returns the popped entry when it matched.
  fn evict_if_generation(&self, cache_key: &str, generation: u64) -> Option<(String, CacheObject)> {
    let mut lock = match self.inner.lock() {
      Ok(lock) => lock,
      Err(_) => {
        error!("Mutex can't be locked to evict a cache entry");
        return None;
      }
    };
    // `peek` does not promote the entry; only pop when the generation still matches.
    if lock.peek(cache_key).map(|o| o.generation) != Some(generation) {
      // Generation mismatch: nothing is popped, so the byte accounting stays untouched.
      return None;
    }
    let res = lock.pop_entry(cache_key);
    if let Some((_, v)) = &res {
      // Mutated under the held lock, keeping the counter authoritative (see field doc).
      // An impossible subtraction is repaired by recounting (or invalidates the accounting).
      let total = self.total_bytes.load(Ordering::Relaxed);
      if let Some(t) = self.sub_or_recount(&lock, total, v.size, "evicting a stale entry") {
        self.total_bytes.store(t, Ordering::Relaxed);
      }
    }
    // This may be inconsistent with the actual number of entries
    self.cnt.store(lock.len(), Ordering::Relaxed);
    res
  }

  /// Push an entry into the LRU cache, enforcing the total-size ceiling. Returns error if the
  /// mutex cannot be acquired.
  ///
  /// Everything happens under ONE lock acquisition, in a strict order that (a) never evicts an
  /// unrelated entry when replacing a same-key predecessor would already make the new object
  /// fit, and (b) refuses fail-closed — without mutating anything — states the counter could
  /// not represent:
  /// 1. Preflight, branched by ceiling. Bounded: refuse only a no-fit object
  ///    (`new_size > ceiling`; unreachable once the constructor clamps `max_each_size`) — the
  ///    pre-eviction projected sum may legitimately overflow even though the object fits after
  ///    step 3, so no addition is checked here; fit is guaranteed by the step-3 loop instead
  ///    (post-loop `total <= ceiling - new_size`, so the step-5 addition cannot overflow).
  ///    Unlimited: no eviction loop runs, so refuse when `(total - old_size) + new_size` is not
  ///    representable (`checked_add`; reachable only at ~16 EiB retained on 64-bit targets).
  ///    A `checked_sub` failure (`total < old_size`) is an invariant violation: the counter
  ///    and the live set disagree, and a corrupted counter can err in EITHER direction (a
  ///    too-small value under-reports and would let the live set breach the ceiling), so the
  ///    counter is repaired by recounting the live entries before the preflight is retried.
  ///    If even the recount overflows, the accounting is invalidated and every store —
  ///    including this one — is refused up front (fail-closed); reads and evictions continue.
  ///    The counter is never kept, clamped, or saturated on a violation.
  /// 2. Same-key removal: pop this key's old entry and free its bytes first.
  /// 3. Total-size eviction: pop LRU entries while the new object does not fit.
  /// 4. Insert; at entry-count capacity this can displace one more entry, accounted the same.
  /// 5. Add the new entry's size — checked; representable whenever the invariant holds.
  ///
  /// Violations detected mid-operation (steps 2-5) are repaired the same way: the failed
  /// subtraction/addition is replaced by a recount of the live set as it stands (the popped
  /// or inserted entry is already reflected in the map, so the recount IS the exact new
  /// total). If invalidated mid-operation, the store is abandoned (`stored = false`) and the
  /// entries popped so far are returned for file cleanup.
  fn push(&self, cache_key: &str, cache_object: &CacheObject) -> CacheResult<PushOutcome> {
    let mut lock = self.inner.lock().map_err(|_| {
      error!("Failed to acquire mutex lock for writing cache entry");
      CacheError::FailedToAcquiredMutexLockForCache
    })?;

    // Accounting already invalidated: the ceiling cannot be enforced trustworthily, so every
    // store is refused before any mutation (the caller rolls back the new file).
    if self.accounting_invalid.load(Ordering::Relaxed) {
      return Ok(PushOutcome {
        stored: false,
        displaced: Vec::new(),
      });
    }

    let new_size = cache_object.size;
    let mut total = self.total_bytes.load(Ordering::Relaxed);

    // Step 1: preflight, before any mutation (with one repair retry on a violation).
    let old_size = lock.peek(cache_key).map(|o| o.size).unwrap_or(0);
    let mut base = total.checked_sub(old_size);
    if base.is_none() {
      // The counter no longer covers a live entry: repair it by recounting, or invalidate.
      let Some(recounted) = self.recover_or_invalidate(&lock, "preparing a store") else {
        return Ok(PushOutcome {
          stored: false,
          displaced: Vec::new(),
        });
      };
      total = recounted;
      // The recount includes the live same-key entry, so this cannot fail again.
      base = total.checked_sub(old_size);
    }
    let refused = match (base, self.max_total_size) {
      (None, _) => true, // unreachable after the repair above; kept as the final guard
      (Some(_), Some(limit)) => new_size > limit,
      (Some(base), None) => base.checked_add(new_size).is_none(),
    };
    if refused {
      return Ok(PushOutcome {
        stored: false,
        displaced: Vec::new(),
      });
    }

    let mut displaced = Vec::new();

    // Step 2: free the same-key predecessor's bytes before evicting anything unrelated.
    // (Underflow here is pre-excluded by the step-1 preflight/repair.)
    if let Some(old) = lock.pop_entry(cache_key) {
      let size = old.1.size;
      displaced.push(old);
      let Some(t) = self.sub_or_recount(&lock, total, size, "replacing a same-key entry") else {
        // Invalidated before the insert: nothing to remove; sync the advisory entry count
        // to the pops that already happened and abandon the store.
        self.cnt.store(lock.len(), Ordering::Relaxed);
        return Ok(PushOutcome {
          stored: false,
          displaced,
        });
      };
      total = t;
    }

    // Step 3: evict LRU entries until the new object fits under the ceiling. (The
    // `saturating_sub` here is only the fit COMPARISON, not the authoritative counter.)
    if let Some(limit) = self.max_total_size {
      while new_size > limit.saturating_sub(total) {
        let Some(lru) = lock.pop_lru() else {
          // An empty LRU with the fit condition still true is impossible under the invariant
          // (empty live set => total == 0 => new_size <= limit per the step-1 preflight), so
          // reaching here proves the counter was corrupted in the OVER-reporting direction -
          // the one corruption the subtraction path cannot detect (`checked_sub` keeps
          // succeeding). Without repair the stale excess would be published into the counter
          // and every later store would evict everything, permanently. Repair it here: the
          // recount of an empty live set is exactly 0 (an empty fold cannot overflow, so the
          // invalidation arm is unreachable and kept only for uniformity), after which the
          // new object fits by the preflight guarantee.
          match self.recover_or_invalidate(&lock, "size-evicting from an empty live set") {
            Some(t) => {
              total = t;
              break;
            }
            None => {
              self.cnt.store(lock.len(), Ordering::Relaxed);
              return Ok(PushOutcome {
                stored: false,
                displaced,
              });
            }
          }
        };
        let size = lru.1.size;
        displaced.push(lru);
        let Some(t) = self.sub_or_recount(&lock, total, size, "size-evicting an LRU entry") else {
          // Invalidated before the insert: same as the step-2 arm above.
          self.cnt.store(lock.len(), Ordering::Relaxed);
          return Ok(PushOutcome {
            stored: false,
            displaced,
          });
        };
        total = t;
      }
    }

    // Step 4: insert; a full-by-entry-count LRU displaces one more entry here.
    if let Some(cap_displaced) = lock.push(cache_key.to_string(), cache_object.clone()) {
      let size = cap_displaced.1.size;
      displaced.push(cap_displaced);
      match total.checked_sub(size) {
        Some(t) => total = t,
        None => {
          // Violation after the insert. On a successful recount (which already includes the
          // new entry) it replaces BOTH this subtraction and the step-5 addition, and the
          // store stands. On invalidation the store is abandoned per the contract above:
          // remove the just-inserted metadata under the same lock (NOT added to `displaced`
          // - the caller's stored=false arm owns the new file's rollback) and refuse.
          match self.recover_or_invalidate(&lock, "capacity-evicting an LRU entry") {
            Some(_) => {
              self.cnt.store(lock.len(), Ordering::Relaxed);
              return Ok(PushOutcome { stored: true, displaced });
            }
            None => {
              let _ = lock.pop_entry(cache_key);
              self.cnt.store(lock.len(), Ordering::Relaxed);
              return Ok(PushOutcome {
                stored: false,
                displaced,
              });
            }
          }
        }
      }
    }

    // Step 5: account the new entry. Representable whenever the invariant holds (step-1
    // preflight on the unlimited path, the step-3 loop on the bounded path). On a violation
    // the recount — which already includes the inserted entry — replaces the addition; if
    // even that fails, the store is abandoned like the step-4 arm (metadata removed, the
    // caller rolls back the new file).
    match total.checked_add(new_size) {
      Some(t) => self.total_bytes.store(t, Ordering::Relaxed),
      None => {
        if self.recover_or_invalidate(&lock, "accounting a stored entry").is_none() {
          let _ = lock.pop_entry(cache_key);
          self.cnt.store(lock.len(), Ordering::Relaxed);
          return Ok(PushOutcome {
            stored: false,
            displaced,
          });
        }
        // On success the recounted total was stored by the helper; the store stands.
      }
    }
    // This may be inconsistent with the actual number of entries
    self.cnt.store(lock.len(), Ordering::Relaxed);
    Ok(PushOutcome { stored: true, displaced })
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

/// Derive the LRU cache key from the URI passed into the cache layer - the upstream-facing
/// request URI seen by the forwarder after the handler's rewrite. Host matching and `Vary`
/// matching are delegated to `http-cache-semantics`. On Host-flattened paths (such as
/// `set_upstream_host` or the `default_app` fallback) the backend must emit an appropriate
/// `Vary` header, or avoid cacheable responses, if forwarded attributes change response content.
fn derive_cache_key_from_uri(uri: &hyper::Uri) -> String {
  uri.to_string()
}

#[cfg(test)]
mod tests {
  use super::*;
  use futures::{StreamExt, stream};
  use std::{
    pin::Pin,
    task::{Context, Poll},
  };

  /// NOTE: the relay channels are bounded. A test that runs the producer to completion BEFORE
  /// draining the receiver (the common pattern below) deadlocks once a body has more frames than
  /// the channel capacity, so these tests create channels with a capacity comfortably above any
  /// test body (16) - except the backpressure tests, which exercise the bound itself and drain
  /// concurrently.
  const TEST_CHANNEL_CAPACITY: usize = 16;

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

  /// Total number of data bytes across forwarded frames (works for any error type).
  fn forwarded_len<E>(frames: Vec<Result<Frame<Bytes>, E>>) -> usize {
    frames
      .into_iter()
      .filter_map(|f| f.ok())
      .filter_map(|f| f.into_data().ok())
      .map(|b| b.len())
      .sum()
  }

  /// Drive the store path for a body that must stay on memory (threshold = `usize::MAX`, so it
  /// never spills to a file) and return the stored bytes if cacheable. Mirrors the old
  /// `spool_body` return shape for the existing on-memory tests.
  async fn store_on_memory<B, E>(body: B, body_tx: mpsc::Sender<Result<Frame<Bytes>, E>>, max_each_size: usize) -> Option<Bytes>
  where
    B: hyper::body::Body<Data = Bytes, Error = E> + Unpin,
  {
    let uri: Uri = "http://example.com/onmem".parse().unwrap();
    spool_and_store(body, body_tx, max_each_size, usize::MAX, &std::env::temp_dir(), &uri)
      .await
      .map(|(target, _hash, _size)| match target {
        CacheFileOrOnMemory::OnMemory(bytes) => bytes,
        CacheFileOrOnMemory::File(_) => unreachable!("usize::MAX on-memory threshold never spills"),
      })
  }

  /// Unique, freshly created temp directory for file-cache store tests.
  async fn temp_cache_dir(tag: &str) -> PathBuf {
    let dir = temp_cache_path(tag);
    fs::create_dir_all(&dir).await.unwrap();
    dir
  }

  /// A fresh, storable cache policy for `uri` (so the freshness gate passes).
  fn fresh_policy(uri: &Uri) -> CachePolicy {
    let req = Request::builder().uri(uri.clone()).body(()).unwrap();
    let res = Response::builder()
      .header("cache-control", "public, max-age=3600")
      .body(())
      .unwrap();
    get_policy_if_cacheable(Some(&req), Some(&res)).unwrap().unwrap()
  }

  /// In-memory `FileStore` for store-path tests (no dir cleanup at construction).
  fn test_file_store() -> FileStore {
    FileStore {
      cnt: Arc::new(AtomicUsize::new(0)),
      runtime_handle: tokio::runtime::Handle::current(),
    }
  }

  #[tokio::test]
  async fn within_limit_caches_and_forwards_all() {
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(TEST_CHANNEL_CAPACITY);
    let body = body_from(vec![data_frame(b"hello"), data_frame(b"world")]);
    let cached = store_on_memory(body, tx, 1024).await;
    assert_eq!(cached.as_deref(), Some(&b"helloworld"[..]));
    assert_eq!(forwarded_data(rx.collect::<Vec<_>>().await), b"helloworld");
  }

  /// Regression test for the truncation bug: an over-limit cacheable response must still be
  /// forwarded to the client in full, just not cached.
  #[tokio::test]
  async fn over_limit_forwards_all_but_does_not_cache() {
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(TEST_CHANNEL_CAPACITY);
    // three 5-byte frames = 15 bytes total, over the 8-byte limit
    let body = body_from(vec![data_frame(b"aaaaa"), data_frame(b"bbbbb"), data_frame(b"ccccc")]);
    let cached = store_on_memory(body, tx, 8).await;
    assert!(cached.is_none(), "over-limit object must not be cached");
    assert_eq!(
      forwarded_data(rx.collect::<Vec<_>>().await),
      b"aaaaabbbbbccccc",
      "all frames must be forwarded, not truncated"
    );
  }

  #[tokio::test]
  async fn boundary_exactly_max_is_cached() {
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(TEST_CHANNEL_CAPACITY);
    // 5 + 3 = 8 == limit (matches the original `size > max_each_size` boundary)
    let body = body_from(vec![data_frame(b"aaaaa"), data_frame(b"bbb")]);
    let cached = store_on_memory(body, tx, 8).await;
    assert_eq!(cached.as_deref(), Some(&b"aaaaabbb"[..]));
    assert_eq!(forwarded_data(rx.collect::<Vec<_>>().await), b"aaaaabbb");
  }

  /// This only pins down trailer *forwarding* and that `buf` holds data bytes only. It does
  /// not assert that trailer-bearing responses are cacheable as a spec (a cache hit does not
  /// reproduce trailers); that is pre-existing behaviour and out of scope (design doc 3/8).
  #[tokio::test]
  async fn forwards_trailers_without_buffering_them() {
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(TEST_CHANNEL_CAPACITY);
    let mut trailers = http::HeaderMap::new();
    trailers.insert("x-trailer", http::HeaderValue::from_static("v"));
    let body = body_from(vec![data_frame(b"data"), Ok(Frame::trailers(trailers))]);
    let cached = store_on_memory(body, tx, 1024).await;
    assert_eq!(cached.as_deref(), Some(&b"data"[..]));
    let forwarded = rx.collect::<Vec<_>>().await;
    assert_eq!(forwarded.len(), 2);
    assert!(forwarded[1].as_ref().unwrap().is_trailers());
  }

  #[tokio::test]
  async fn returns_none_when_downstream_dropped() {
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(TEST_CHANNEL_CAPACITY);
    drop(rx);
    let body = body_from(vec![data_frame(b"a"), data_frame(b"b")]);
    let cached = store_on_memory(body, tx, 1024).await;
    assert!(cached.is_none());
  }

  /// Stand-in body error type. `hyper::Error` has no public constructor, so the error path is
  /// exercised with a body whose `Error` is this type; `spool_and_store` is generic over the error.
  #[derive(Debug)]
  struct TestBodyError;

  /// Regression test for the upstream-error path: an error frame must be propagated downstream
  /// (not masked as a clean EOF), and the object must not be cached.
  #[tokio::test]
  async fn upstream_error_is_propagated_and_not_cached() {
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, TestBodyError>>(TEST_CHANNEL_CAPACITY);
    let frames: Vec<Result<Frame<Bytes>, TestBodyError>> =
      vec![Ok(Frame::data(Bytes::from_static(b"partial"))), Err(TestBodyError)];
    let body = StreamBody::new(stream::iter(frames));
    let cached = store_on_memory(body, tx, 1024).await;
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

    let file_store = FileStore {
      cnt: Arc::new(AtomicUsize::new(0)),
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

    let file_store = FileStore {
      cnt: Arc::new(AtomicUsize::new(1)), // the entry for this file is still counted
      runtime_handle: tokio::runtime::Handle::current(),
    };
    let body = file_store.read(path.clone(), &wrong_hash).await.unwrap();
    let _ = BodyExt::collect(body).await; // drain to EOF; data frames are all Ok, the mismatch is internal
    assert!(
      fs::metadata(&path).await.is_err(),
      "a corrupted cache file must be removed on hash mismatch"
    );
    // The mismatch path unlinks WITHOUT decrementing: the LRU entry still exists and the count
    // is reconciled when that entry is evicted through the metadata path (no double decrement).
    assert_eq!(
      file_store.count().await,
      1,
      "the integrity-check removal must not touch the count"
    );
  }

  /// An on-memory cache hit serves the stored object directly, without re-hashing it. The entry is
  /// inserted with an intentionally wrong `hash`: the previous per-hit re-hash would have detected
  /// the mismatch, evicted the entry, and returned `None`; the immutable in-process object is now
  /// trusted and returned as-is. Drives `get()` end-to-end and asserts the served body.
  ///
  /// The entry is inserted directly via the cache manager rather than through `put()`: `put()`
  /// spawns a background task that returns the downstream stream first and only pushes the cache
  /// entry afterwards, so an immediate `get()` would race. Direct insertion is deterministic.
  #[tokio::test]
  async fn on_memory_hit_serves_object_without_rehash() {
    let cache = RpxyCache {
      inner: LruCacheManager::new(10, None),
      file_store: FileStore {
        cnt: Arc::new(AtomicUsize::new(0)),
        runtime_handle: tokio::runtime::Handle::current(),
      },
      runtime_handle: tokio::runtime::Handle::current(),
      max_each_size: 65_535,
      max_each_size_on_memory: 4_096,
      cache_dir: std::env::temp_dir(),
    };

    let uri: Uri = "http://example.com/onmem".parse().unwrap();
    let object = Bytes::from_static(b"on-memory cached body");

    // Build a fresh, storable policy so get()'s freshness gate (policy.before_request) is Fresh.
    let policy_req = Request::builder().uri(uri.clone()).body(()).unwrap();
    let policy_res = Response::builder()
      .header("cache-control", "public, max-age=3600")
      .body(())
      .unwrap();
    let policy = get_policy_if_cacheable(Some(&policy_req), Some(&policy_res))
      .unwrap()
      .unwrap();

    let cache_object = CacheObject::new(
      policy,
      CacheFileOrOnMemory::OnMemory(object.clone()),
      // Intentionally wrong: an on-memory hit must not consult this hash.
      Bytes::from_static(&[0u8; 32]),
      object.len(),
    );
    let cache_key = derive_cache_key_from_uri(&uri);
    cache.inner.push(&cache_key, &cache_object).unwrap();

    let req = Request::builder().uri(uri.clone()).body(()).unwrap();
    let response = cache.get(&req).await.expect("an on-memory hit must return a response");
    let got = BodyExt::collect(response.into_body()).await.unwrap().to_bytes();
    assert_eq!(
      got, object,
      "an on-memory hit must serve the stored object even with a stale hash"
    );
  }

  /// A body larger than the on-memory threshold spills to a file; the committed file is renamed
  /// into `cache_dir`, the whole body is forwarded downstream, and reading the file back with the
  /// returned hash succeeds (proving the incremental hash matches a one-shot hash of the bytes).
  #[tokio::test]
  async fn store_spills_large_object_to_file_and_round_trips() {
    let dir = temp_cache_dir("spill").await;
    let uri: Uri = "http://example.com/big".parse().unwrap();
    // 5000 + 5000 = 10000 bytes, well over the 4096 on-memory threshold and under max_each_size.
    let chunk = vec![7u8; 5000];
    let frames: Vec<Result<Frame<Bytes>, TestBodyError>> = vec![
      Ok(Frame::data(Bytes::from(chunk.clone()))),
      Ok(Frame::data(Bytes::from(chunk.clone()))),
    ];
    let body = StreamBody::new(stream::iter(frames));
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, TestBodyError>>(TEST_CHANNEL_CAPACITY);

    let (target, hash, size) = spool_and_store(body, tx, 1_000_000, 4096, &dir, &uri)
      .await
      .expect("a within-limit object must be cacheable");
    let CacheFileOrOnMemory::File(path) = target else {
      panic!("an over-threshold object must spill to a file target");
    };
    assert!(path.starts_with(&dir), "the committed file must live in cache_dir");
    assert_eq!(size, 10000, "the accounted size is the full data length");
    assert_eq!(
      forwarded_len(rx.collect::<Vec<_>>().await),
      10000,
      "the whole body is forwarded"
    );

    // Read the committed file back, verifying integrity against the incrementally computed hash.
    let file_store = FileStore {
      cnt: Arc::new(AtomicUsize::new(0)),
      runtime_handle: tokio::runtime::Handle::current(),
    };
    let read_body = file_store.read(path.clone(), &hash).await.unwrap();
    let got = BodyExt::collect(read_body).await.unwrap().to_bytes();
    assert_eq!(got.len(), 10000);
    assert!(got.iter().all(|&b| b == 7), "round-tripped bytes must match");
    assert!(fs::metadata(&path).await.is_ok(), "a verified file is left in place");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// A body within the on-memory threshold stays on memory and no file is created.
  #[tokio::test]
  async fn store_keeps_small_object_on_memory() {
    let dir = temp_cache_dir("onmem-store").await;
    let uri: Uri = "http://example.com/small".parse().unwrap();
    let (tx, _rx) = mpsc::channel::<Result<Frame<Bytes>, hyper::Error>>(TEST_CHANNEL_CAPACITY);
    let body = body_from(vec![data_frame(b"tiny")]);

    let (target, _hash, size) = spool_and_store(body, tx, 1_000_000, 4096, &dir, &uri)
      .await
      .expect("cacheable");
    assert!(
      matches!(target, CacheFileOrOnMemory::OnMemory(ref b) if b.as_ref() == b"tiny"),
      "a sub-threshold object must stay on memory"
    );
    assert_eq!(size, 4, "the accounted size is the data length");
    let mut entries = fs::read_dir(&dir).await.unwrap();
    assert!(
      entries.next_entry().await.unwrap().is_none(),
      "no file must be created for an on-memory object"
    );
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// A single frame larger than the on-memory threshold spills directly to a file (the buffer is
  /// not first grown by the whole frame), forwards in full, and round-trips intact. Guards the
  /// spill-first-on-threshold-crossing path that bounds store-path memory against a large frame.
  #[tokio::test]
  async fn store_single_large_frame_spills_directly() {
    let dir = temp_cache_dir("single-large").await;
    let uri: Uri = "http://example.com/onebig".parse().unwrap();
    let data = vec![3u8; 50_000]; // one frame, well over the 4096 threshold
    let frames: Vec<Result<Frame<Bytes>, TestBodyError>> = vec![Ok(Frame::data(Bytes::from(data)))];
    let body = StreamBody::new(stream::iter(frames));
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, TestBodyError>>(TEST_CHANNEL_CAPACITY);

    let (target, hash, _size) = spool_and_store(body, tx, 1_000_000, 4096, &dir, &uri)
      .await
      .expect("cacheable");
    let CacheFileOrOnMemory::File(path) = target else {
      panic!("a single frame over the threshold must spill to a file target");
    };
    assert_eq!(
      forwarded_len(rx.collect::<Vec<_>>().await),
      50_000,
      "the whole frame is forwarded"
    );

    let file_store = FileStore {
      cnt: Arc::new(AtomicUsize::new(0)),
      runtime_handle: tokio::runtime::Handle::current(),
    };
    let read_body = file_store.read(path.clone(), &hash).await.unwrap();
    let got = BodyExt::collect(read_body).await.unwrap().to_bytes();
    assert_eq!(got.len(), 50_000);
    assert!(got.iter().all(|&b| b == 3), "round-tripped bytes must match");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// Exceeding `max_each_size` *after* a spill keeps forwarding the full body, caches nothing, and
  /// leaves no temp file behind.
  #[tokio::test]
  async fn store_too_large_after_spill_forwards_all_and_leaves_no_file() {
    let dir = temp_cache_dir("toolarge").await;
    let uri: Uri = "http://example.com/big".parse().unwrap();
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, TestBodyError>>(TEST_CHANNEL_CAPACITY);
    // on-memory 4096, max_each_size 8000: 4000 (M) -> 8000 (spill) -> 12000 (too large).
    let frame = |n: usize| Ok(Frame::data(Bytes::from(vec![1u8; n])));
    let frames: Vec<Result<Frame<Bytes>, TestBodyError>> = vec![frame(4000), frame(4000), frame(4000)];
    let body = StreamBody::new(stream::iter(frames));

    let out = spool_and_store(body, tx, 8000, 4096, &dir, &uri).await;
    assert!(out.is_none(), "an over-limit object must not be cached");
    assert_eq!(forwarded_len(rx.collect::<Vec<_>>().await), 12000, "all bytes are forwarded");
    let mut entries = fs::read_dir(&dir).await.unwrap();
    assert!(
      entries.next_entry().await.unwrap().is_none(),
      "the temp file must be cleaned up on abort"
    );
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// An upstream error after a spill forwards the error, caches nothing, and cleans up the temp.
  #[tokio::test]
  async fn store_upstream_error_after_spill_forwards_and_cleans_temp() {
    let dir = temp_cache_dir("err-spill").await;
    let uri: Uri = "http://example.com/big".parse().unwrap();
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, TestBodyError>>(TEST_CHANNEL_CAPACITY);
    let frames: Vec<Result<Frame<Bytes>, TestBodyError>> =
      vec![Ok(Frame::data(Bytes::from(vec![1u8; 5000]))), Err(TestBodyError)];
    let body = StreamBody::new(stream::iter(frames));

    let out = spool_and_store(body, tx, 1_000_000, 4096, &dir, &uri).await;
    assert!(out.is_none(), "an errored body must not be cached");
    let forwarded = rx.collect::<Vec<_>>().await;
    assert!(
      forwarded.iter().any(|f| f.is_err()),
      "the upstream error is forwarded downstream"
    );
    let mut entries = fs::read_dir(&dir).await.unwrap();
    assert!(
      entries.next_entry().await.unwrap().is_none(),
      "the temp file must be cleaned up after an upstream error"
    );
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// A store-side I/O failure (here: a non-existent cache_dir so the spill cannot be created) must
  /// never cut the downstream relay: the full body is still forwarded, and nothing is cached.
  #[tokio::test]
  async fn store_io_failure_keeps_forwarding_without_caching() {
    let dir = temp_cache_path("missing-dir"); // intentionally NOT created
    let uri: Uri = "http://example.com/big".parse().unwrap();
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, TestBodyError>>(TEST_CHANNEL_CAPACITY);
    let frames: Vec<Result<Frame<Bytes>, TestBodyError>> = vec![
      Ok(Frame::data(Bytes::from(vec![9u8; 5000]))),
      Ok(Frame::data(Bytes::from(vec![9u8; 1000]))),
    ];
    let body = StreamBody::new(stream::iter(frames));

    let out = spool_and_store(body, tx, 1_000_000, 4096, &dir, &uri).await;
    assert!(out.is_none(), "a failed store must not cache");
    assert_eq!(
      forwarded_len(rx.collect::<Vec<_>>().await),
      6000,
      "the full body must still reach downstream despite the store I/O failure"
    );
  }

  /// Re-storing the same key with a new file evicts the old generation's file (no orphan) and the
  /// file count stays at one.
  #[tokio::test]
  async fn publish_same_key_file_update_evicts_old_file() {
    let dir = temp_cache_dir("pub-ff").await;
    let manager = LruCacheManager::new(10, None);
    let file_store = test_file_store();
    let uri: Uri = "http://example.com/x".parse().unwrap();
    let key = derive_cache_key_from_uri(&uri);

    let path_a = dir.join("file-a");
    fs::write(&path_a, b"AAAA").await.unwrap();
    let obj_a = CacheObject::new(
      fresh_policy(&uri),
      CacheFileOrOnMemory::File(path_a.clone()),
      Bytes::from_static(&[1u8; 32]),
      4,
    );
    publish_cache_object(&manager, &file_store, &key, obj_a).await;
    assert_eq!(file_store.count().await, 1);

    let path_b = dir.join("file-b");
    fs::write(&path_b, b"BBBB").await.unwrap();
    let obj_b = CacheObject::new(
      fresh_policy(&uri),
      CacheFileOrOnMemory::File(path_b.clone()),
      Bytes::from_static(&[2u8; 32]),
      4,
    );
    publish_cache_object(&manager, &file_store, &key, obj_b).await;

    assert!(
      fs::metadata(&path_a).await.is_err(),
      "the old generation's file must be evicted"
    );
    assert!(fs::metadata(&path_b).await.is_ok(), "the new file remains");
    assert_eq!(file_store.count().await, 1, "exactly one committed file");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// Re-storing the same key as an on-memory object evicts the previously committed file and drops
  /// the file count back to zero (Phase-M displaced-file eviction).
  #[tokio::test]
  async fn publish_file_then_on_memory_evicts_old_file() {
    let dir = temp_cache_dir("pub-fm").await;
    let manager = LruCacheManager::new(10, None);
    let file_store = test_file_store();
    let uri: Uri = "http://example.com/x".parse().unwrap();
    let key = derive_cache_key_from_uri(&uri);

    let path_a = dir.join("file-a");
    fs::write(&path_a, b"AAAA").await.unwrap();
    let obj_a = CacheObject::new(
      fresh_policy(&uri),
      CacheFileOrOnMemory::File(path_a.clone()),
      Bytes::from_static(&[1u8; 32]),
      4,
    );
    publish_cache_object(&manager, &file_store, &key, obj_a).await;
    assert_eq!(file_store.count().await, 1);

    let obj_b = CacheObject::new(
      fresh_policy(&uri),
      CacheFileOrOnMemory::OnMemory(Bytes::from_static(b"small")),
      Bytes::from_static(&[2u8; 32]),
      5,
    );
    publish_cache_object(&manager, &file_store, &key, obj_b).await;

    assert!(fs::metadata(&path_a).await.is_err(), "the displaced file must be evicted");
    assert_eq!(file_store.count().await, 0, "the file count drops back to zero");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// Capacity eviction across keys also evicts the displaced file (insurance for the shared
  /// displaced-file rule on a different-key eviction, not just a same-key update).
  #[tokio::test]
  async fn publish_capacity_eviction_removes_displaced_file() {
    let dir = temp_cache_dir("pub-cap").await;
    let manager = LruCacheManager::new(1, None); // capacity 1: the second push evicts the first
    let file_store = test_file_store();

    let uri_x: Uri = "http://example.com/x".parse().unwrap();
    let path_a = dir.join("file-a");
    fs::write(&path_a, b"AAAA").await.unwrap();
    let obj_a = CacheObject::new(
      fresh_policy(&uri_x),
      CacheFileOrOnMemory::File(path_a.clone()),
      Bytes::from_static(&[1u8; 32]),
      4,
    );
    publish_cache_object(&manager, &file_store, &derive_cache_key_from_uri(&uri_x), obj_a).await;
    assert_eq!(file_store.count().await, 1);

    let uri_y: Uri = "http://example.com/y".parse().unwrap();
    let obj_b = CacheObject::new(
      fresh_policy(&uri_y),
      CacheFileOrOnMemory::OnMemory(Bytes::from_static(b"small")),
      Bytes::from_static(&[2u8; 32]),
      5,
    );
    publish_cache_object(&manager, &file_store, &derive_cache_key_from_uri(&uri_y), obj_b).await;

    assert!(
      fs::metadata(&path_a).await.is_err(),
      "the capacity-evicted file must be removed"
    );
    assert_eq!(file_store.count().await, 0, "no committed files remain");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// On-memory `CacheObject` with an explicit accounted `size` (the actual payload stays tiny),
  /// for total-size accounting tests - including near-`usize::MAX` sizes - without allocating
  /// real data.
  fn sized_object(uri: &Uri, size: usize) -> CacheObject {
    CacheObject::new(
      fresh_policy(uri),
      CacheFileOrOnMemory::OnMemory(Bytes::from_static(b"x")),
      Bytes::from_static(&[0u8; 32]),
      size,
    )
  }

  /// Parse a URI and derive its cache key (shorthand for the ceiling tests).
  fn key_of(uri: &str) -> (Uri, String) {
    let uri: Uri = uri.parse().unwrap();
    let key = derive_cache_key_from_uri(&uri);
    (uri, key)
  }

  /// The per-entry limits are clamped to the total ceiling first, then transitively to each
  /// other; without a ceiling the pre-existing behavior is unchanged.
  #[test]
  fn per_entry_limits_clamp_to_total_ceiling_transitively() {
    // Ceiling below both per-entry limits: both clamp down to it.
    assert_eq!(effective_per_entry_limits(65_535, 65_535, Some(100)), (100, 100));
    // Ceiling above the per-entry limit: untouched.
    assert_eq!(effective_per_entry_limits(100, 50, Some(1000)), (100, 50));
    // No ceiling: only the pre-existing on-memory clamp applies.
    assert_eq!(effective_per_entry_limits(100, 200, None), (100, 100));
    assert_eq!(effective_per_entry_limits(65_535, 4_096, None), (65_535, 4_096));
  }

  /// Publishing beyond the ceiling pops LRU entries until the new object fits; the popped
  /// file-backed entry's file is removed and the byte accounting converges. (design test 3)
  #[tokio::test]
  async fn ceiling_eviction_pops_lru_until_new_fits() {
    let dir = temp_cache_dir("ceil-basic").await;
    let manager = LruCacheManager::new(10, Some(100));
    let file_store = test_file_store();

    let (uri_a, key_a) = key_of("http://example.com/a");
    let path_a = dir.join("file-a");
    fs::write(&path_a, vec![0u8; 60]).await.unwrap();
    let obj_a = CacheObject::new(
      fresh_policy(&uri_a),
      CacheFileOrOnMemory::File(path_a.clone()),
      Bytes::from_static(&[1u8; 32]),
      60,
    );
    publish_cache_object(&manager, &file_store, &key_a, obj_a).await;
    assert_eq!(manager.total_bytes(), 60);

    // 60 + 50 > 100: publishing B must size-evict A.
    let (uri_b, key_b) = key_of("http://example.com/b");
    publish_cache_object(&manager, &file_store, &key_b, sized_object(&uri_b, 50)).await;

    assert!(fs::metadata(&path_a).await.is_err(), "the size-evicted file must be removed");
    assert_eq!(file_store.count().await, 0);
    assert_eq!(manager.total_bytes(), 50, "only the new entry remains accounted");
    assert!(manager.get(&key_a).unwrap().is_none(), "the evicted entry is gone");
    assert!(manager.get(&key_b).unwrap().is_some(), "the new entry is stored");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// One publish can displace several small entries (the `Vec` displaced path). (design test 4)
  #[tokio::test]
  async fn ceiling_multi_displacement_pops_several_entries() {
    let manager = LruCacheManager::new(10, Some(100));
    for (uri_str, size) in [
      ("http://example.com/a", 30usize),
      ("http://example.com/b", 30),
      ("http://example.com/c", 30),
    ] {
      let (uri, key) = key_of(uri_str);
      let outcome = manager.push(&key, &sized_object(&uri, size)).unwrap();
      assert!(outcome.stored);
    }
    assert_eq!(manager.total_bytes(), 90);

    let (uri_d, key_d) = key_of("http://example.com/d");
    let outcome = manager.push(&key_d, &sized_object(&uri_d, 95)).unwrap();
    assert!(outcome.stored);
    assert_eq!(outcome.displaced.len(), 3, "one publish displaces all three small entries");
    assert_eq!(manager.total_bytes(), 95);
    assert_eq!(manager.count(), 1);
  }

  /// A same-key replacement frees its predecessor's bytes BEFORE any unrelated entry is
  /// considered: with ceiling=100, A=10 (older) and K=80, replacing K with K'=90 must evict
  /// nothing but old K - A survives. (design test 5, codex round-1 blocker 1)
  #[tokio::test]
  async fn same_key_replacement_evicts_only_predecessor() {
    let dir = temp_cache_dir("ceil-samekey").await;
    let manager = LruCacheManager::new(10, Some(100));
    let file_store = test_file_store();

    let (uri_a, key_a) = key_of("http://example.com/a");
    publish_cache_object(&manager, &file_store, &key_a, sized_object(&uri_a, 10)).await;

    let (uri_k, key_k) = key_of("http://example.com/k");
    let path_k = dir.join("file-k");
    fs::write(&path_k, vec![0u8; 80]).await.unwrap();
    let obj_k = CacheObject::new(
      fresh_policy(&uri_k),
      CacheFileOrOnMemory::File(path_k.clone()),
      Bytes::from_static(&[2u8; 32]),
      80,
    );
    publish_cache_object(&manager, &file_store, &key_k, obj_k).await;
    assert_eq!(manager.total_bytes(), 90);

    // Replace K with K'=90: old K's 80 bytes free first, so A (LRU-oldest) must survive.
    publish_cache_object(&manager, &file_store, &key_k, sized_object(&uri_k, 90)).await;

    assert!(
      manager.get(&key_a).unwrap().is_some(),
      "the unrelated LRU-oldest entry must survive a same-key replacement"
    );
    assert!(
      fs::metadata(&path_k).await.is_err(),
      "the replaced predecessor's file is evicted"
    );
    assert_eq!(file_store.count().await, 0);
    assert_eq!(manager.total_bytes(), 100, "A(10) + K'(90) exactly fill the ceiling");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// A same-key replacement subtracts the old size and adds the new one. (design test 6)
  #[tokio::test]
  async fn same_key_replacement_adjusts_total_bytes() {
    let manager = LruCacheManager::new(10, None);
    let (uri, key) = key_of("http://example.com/x");
    assert!(manager.push(&key, &sized_object(&uri, 40)).unwrap().stored);
    assert_eq!(manager.total_bytes(), 40);

    let outcome = manager.push(&key, &sized_object(&uri, 15)).unwrap();
    assert!(outcome.stored);
    assert_eq!(outcome.displaced.len(), 1, "the predecessor is displaced");
    assert_eq!(manager.total_bytes(), 15, "old size subtracted, new size added");
    assert_eq!(manager.count(), 1);
  }

  /// An entry displaced by the LRU's entry-count capacity (not by size) is byte-accounted the
  /// same way: final total = old_total - displaced_size + new_size. (design test 7)
  #[tokio::test]
  async fn capacity_displacement_is_size_accounted() {
    let manager = LruCacheManager::new(2, Some(1000));
    let (uri_a, key_a) = key_of("http://example.com/a");
    let (uri_b, key_b) = key_of("http://example.com/b");
    assert!(manager.push(&key_a, &sized_object(&uri_a, 10)).unwrap().stored);
    assert!(manager.push(&key_b, &sized_object(&uri_b, 20)).unwrap().stored);
    let old_total = manager.total_bytes();
    assert_eq!(old_total, 30);

    // Total is far below the ceiling; the displacement is purely by entry-count capacity (2).
    let (uri_c, key_c) = key_of("http://example.com/c");
    let outcome = manager.push(&key_c, &sized_object(&uri_c, 40)).unwrap();
    assert!(outcome.stored);
    assert_eq!(outcome.displaced.len(), 1, "the LRU entry is displaced by capacity");
    assert_eq!(
      manager.total_bytes(),
      old_total - 10 + 40,
      "final total must equal old_total - displaced_size + new_size"
    );
    assert_eq!(manager.count(), 2);
  }

  /// `evict_if_generation` subtracts the entry's bytes on a generation match only; a stale
  /// generation pops nothing and subtracts nothing. (design test 8)
  #[tokio::test]
  async fn evict_if_generation_subtracts_only_on_match() {
    let manager = LruCacheManager::new(10, None);
    let (uri, key) = key_of("http://example.com/x");
    let obj_a = sized_object(&uri, 25);
    let gen_a = obj_a.generation;
    assert!(manager.push(&key, &obj_a).unwrap().stored);
    let obj_b = sized_object(&uri, 35);
    let gen_b = obj_b.generation;
    assert!(manager.push(&key, &obj_b).unwrap().stored);
    assert_eq!(manager.total_bytes(), 35);

    assert!(manager.evict_if_generation(&key, gen_a).is_none());
    assert_eq!(manager.total_bytes(), 35, "a generation mismatch must subtract nothing");

    assert!(manager.evict_if_generation(&key, gen_b).is_some());
    assert_eq!(manager.total_bytes(), 0, "a generation match subtracts the entry's bytes");
  }

  /// After a hash-mismatch unlink (uncounted by design), the LRU entry keeps accounting the
  /// bytes; the next access converges the entry count, the file count, and the byte total
  /// exactly once. Driven end-to-end through `RpxyCache::get` — the first hit drains the
  /// corrupted file to EOF (mismatch unlink), the SECOND hit takes get()'s read-failure
  /// branch (open failure -> generation-matched eviction + counted removal) and returns
  /// `None` — so this fails if get() ever stops performing the convergence itself.
  /// (design test 9, codex impl-review round-1 item 1)
  #[tokio::test]
  async fn hash_mismatch_accounting_converges_exactly_once() {
    let dir = temp_cache_dir("ceil-mismatch").await;
    let cache = RpxyCache {
      inner: LruCacheManager::new(10, None),
      file_store: test_file_store(),
      runtime_handle: tokio::runtime::Handle::current(),
      max_each_size: 65_535,
      max_each_size_on_memory: 4_096,
      cache_dir: dir.clone(),
    };

    // Unrelated entry: a double subtraction would drive the total below its size.
    let (uri_o, key_o) = key_of("http://example.com/other");
    publish_cache_object(&cache.inner, &cache.file_store, &key_o, sized_object(&uri_o, 7)).await;

    // File-backed entry whose recorded hash does not match its on-disk content.
    let (uri, key) = key_of("http://example.com/corrupt");
    let path = dir.join("file-corrupt");
    fs::write(&path, vec![9u8; 20]).await.unwrap();
    let obj = CacheObject::new(
      fresh_policy(&uri),
      CacheFileOrOnMemory::File(path.clone()),
      Bytes::from_static(&[0u8; 32]), // wrong hash: the first hit must detect the mismatch
      20,
    );
    publish_cache_object(&cache.inner, &cache.file_store, &key, obj).await;
    assert_eq!(cache.file_store.count().await, 1);
    assert_eq!(cache.inner.total_bytes(), 27);

    // First hit: served from the file; the integrity check at EOF unlinks the file WITHOUT
    // touching any counter (the producer task holds the channel open until the unlink is
    // done, so draining to EOF is sufficient synchronization).
    let req = Request::builder().uri(uri.clone()).body(()).unwrap();
    let response = cache.get(&req).await.expect("the first hit serves the (corrupt) file");
    let _ = BodyExt::collect(response.into_body()).await;
    assert!(fs::metadata(&path).await.is_err(), "the corrupted file is unlinked");
    assert_eq!(
      cache.file_store.count().await,
      1,
      "the uncounted unlink must not touch the file count"
    );
    assert_eq!(cache.inner.total_bytes(), 27, "the live LRU entry still accounts its bytes");
    assert_eq!(cache.inner.count(), 2);

    // Second hit: the file cannot be opened; get() itself must evict the metadata
    // (generation-matched) and the counted file, converging all three exactly once.
    assert!(cache.get(&req).await.is_none(), "the second hit must fail closed");
    assert_eq!(cache.file_store.count().await, 0);
    assert_eq!(
      cache.inner.total_bytes(),
      7,
      "exactly one subtraction; the unrelated entry remains accounted"
    );
    assert_eq!(cache.inner.count(), 1);

    // Third hit is a plain miss: no double subtraction, and the unrelated entry still serves.
    assert!(cache.get(&req).await.is_none());
    assert_eq!(cache.inner.total_bytes(), 7);
    let req_o = Request::builder().uri(uri_o.clone()).body(()).unwrap();
    assert!(cache.get(&req_o).await.is_some(), "the unrelated entry is untouched");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// codex round-2 counterexample: a corrupted (under-reporting) counter must not let the
  /// live set breach the ceiling. ceiling=200, live A=100 + B=100, counter corrupted to 50;
  /// evicting A hits the impossible subtraction and RECOUNTS the live entries (keeping or
  /// clamping the old value would under-report B and let a 150-byte push skip B's eviction,
  /// ending with a 250-byte live set). After the recount, pushing C=150 must evict B.
  /// (codex impl-review round-2 item 1)
  #[tokio::test]
  async fn corrupted_counter_recovers_by_recount_and_keeps_ceiling() {
    let manager = LruCacheManager::new(10, Some(200));
    let (uri_a, key_a) = key_of("http://example.com/a");
    let obj_a = sized_object(&uri_a, 100);
    let gen_a = obj_a.generation;
    assert!(manager.push(&key_a, &obj_a).unwrap().stored);
    let (uri_b, key_b) = key_of("http://example.com/b");
    assert!(manager.push(&key_b, &sized_object(&uri_b, 100)).unwrap().stored);
    assert_eq!(manager.total_bytes(), 200);

    // Simulate corruption: the counter under-reports the live set.
    manager.total_bytes.store(50, Ordering::Relaxed);

    // Evicting A cannot subtract 100 from 50; the recount restores the exact remainder (B).
    assert!(manager.evict_if_generation(&key_a, gen_a).is_some());
    assert_eq!(manager.total_bytes(), 100, "the recount restores the exact live total");

    // With the repaired counter, C=150 must displace B to fit under the 200-byte ceiling.
    let (uri_c, key_c) = key_of("http://example.com/c");
    let outcome = manager.push(&key_c, &sized_object(&uri_c, 150)).unwrap();
    assert!(outcome.stored);
    assert_eq!(outcome.displaced.len(), 1, "B is size-evicted");
    assert!(manager.get(&key_b).unwrap().is_none(), "B must not survive");
    assert_eq!(manager.total_bytes(), 150, "the live set stays under the ceiling");
  }

  /// A violation detected by the same-key preflight is repaired by recounting BEFORE any
  /// mutation, and the push then proceeds with exact accounting.
  /// (codex impl-review round-2 item 1)
  #[tokio::test]
  async fn preflight_violation_repairs_by_recount_and_proceeds() {
    let manager = LruCacheManager::new(10, None);
    let (uri, key) = key_of("http://example.com/x");
    assert!(manager.push(&key, &sized_object(&uri, 100)).unwrap().stored);

    // Corrupt the counter below the live entry's size: checked_sub(50 - 100) fails.
    manager.total_bytes.store(50, Ordering::Relaxed);

    // The preflight repairs the counter (recount = 100), then the replacement proceeds.
    let outcome = manager.push(&key, &sized_object(&uri, 10)).unwrap();
    assert!(outcome.stored, "the repaired push must proceed");
    assert_eq!(outcome.displaced.len(), 1, "the predecessor is displaced normally");
    assert_eq!(manager.total_bytes(), 10, "exact accounting after the repair");
  }

  /// A store whose capacity displacement triggers an UNRECOVERABLE violation (the recount
  /// overflows) must be abandoned: the just-inserted metadata is removed under the lock,
  /// `stored = false` makes publish roll back the new file, the displaced entry's file is
  /// still cleaned up, the advisory entry count converges, and every later store is refused
  /// pre-mutation. (codex impl-review round-3 item 1)
  #[tokio::test]
  async fn midoperation_invalidation_abandons_the_inserted_store() {
    let dir = temp_cache_dir("ceil-midinval").await;
    let manager = LruCacheManager::new(2, None); // entry-count capacity 2
    let file_store = test_file_store();

    // e1: file-backed, fabricated huge size (content is tiny; size is metadata).
    let (uri_1, key_1) = key_of("http://example.com/e1");
    let path_1 = dir.join("file-e1");
    fs::write(&path_1, b"E1").await.unwrap();
    let obj_1 = CacheObject::new(
      fresh_policy(&uri_1),
      CacheFileOrOnMemory::File(path_1.clone()),
      Bytes::from_static(&[1u8; 32]),
      usize::MAX - 4,
    );
    publish_cache_object(&manager, &file_store, &key_1, obj_1).await;
    manager.total_bytes.store(0, Ordering::Relaxed); // corruption between pushes

    let (uri_2, key_2) = key_of("http://example.com/e2");
    publish_cache_object(&manager, &file_store, &key_2, sized_object(&uri_2, 100)).await;
    manager.total_bytes.store(0, Ordering::Relaxed); // corruption again

    // The new file-backed store: capacity 2 is full, so the insert displaces e1 (LRU-oldest,
    // size MAX-4); the impossible subtraction recounts {e2=100, new=MAX-4} which overflows
    // -> invalidation -> the store must be abandoned and rolled back.
    let (uri_n, key_n) = key_of("http://example.com/new");
    let path_n = dir.join("file-new");
    fs::write(&path_n, b"NEW").await.unwrap();
    let obj_n = CacheObject::new(
      fresh_policy(&uri_n),
      CacheFileOrOnMemory::File(path_n.clone()),
      Bytes::from_static(&[2u8; 32]),
      usize::MAX - 4,
    );
    publish_cache_object(&manager, &file_store, &key_n, obj_n).await;

    assert!(
      manager.get(&key_n).unwrap().is_none(),
      "the abandoned store must not leave its metadata behind"
    );
    assert!(fs::metadata(&path_n).await.is_err(), "the new file is rolled back");
    assert!(fs::metadata(&path_1).await.is_err(), "the displaced file is still cleaned up");
    assert_eq!(file_store.count().await, 0, "both file rollbacks restore the count");
    assert!(manager.get(&key_2).unwrap().is_some(), "the surviving entry keeps serving");
    assert_eq!(manager.count(), 1, "the advisory entry count converges to the LRU length");

    // Every later store is refused before any mutation.
    let (uri_x, key_x) = key_of("http://example.com/x");
    let outcome = manager.push(&key_x, &sized_object(&uri_x, 1)).unwrap();
    assert!(!outcome.stored, "stores after invalidation are refused up front");
    assert!(outcome.displaced.is_empty());
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// A size-eviction (step 3) invalidation early-return also keeps the advisory entry count
  /// in sync with the LRU length. The corrupted state is assembled by direct map insertion
  /// (bypassing push) so the eviction subtraction fails while the recount overflows.
  /// (codex impl-review round-3 item 2)
  #[tokio::test]
  async fn size_eviction_invalidation_early_return_syncs_entry_count() {
    let manager = LruCacheManager::new(10, Some(300));
    let (uri_a, key_a) = key_of("http://example.com/a");
    let (uri_b, key_b) = key_of("http://example.com/b");
    let (uri_c, key_c) = key_of("http://example.com/c");
    {
      // Direct insertion bypasses push: the counter and the advisory count stay stale on
      // purpose (a is LRU-oldest).
      let mut lock = manager.inner.lock().unwrap();
      lock.push(key_a.clone(), sized_object(&uri_a, 100));
      lock.push(key_b.clone(), sized_object(&uri_b, usize::MAX - 4));
      lock.push(key_c.clone(), sized_object(&uri_c, 100));
    }
    manager.total_bytes.store(50, Ordering::Relaxed); // cannot cover a=100
    assert_eq!(manager.count(), 0, "the advisory count is stale before the push");

    // 280 > 300 - 50 forces size eviction; popping a=100 from total=50 fails; the recount
    // over {b=MAX-4, c=100} overflows -> invalidation mid-step-3.
    let (uri_d, key_d) = key_of("http://example.com/d");
    let outcome = manager.push(&key_d, &sized_object(&uri_d, 280)).unwrap();
    assert!(!outcome.stored, "the store is abandoned on mid-eviction invalidation");
    assert_eq!(outcome.displaced.len(), 1, "the popped entry is handed back for cleanup");
    assert!(manager.get(&key_d).unwrap().is_none(), "nothing was inserted");
    assert_eq!(
      manager.count(),
      2,
      "the early return must sync the advisory count to the LRU length"
    );
  }

  /// An OVER-reporting corrupted counter is the one corruption the subtraction path cannot
  /// detect (`checked_sub` keeps succeeding). It surfaces as "the LRU is empty but the fit
  /// condition still holds" in the step-3 loop, which must repair the counter by recounting
  /// (empty live set = exactly 0) instead of silently publishing the stale excess - which
  /// would leave every later store evicting everything, permanently.
  /// (PR #643 Copilot review finding)
  #[tokio::test]
  async fn overreporting_corruption_repairs_when_eviction_empties_the_lru() {
    let manager = LruCacheManager::new(10, Some(200));
    let (uri_a, key_a) = key_of("http://example.com/a");
    assert!(manager.push(&key_a, &sized_object(&uri_a, 10)).unwrap().stored);

    // Corrupt the counter ABOVE the true total: subtractions keep succeeding, so only the
    // empty-LRU repair point can catch this.
    manager.total_bytes.store(500, Ordering::Relaxed);

    // 100 > 200 - 500 (saturated to 0) forces eviction; A is popped (500 - 10 = 490 still
    // "succeeds"), the LRU empties with the condition still true -> recount to 0 -> store.
    let (uri_c, key_c) = key_of("http://example.com/c");
    let outcome = manager.push(&key_c, &sized_object(&uri_c, 100)).unwrap();
    assert!(outcome.stored, "the store proceeds after the repair");
    assert_eq!(outcome.displaced.len(), 1, "A was evicted while the counter looked full");
    assert_eq!(
      manager.total_bytes(),
      100,
      "the counter must be repaired to the exact live total, not stale excess + new size"
    );

    // Self-healed: the next store behaves normally instead of evicting everything again.
    let (uri_d, key_d) = key_of("http://example.com/d");
    let outcome = manager.push(&key_d, &sized_object(&uri_d, 50)).unwrap();
    assert!(outcome.stored);
    assert!(outcome.displaced.is_empty(), "100 + 50 fits under 200; nothing is evicted");
    assert_eq!(manager.total_bytes(), 150);
    assert_eq!(manager.count(), 2);
  }

  /// When even the recount overflows, the accounting is invalidated: every further store is
  /// refused up front (fail-closed), while reads and evictions keep working.
  /// (codex impl-review round-2 item 1)
  #[tokio::test]
  async fn unrecoverable_accounting_invalidates_and_refuses_stores() {
    let manager = LruCacheManager::new(10, None);
    // Assemble a live set whose true sum overflows usize, by corrupting the counter between
    // pushes so each individual preflight passes (sizes are metadata only - no allocation).
    let (uri_1, key_1) = key_of("http://example.com/e1");
    assert!(manager.push(&key_1, &sized_object(&uri_1, usize::MAX - 4)).unwrap().stored);
    manager.total_bytes.store(0, Ordering::Relaxed);
    let (uri_2, key_2) = key_of("http://example.com/e2");
    assert!(manager.push(&key_2, &sized_object(&uri_2, 100)).unwrap().stored);
    manager.total_bytes.store(0, Ordering::Relaxed);
    let (uri_3, key_3) = key_of("http://example.com/e3");
    let obj_3 = sized_object(&uri_3, 50);
    let gen_3 = obj_3.generation;
    assert!(manager.push(&key_3, &obj_3).unwrap().stored);

    // Trigger a violation whose recount ((MAX-4) + 100) itself overflows -> invalidation.
    manager.total_bytes.store(10, Ordering::Relaxed);
    assert!(
      manager.evict_if_generation(&key_3, gen_3).is_some(),
      "the eviction itself still completes"
    );

    // Every further store is refused before any mutation; the live entries are untouched.
    let (uri_4, key_4) = key_of("http://example.com/e4");
    let outcome = manager.push(&key_4, &sized_object(&uri_4, 1)).unwrap();
    assert!(!outcome.stored, "an invalidated accounting must refuse all stores");
    assert!(outcome.displaced.is_empty(), "the refusal must not evict anything");
    assert!(manager.get(&key_4).unwrap().is_none());

    // Reads keep serving and evictions keep cleaning up.
    assert!(manager.get(&key_2).unwrap().is_some(), "reads continue after invalidation");
    let gen_2 = manager.get(&key_2).unwrap().unwrap().generation;
    assert!(
      manager.evict_if_generation(&key_2, gen_2).is_some(),
      "evictions continue after invalidation"
    );
  }

  /// Without a ceiling nothing is ever size-evicted; the counter simply tracks the sum.
  /// (design test 10)
  #[tokio::test]
  async fn unlimited_ceiling_never_evicts_by_size() {
    let manager = LruCacheManager::new(10, None);
    for i in 0..5 {
      let (uri, key) = key_of(&format!("http://example.com/{i}"));
      let outcome = manager.push(&key, &sized_object(&uri, 1000)).unwrap();
      assert!(outcome.stored);
      assert!(outcome.displaced.is_empty(), "no size eviction without a ceiling");
    }
    assert_eq!(manager.count(), 5);
    assert_eq!(manager.total_bytes(), 5000);
  }

  /// Preflight refusal, no-fit arm: an object larger than the ceiling (bypassing the
  /// constructor clamp by direct construction) is refused without evicting anything, and the
  /// file-backed publish path rolls back the already-counted file. (design test 12a)
  #[tokio::test]
  async fn nofit_refusal_rolls_back_file_and_evicts_nothing() {
    let dir = temp_cache_dir("ceil-nofit").await;
    let manager = LruCacheManager::new(10, Some(50));
    let file_store = test_file_store();

    let (uri_o, key_o) = key_of("http://example.com/other");
    publish_cache_object(&manager, &file_store, &key_o, sized_object(&uri_o, 10)).await;
    assert_eq!(manager.total_bytes(), 10);

    let (uri_big, key_big) = key_of("http://example.com/big");
    let path_big = dir.join("file-big");
    fs::write(&path_big, vec![1u8; 60]).await.unwrap();
    let obj_big = CacheObject::new(
      fresh_policy(&uri_big),
      CacheFileOrOnMemory::File(path_big.clone()),
      Bytes::from_static(&[3u8; 32]),
      60, // > ceiling 50: only reachable when the constructor clamp is bypassed
    );
    publish_cache_object(&manager, &file_store, &key_big, obj_big).await;

    assert!(
      fs::metadata(&path_big).await.is_err(),
      "the refused object's file is rolled back"
    );
    assert_eq!(file_store.count().await, 0, "the rollback also restores the file count");
    assert!(manager.get(&key_big).unwrap().is_none(), "no metadata is published");
    assert!(manager.get(&key_o).unwrap().is_some(), "nothing was evicted by the refusal");
    assert_eq!(manager.total_bytes(), 10, "the byte accounting is untouched");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// Preflight refusal, unrepresentable arm (unlimited path): a push whose projected total
  /// overflows `usize` is refused without mutating anything; a same-key replacement whose
  /// freed predecessor makes the total representable proceeds. (design test 12b)
  #[tokio::test]
  async fn unrepresentable_total_is_refused_without_mutation() {
    let manager = LruCacheManager::new(10, None);
    let (uri_huge, key_huge) = key_of("http://example.com/huge");
    assert!(
      manager
        .push(&key_huge, &sized_object(&uri_huge, usize::MAX - 4))
        .unwrap()
        .stored
    );
    assert_eq!(manager.total_bytes(), usize::MAX - 4);

    // (MAX - 4) + 10 is not representable: refuse, evict nothing, insert nothing.
    let (uri_b, key_b) = key_of("http://example.com/b");
    let outcome = manager.push(&key_b, &sized_object(&uri_b, 10)).unwrap();
    assert!(!outcome.stored, "an unrepresentable projected total must be refused");
    assert!(outcome.displaced.is_empty(), "the refusal must not evict anything");
    assert_eq!(manager.total_bytes(), usize::MAX - 4);
    assert!(manager.get(&key_huge).unwrap().is_some());
    assert!(manager.get(&key_b).unwrap().is_none());

    // Positive arm: replacing the huge entry itself subtracts its size first - representable.
    let outcome = manager.push(&key_huge, &sized_object(&uri_huge, 10)).unwrap();
    assert!(outcome.stored, "a same-key replacement freeing the predecessor must proceed");
    assert_eq!(manager.total_bytes(), 10);
  }

  /// Bounded near-`usize::MAX`: the PRE-eviction projected sum would overflow, but the object
  /// fits after the size-eviction loop - it must be stored, not false-rejected.
  /// (design test 12c, codex round-4)
  #[tokio::test]
  async fn bounded_near_max_evicts_and_stores_without_false_rejection() {
    let manager = LruCacheManager::new(10, Some(usize::MAX));
    let (uri_a, key_a) = key_of("http://example.com/a");
    assert!(manager.push(&key_a, &sized_object(&uri_a, usize::MAX - 5)).unwrap().stored);

    // (MAX - 5) + 10 overflows, but evicting A makes it fit: 10 <= MAX.
    let (uri_b, key_b) = key_of("http://example.com/b");
    let outcome = manager.push(&key_b, &sized_object(&uri_b, 10)).unwrap();
    assert!(outcome.stored, "the bounded path must not false-reject on the projected sum");
    assert_eq!(outcome.displaced.len(), 1, "A is size-evicted to make room");
    assert_eq!(manager.total_bytes(), 10);
    assert!(manager.get(&key_a).unwrap().is_none());
    assert!(manager.get(&key_b).unwrap().is_some());
  }

  /// On-memory and file-backed objects count toward the same total. (design test 13)
  #[tokio::test]
  async fn mixed_tiers_share_one_total() {
    let dir = temp_cache_dir("ceil-mixed").await;
    let manager = LruCacheManager::new(10, None);
    let file_store = test_file_store();

    let (uri_f, key_f) = key_of("http://example.com/file");
    let path_f = dir.join("file-f");
    fs::write(&path_f, b"AAAA").await.unwrap();
    let obj_f = CacheObject::new(
      fresh_policy(&uri_f),
      CacheFileOrOnMemory::File(path_f.clone()),
      Bytes::from_static(&[1u8; 32]),
      4,
    );
    publish_cache_object(&manager, &file_store, &key_f, obj_f).await;

    let (uri_m, key_m) = key_of("http://example.com/mem");
    publish_cache_object(&manager, &file_store, &key_m, sized_object(&uri_m, 5)).await;

    assert_eq!(manager.total_bytes(), 9, "both tiers share one byte total");
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// Concurrency smoke: pushes racing from many tasks never leave the retained total above the
  /// ceiling once quiescent (every push holds the one LRU lock for its whole check-evict-insert
  /// sequence). (design test 14)
  #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
  async fn concurrent_publish_storm_respects_ceiling_after_quiescence() {
    const N: usize = 32;
    let manager = LruCacheManager::new(64, Some(500));
    let handles: Vec<_> = (0..N)
      .map(|i| {
        let m = manager.clone();
        tokio::spawn(async move {
          let (uri, key) = key_of(&format!("http://example.com/{i}"));
          m.push(&key, &sized_object(&uri, 100)).unwrap();
        })
      })
      .collect();
    for h in handles {
      h.await.unwrap();
    }
    // Steady state with equal sizes: exactly ceiling/size entries remain.
    assert_eq!(manager.total_bytes(), 500, "the retained total converges to the ceiling");
    assert_eq!(manager.count(), 5);
  }

  /// Removing a file when the count is already zero must saturate, not underflow/panic. This
  /// defends the cross-lock count race: the file count and the LRU map are updated under separate
  /// locks, so a pathological concurrent ordering could otherwise drive the `usize` count below
  /// zero (panic in debug, wraparound in release).
  #[tokio::test]
  async fn file_store_remove_count_saturates_at_zero() {
    let dir = temp_cache_dir("saturate").await;
    let path = dir.join("f");
    fs::write(&path, b"x").await.unwrap();
    let store = FileStore {
      cnt: Arc::new(AtomicUsize::new(0)),
      runtime_handle: tokio::runtime::Handle::current(),
    };
    store.remove(&path).await.unwrap();
    assert_eq!(
      store.count().await,
      0,
      "the file count must saturate at zero, not wrap around"
    );
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// Evicting a counted file entry must restore the count even when the file is already gone (e.g.
  /// removed externally, or by the integrity-check path on a hash mismatch). Otherwise the file
  /// count leaks above the number of live entries once the metadata is popped.
  #[tokio::test]
  async fn evict_missing_file_still_restores_count() {
    let file_store = test_file_store();
    file_store.incr_count().await; // a counted file entry exists
    assert_eq!(file_store.count().await, 1);

    // The file is already gone (never created here); eviction must still correct the count.
    let missing = std::env::temp_dir().join("rpxy-cache-test-never-created-file");
    file_store.evict(&missing).await;
    assert_eq!(
      file_store.count().await,
      0,
      "the file count must be restored even when the file was already gone"
    );
  }

  /// A stale snapshot must not evict a newer live entry of the same key. Models the race where a
  /// `get()` cloned generation A, a concurrent re-store published generation B under the same key,
  /// and the stale `get()` then attempts eviction: B must survive, and only B's own generation can
  /// evict B.
  #[tokio::test]
  async fn evict_if_generation_spares_newer_entry() {
    let manager = LruCacheManager::new(10, None);
    let uri: Uri = "http://example.com/x".parse().unwrap();
    let key = derive_cache_key_from_uri(&uri);

    let obj_a = CacheObject::new(
      fresh_policy(&uri),
      CacheFileOrOnMemory::OnMemory(Bytes::from_static(b"A")),
      Bytes::from_static(&[1u8; 32]),
      1,
    );
    let gen_a = obj_a.generation;
    manager.push(&key, &obj_a).unwrap();

    // A concurrent re-store replaces the entry under the same key with a newer generation.
    let obj_b = CacheObject::new(
      fresh_policy(&uri),
      CacheFileOrOnMemory::OnMemory(Bytes::from_static(b"B")),
      Bytes::from_static(&[2u8; 32]),
      1,
    );
    let gen_b = obj_b.generation;
    manager.push(&key, &obj_b).unwrap();

    // The stale snapshot (generation A) must not evict the live entry B.
    assert!(
      manager.evict_if_generation(&key, gen_a).is_none(),
      "a stale generation must not evict the newer entry"
    );
    let current = manager.get(&key).unwrap().expect("the newer entry must survive");
    assert_eq!(current.generation, gen_b);

    // The current generation can still be evicted.
    assert!(manager.evict_if_generation(&key, gen_b).is_some());
    assert!(
      manager.get(&key).unwrap().is_none(),
      "evicting the current generation removes it"
    );
  }

  /// Body wrapper counting the frames pulled from it, to observe how far ahead of a stalled
  /// consumer the spool producer runs.
  struct CountingBody<B> {
    inner: B,
    pulled: Arc<AtomicUsize>,
  }

  impl<B> hyper::body::Body for CountingBody<B>
  where
    B: hyper::body::Body<Data = Bytes> + Unpin,
  {
    type Data = Bytes;
    type Error = B::Error;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Self::Error>>> {
      let this = self.get_mut();
      let res = Pin::new(&mut this.inner).poll_frame(cx);
      if let Poll::Ready(Some(_)) = &res {
        this.pulled.fetch_add(1, Ordering::Relaxed);
      }
      res
    }
  }

  /// The store/miss path must not race ahead of a stalled consumer: the producer parks once the
  /// channel (capacity + the sender's guaranteed slot) is full instead of pulling the whole body.
  /// Yielding cannot un-park it; only draining the receiver can.
  #[tokio::test]
  async fn store_backpressure_limits_producer_readahead() {
    const TEST_CAPACITY: usize = 2;
    const TOTAL_FRAMES: usize = 32;
    let (tx, rx) = mpsc::channel::<Result<Frame<Bytes>, TestBodyError>>(TEST_CAPACITY);
    let pulled = Arc::new(AtomicUsize::new(0));
    let frames: Vec<Result<Frame<Bytes>, TestBodyError>> = (0..TOTAL_FRAMES)
      .map(|_| Ok(Frame::data(Bytes::from(vec![0u8; 1024]))))
      .collect();
    let body = CountingBody {
      inner: StreamBody::new(stream::iter(frames)),
      pulled: pulled.clone(),
    };
    let uri: Uri = "http://example.com/bp".parse().unwrap();
    let dir = std::env::temp_dir();
    let spool = tokio::spawn(async move {
      // On-memory threshold usize::MAX: no disk involved, the only await point is the bounded send.
      spool_and_store(body, tx, usize::MAX, usize::MAX, &dir, &uri).await
    });

    // Let the producer run until it parks on the full channel (single-threaded test runtime).
    for _ in 0..50 {
      tokio::task::yield_now().await;
    }
    let ahead = pulled.load(Ordering::Relaxed);
    assert!(
      ahead < TOTAL_FRAMES,
      "the producer must be parked by backpressure, not run to EOF (pulled {ahead})"
    );
    // Bound: TEST_CAPACITY buffered + the sender's guaranteed slot + the frame parked in the send.
    assert!(
      ahead <= TEST_CAPACITY + 2,
      "the producer read-ahead must be bounded by the channel capacity (pulled {ahead})"
    );

    // Draining un-parks the producer; the spool completes and the full object is cached.
    assert_eq!(forwarded_len(rx.collect::<Vec<_>>().await), TOTAL_FRAMES * 1024);
    let stored = spool.await.unwrap();
    assert!(
      matches!(stored, Some((CacheFileOrOnMemory::OnMemory(ref b), _, _)) if b.len() == TOTAL_FRAMES * 1024),
      "the drained object must be cached in full"
    );
  }

  /// A slow consumer paces the file-read hit path. Mismatch eviction happens only at EOF, and a
  /// full channel provably keeps the producer from reaching EOF, so with only one frame consumed
  /// the wrong-hashed file must still exist; draining to EOF must then evict it.
  #[tokio::test]
  async fn file_read_backpressure_holds_eof_eviction_until_drained() {
    let path = temp_cache_path("bp-read");
    // Enough chunks that the producer cannot reach EOF while the channel is full.
    let content = vec![5u8; FILE_CACHE_READ_CHUNK * (CACHE_STREAM_CHANNEL_CAPACITY + 4)];
    fs::write(&path, &content).await.unwrap();
    let wrong_hash = Bytes::from_static(&[0u8; 32]);
    let file_store = FileStore {
      cnt: Arc::new(AtomicUsize::new(1)),
      runtime_handle: tokio::runtime::Handle::current(),
    };
    let mut body = file_store.read(path.clone(), &wrong_hash).await.unwrap();

    // Consume a single frame, then stall.
    assert!(body.frame().await.is_some(), "the first frame must arrive");
    for _ in 0..50 {
      tokio::task::yield_now().await;
    }
    assert!(
      fs::metadata(&path).await.is_ok(),
      "the producer must be parked before EOF, so the mismatch eviction has not run yet"
    );

    // Drain to EOF: the integrity check finally runs and evicts the corrupted file.
    while body.frame().await.is_some() {}
    assert!(
      fs::metadata(&path).await.is_err(),
      "draining to EOF must evict the wrong-hashed file"
    );
  }

  /// Dropping the receiver mid-stream after the store has spilled to a temp file must abort the
  /// store, clean up the temp file, and cache nothing (complements
  /// `returns_none_when_downstream_dropped`, which covers the receiver being gone from the start).
  #[tokio::test]
  async fn store_dropped_receiver_after_spill_cleans_temp() {
    let dir = temp_cache_dir("drop-spill").await;
    let uri: Uri = "http://example.com/drop".parse().unwrap();
    let (tx, mut rx) = mpsc::channel::<Result<Frame<Bytes>, TestBodyError>>(0);
    // 6 x 2000 bytes with a 4096 on-memory threshold: the spill starts at the third frame.
    let frames: Vec<Result<Frame<Bytes>, TestBodyError>> =
      (0..6).map(|_| Ok(Frame::data(Bytes::from(vec![2u8; 2000])))).collect();
    let body = StreamBody::new(stream::iter(frames));
    let dir_clone = dir.clone();
    let spool = tokio::spawn(async move { spool_and_store(body, tx, 1_000_000, 4096, &dir_clone, &uri).await });

    // Consume enough frames for the spill to have happened (the third frame was processed once
    // the fourth has been forwarded), then hang up.
    for _ in 0..4 {
      assert!(rx.next().await.is_some());
    }
    drop(rx);

    assert!(spool.await.unwrap().is_none(), "an aborted store must not cache");
    let mut entries = fs::read_dir(&dir).await.unwrap();
    assert!(
      entries.next_entry().await.unwrap().is_none(),
      "the temp file must be cleaned up when the receiver goes away mid-spill"
    );
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// Concurrent count updates converge without locks. Phase-separated for a deterministic
  /// expectation (free interleaving with the saturating decrement would make the final count
  /// scheduling-dependent): phase 1 increments concurrently, phase 2 evicts each task's own
  /// pre-created file concurrently.
  #[tokio::test]
  async fn file_store_count_concurrent_storm_phased() {
    const N: usize = 64;
    let dir = temp_cache_dir("storm").await;
    let store = test_file_store();

    // Phase 1: N concurrent increments (publish-side bookkeeping).
    let handles: Vec<_> = (0..N)
      .map(|_| {
        let s = store.clone();
        tokio::spawn(async move { s.incr_count().await })
      })
      .collect();
    for h in handles {
      h.await.unwrap();
    }
    assert_eq!(store.count().await, N, "all concurrent increments must be counted");

    // Phase 2: N concurrent evictions, each of its own counted file.
    let mut handles = Vec::with_capacity(N);
    for i in 0..N {
      let p = dir.join(format!("f{i}"));
      fs::write(&p, b"x").await.unwrap();
      let s = store.clone();
      handles.push(tokio::spawn(async move { s.evict(&p).await }));
    }
    for h in handles {
      h.await.unwrap();
    }
    assert_eq!(store.count().await, 0, "all concurrent evictions must be counted");
    let mut entries = fs::read_dir(&dir).await.unwrap();
    assert!(
      entries.next_entry().await.unwrap().is_none(),
      "every evicted file must be unlinked"
    );
    let _ = fs::remove_dir_all(&dir).await;
  }

  /// Dropping the receiver mid-read must not evict the file. The file has more chunks than the
  /// channel can hold, so the producer cannot have reached EOF when the drop lands; the stored
  /// hash is intentionally wrong, so the file surviving proves the aborted read exits without
  /// acting on the (incomplete) integrity check - a run to EOF would have evicted it.
  #[tokio::test]
  async fn file_read_dropped_receiver_does_not_evict() {
    let path = temp_cache_path("drop-read");
    let content = vec![6u8; FILE_CACHE_READ_CHUNK * (CACHE_STREAM_CHANNEL_CAPACITY + 4)];
    fs::write(&path, &content).await.unwrap();
    let wrong_hash = Bytes::from_static(&[0u8; 32]);
    let file_store = FileStore {
      cnt: Arc::new(AtomicUsize::new(1)),
      runtime_handle: tokio::runtime::Handle::current(),
    };
    let mut body = file_store.read(path.clone(), &wrong_hash).await.unwrap();
    assert!(body.frame().await.is_some(), "the first frame must arrive");
    drop(body);

    // Let the producer observe the disconnect and exit.
    for _ in 0..50 {
      tokio::task::yield_now().await;
    }
    assert!(
      fs::metadata(&path).await.is_ok(),
      "an aborted read must leave the file in place (no integrity verdict without EOF)"
    );
    let _ = fs::remove_file(&path).await;
  }
}
