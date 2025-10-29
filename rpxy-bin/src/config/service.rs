use super::toml::ConfigToml;
use crate::log::{debug, error, warn};
use async_trait::async_trait;
use hot_reload::{RealtimeWatch, RealtimeWatchHandle, Reload, ReloaderError, WatchEvent};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
  path::{Path, PathBuf},
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
  },
  time::Duration,
};
use tokio::{
  runtime::Handle,
  sync::{Mutex, mpsc},
};

#[derive(Clone)]
pub struct ConfigTomlReloader {
  pub config_path: PathBuf,
}

#[async_trait]
impl Reload<ConfigToml, String> for ConfigTomlReloader {
  type Source = String;
  async fn new(source: &Self::Source) -> Result<Self, ReloaderError<ConfigToml, String>> {
    Ok(Self {
      config_path: PathBuf::from(source),
    })
  }

  async fn reload(&self) -> Result<Option<ConfigToml>, ReloaderError<ConfigToml, String>> {
    let conf = ConfigToml::new(&self.config_path).map_err(|e| ReloaderError::<ConfigToml, String>::Reload(e.to_string()))?;
    Ok(Some(conf))
  }
}

/* ---------------------------------------------------------- */
// Extended trait implementation for realtime reloading of configuration file

/// Default channel size for mpsc channels
const DEFAULT_CHANNEL_SIZE: usize = 100;
/// Duration to debounce rapid successive file events.
const FILE_EVENT_DEBOUNCE: Duration = Duration::from_millis(200);

#[derive(Debug)]
/// Simple enum to represent debounced file events.
enum DebouncedEvent {
  Reload,
  Removed,
  Error(String),
}

/// Queue and debounce file events to avoid rapid successive reloads.
async fn queue_debounced_event(
  event: DebouncedEvent,
  debounce_counter: Arc<AtomicU64>,
  latest_event: Arc<Mutex<Option<(u64, DebouncedEvent)>>>,
  tx: mpsc::Sender<WatchEvent<ConfigToml>>,
  config_path: PathBuf,
) {
  let event_id = debounce_counter.fetch_add(1, Ordering::AcqRel) + 1;

  {
    let mut slot = latest_event.lock().await;
    *slot = Some((event_id, event));
  }

  tokio::time::sleep(FILE_EVENT_DEBOUNCE).await;

  if debounce_counter.load(Ordering::Acquire) != event_id {
    return;
  }

  let should_process = {
    let slot = latest_event.lock().await;
    matches!(slot.as_ref(), Some((stored_id, _)) if *stored_id == event_id)
  };

  if !should_process {
    return;
  }

  let event_to_process = {
    let mut slot = latest_event.lock().await;
    slot.take().map(|(_, event)| event)
  };

  if let Some(event) = event_to_process {
    handle_debounced_event(event, &tx, &config_path).await;
  }
}

/// Handle a debounced file event by reading and parsing the config file, then sending appropriate events.
async fn handle_debounced_event(event: DebouncedEvent, tx: &mpsc::Sender<WatchEvent<ConfigToml>>, config_path: &PathBuf) {
  match event {
    DebouncedEvent::Reload => match tokio::fs::read_to_string(config_path).await {
      Ok(content) => match toml::from_str::<ConfigToml>(&content) {
        Ok(config_toml) => {
          if let Err(e) = tx.send(WatchEvent::Changed(config_toml)).await {
            error!("Failed to send changed event: {}", e);
          }
        }
        Err(e) => {
          warn!("Failed to parse config file: {}", e);
          let message = e.to_string();
          if let Err(send_err) = tx.send(WatchEvent::Error(message)).await {
            error!("Failed to send error event: {}", send_err);
          }
        }
      },
      Err(e) => {
        error!("Failed to read config file: {}", e);
        let message = e.to_string();
        if let Err(send_err) = tx.send(WatchEvent::Error(message)).await {
          error!("Failed to send error event: {}", send_err);
        }
      }
    },
    DebouncedEvent::Removed => {
      warn!("Config file was removed");
      if let Err(e) = tx.send(WatchEvent::Removed).await {
        error!("Failed to send removed event: {}", e);
      }
    }
    DebouncedEvent::Error(message) => {
      if let Err(e) = tx.send(WatchEvent::Error(message)).await {
        error!("Failed to send error event: {}", e);
      }
    }
  }
}

#[async_trait]
impl RealtimeWatch<ConfigToml, String> for ConfigTomlReloader {
  /// Establish a file watcher on the configuration file path.
  async fn watch_realtime(&self) -> Result<RealtimeWatchHandle<ConfigToml>, ReloaderError<ConfigToml, String>> {
    let (tx, rx) = mpsc::channel(DEFAULT_CHANNEL_SIZE);
    let config_path = self.config_path.clone();
    let debounce_counter = Arc::new(AtomicU64::new(0));
    let latest_event = Arc::new(Mutex::new(None::<(u64, DebouncedEvent)>));

    let watcher = {
      let tx = tx.clone();
      let config_path_for_callback = config_path.clone();
      // Get Tokio runtime handle to spawn tasks from the notify callback thread
      let handle = Handle::current();
      let debounce_counter = debounce_counter.clone();
      let latest_event = latest_event.clone();

      let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        let tx = tx.clone();
        let config_path = config_path_for_callback.clone();
        let handle = handle.clone();
        let debounce_counter = debounce_counter.clone();
        let latest_event = latest_event.clone();

        // Spawn async task on Tokio runtime from the notify callback thread
        handle.spawn(async move {
          let event = match res {
            Ok(event) => {
              debug!("File event: {:?}", event);
              match event.kind {
                EventKind::Modify(_) | EventKind::Create(_) => Some(DebouncedEvent::Reload),
                EventKind::Remove(_) => Some(DebouncedEvent::Removed),
                _ => {
                  debug!("Ignoring event kind: {:?}", event.kind);
                  None
                }
              }
            }
            Err(e) => {
              error!("Watch error: {}", e);
              Some(DebouncedEvent::Error(e.to_string()))
            }
          };

          if let Some(event) = event {
            queue_debounced_event(event, debounce_counter, latest_event, tx, config_path).await;
          }
        });
      })
      .map_err(|e| ReloaderError::Other(e.into()))?;

      watcher
        .watch(Path::new(&config_path), RecursiveMode::NonRecursive)
        .map_err(|e| ReloaderError::Other(e.into()))?;

      watcher
    };

    debug!("File watching established for: {:?}", config_path);

    Ok(RealtimeWatchHandle::with_cleanup(rx, Box::new(watcher)))
  }
}
