use crate::{LiveIndexError, ReloadError, SharedGenerationEngine};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(250);
const IDLE_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatcherConfig {
    pub debounce: Duration,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce: DEFAULT_DEBOUNCE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatcherStatus {
    pub running: bool,
    pub events_seen: u64,
    pub batches_seen: u64,
    pub rebuilds_succeeded: u64,
    pub rebuilds_failed: u64,
    pub last_generation: u64,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub enum WatcherError {
    LiveIndex(LiveIndexError),
    Notify(notify::Error),
    StartupChannelClosed,
    StatusPoisoned,
    WorkerPanicked,
}

impl fmt::Display for WatcherError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LiveIndex(error) => error.fmt(f),
            Self::Notify(error) => error.fmt(f),
            Self::StartupChannelClosed => write!(f, "watcher startup channel closed unexpectedly"),
            Self::StatusPoisoned => write!(f, "watcher status lock is poisoned"),
            Self::WorkerPanicked => write!(f, "watcher worker thread panicked"),
        }
    }
}

impl Error for WatcherError {}

impl From<LiveIndexError> for WatcherError {
    fn from(value: LiveIndexError) -> Self {
        Self::LiveIndex(value)
    }
}

impl From<notify::Error> for WatcherError {
    fn from(value: notify::Error) -> Self {
        Self::Notify(value)
    }
}

/// Background native filesystem watcher for one SharedGenerationEngine.
///
/// Events are coalesced with a trailing-edge debounce. One event burst causes one
/// full candidate rebuild, while reads remain available during the disk scan. The
/// watcher owns its worker thread and stops it on Drop.
pub struct LiveWatcher {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<WatcherStatus>>,
    worker: Option<JoinHandle<()>>,
}

impl LiveWatcher {
    pub fn start(
        engine: SharedGenerationEngine,
        config: WatcherConfig,
    ) -> Result<Self, WatcherError> {
        let (root, ignored_state_path) = engine.watch_snapshot()?;
        let generation = engine.generation()?;
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(WatcherStatus {
            running: false,
            events_seen: 0,
            batches_seen: 0,
            rebuilds_succeeded: 0,
            rebuilds_failed: 0,
            last_generation: generation,
            last_error: None,
        }));
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);

        let worker_stop = Arc::clone(&stop);
        let worker_status = Arc::clone(&status);
        let worker = thread::Builder::new()
            .name("skb-v2-watcher".to_string())
            .spawn(move || {
                run_worker(
                    engine,
                    root,
                    ignored_state_path,
                    config,
                    worker_stop,
                    worker_status,
                    ready_tx,
                );
            })
            .map_err(|error| WatcherError::Notify(notify::Error::generic(&error.to_string())))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                stop,
                status,
                worker: Some(worker),
            }),
            Ok(Err(error)) => {
                stop.store(true, Ordering::Release);
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                stop.store(true, Ordering::Release);
                let _ = worker.join();
                Err(WatcherError::StartupChannelClosed)
            }
        }
    }

    pub fn status(&self) -> Result<WatcherStatus, WatcherError> {
        self.status
            .lock()
            .map(|status| status.clone())
            .map_err(|_| WatcherError::StatusPoisoned)
    }

    pub fn stop(mut self) -> Result<(), WatcherError> {
        self.stop_inner()
    }

    fn stop_inner(&mut self) -> Result<(), WatcherError> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| WatcherError::WorkerPanicked)?;
        }
        Ok(())
    }
}

impl Drop for LiveWatcher {
    fn drop(&mut self) {
        let _ = self.stop_inner();
    }
}

fn run_worker(
    engine: SharedGenerationEngine,
    root: PathBuf,
    ignored_state_path: PathBuf,
    config: WatcherConfig,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<WatcherStatus>>,
    ready_tx: mpsc::SyncSender<Result<(), WatcherError>>,
) {
    let (event_tx, event_rx) = mpsc::channel();
    let mut watcher = match build_watcher(event_tx) {
        Ok(watcher) => watcher,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };

    if let Err(error) = watcher.watch(&root, RecursiveMode::Recursive) {
        let _ = ready_tx.send(Err(error.into()));
        return;
    }

    update_status(&status, |current| {
        current.running = true;
    });
    if ready_tx.send(Ok(())).is_err() {
        return;
    }

    while !stop.load(Ordering::Acquire) {
        let first = match event_rx.recv_timeout(IDLE_POLL) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                record_error(&status, "filesystem event channel disconnected".to_string());
                break;
            }
        };

        if !handle_event_result(first, &ignored_state_path, &status) {
            continue;
        }

        let mut deadline = Instant::now() + config.debounce;
        loop {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            match event_rx.recv_timeout(deadline.saturating_duration_since(now)) {
                Ok(event) => {
                    if handle_event_result(event, &ignored_state_path, &status) {
                        deadline = Instant::now() + config.debounce;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        if stop.load(Ordering::Acquire) {
            break;
        }

        update_status(&status, |current| {
            current.batches_seen = current.batches_seen.saturating_add(1);
        });

        match engine.rebuild_from_disk() {
            Ok(report) => update_status(&status, |current| {
                current.rebuilds_succeeded = current.rebuilds_succeeded.saturating_add(1);
                current.last_generation = report.generation;
                current.last_error = None;
            }),
            Err(LiveIndexError::Reload(ReloadError::GenerationChangedDuringBuild { .. })) => {
                if let Ok(generation) = engine.generation() {
                    update_status(&status, |current| {
                        current.last_generation = generation;
                    });
                }
            }
            Err(error) => update_status(&status, |current| {
                current.rebuilds_failed = current.rebuilds_failed.saturating_add(1);
                current.last_error = Some(error.to_string());
            }),
        }
    }

    update_status(&status, |current| {
        current.running = false;
    });
}

fn build_watcher(
    event_tx: mpsc::Sender<notify::Result<Event>>,
) -> Result<RecommendedWatcher, WatcherError> {
    Ok(notify::recommended_watcher(move |event| {
        let _ = event_tx.send(event);
    })?)
}

fn handle_event_result(
    result: notify::Result<Event>,
    ignored_state_path: &Path,
    status: &Arc<Mutex<WatcherStatus>>,
) -> bool {
    match result {
        Ok(event) => {
            update_status(status, |current| {
                current.events_seen = current.events_seen.saturating_add(1);
            });
            event_requires_rebuild(&event, ignored_state_path)
        }
        Err(error) => {
            record_error(status, error.to_string());
            false
        }
    }
}

fn event_requires_rebuild(event: &Event, ignored_state_path: &Path) -> bool {
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }

    if !event.paths.is_empty()
        && event
            .paths
            .iter()
            .all(|path| path.as_path() == ignored_state_path)
    {
        return false;
    }

    true
}

fn update_status(status: &Arc<Mutex<WatcherStatus>>, update: impl FnOnce(&mut WatcherStatus)) {
    if let Ok(mut current) = status.lock() {
        update(&mut current);
    }
}

fn record_error(status: &Arc<Mutex<WatcherStatus>>, error: String) {
    update_status(status, |current| {
        current.last_error = Some(error);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use skb::state::UsageState;
    use skb::FileIndex;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "skb-v2-watcher-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn shared_from_root(root: &Path) -> SharedGenerationEngine {
        let (index, _) = FileIndex::scan(root).unwrap();
        SharedGenerationEngine::from_parts(
            index,
            UsageState::default(),
            root.join(".skb-v2-test-state.json"),
        )
    }

    fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            thread::sleep(Duration::from_millis(25));
        }
        condition()
    }

    #[test]
    fn access_events_do_not_trigger_rebuild() {
        let event = Event::new(EventKind::Access(notify::event::AccessKind::Any));
        assert!(!event_requires_rebuild(&event, Path::new("ignored")));
    }

    #[test]
    fn state_file_events_are_ignored() {
        let ignored = PathBuf::from("root/.skb-state.json");
        let event = Event::new(EventKind::Modify(notify::event::ModifyKind::Any))
            .add_path(ignored.clone());
        assert!(!event_requires_rebuild(&event, &ignored));
    }

    #[test]
    fn native_watcher_tracks_create_rename_and_delete() {
        let root = temp_root("native-events");
        fs::write(root.join("alpha.txt"), b"alpha").unwrap();
        let shared = shared_from_root(&root);
        let watcher = LiveWatcher::start(
            shared.clone(),
            WatcherConfig {
                debounce: Duration::from_millis(75),
            },
        )
        .unwrap();

        let beta = root.join("beta.txt");
        fs::write(&beta, b"beta").unwrap();
        assert!(wait_until(Duration::from_secs(10), || {
            shared.find_first_ref("beta.txt").unwrap().is_some()
        }));

        let gamma = root.join("gamma.txt");
        fs::rename(&beta, &gamma).unwrap();
        assert!(wait_until(Duration::from_secs(10), || {
            shared.find_first_ref("gamma.txt").unwrap().is_some()
                && shared.find_first_ref("beta.txt").unwrap().is_none()
        }));

        fs::remove_file(&gamma).unwrap();
        assert!(wait_until(Duration::from_secs(10), || {
            shared.find_first_ref("gamma.txt").unwrap().is_none()
        }));

        let status = watcher.status().unwrap();
        assert!(status.events_seen >= 3);
        assert!(status.rebuilds_succeeded >= 3);
        watcher.stop().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
