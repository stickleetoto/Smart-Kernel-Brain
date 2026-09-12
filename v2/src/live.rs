use crate::{GenerationEngine, LeanFileRefV2, ReloadError, ResolveError};
use serde::{Deserialize, Serialize};
use skb::state::UsageState;
use skb::{FileIndex, ResolvedFile};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadReport {
    pub previous_generation: u64,
    pub generation: u64,
    pub files_indexed: usize,
    pub directories_seen: usize,
    pub skipped_entries: usize,
    pub scan_elapsed_micros: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveIndexError {
    LockPoisoned,
    RootUnavailable { root: String, message: String },
    Reload(ReloadError),
    Resolve(ResolveError),
}

impl fmt::Display for LiveIndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LockPoisoned => write!(f, "live index lock is poisoned"),
            Self::RootUnavailable { root, message } => {
                write!(f, "cannot rebuild index root {root:?}: {message}")
            }
            Self::Reload(error) => error.fmt(f),
            Self::Resolve(error) => error.fmt(f),
        }
    }
}

impl Error for LiveIndexError {}

impl From<ReloadError> for LiveIndexError {
    fn from(value: ReloadError) -> Self {
        Self::Reload(value)
    }
}

impl From<ResolveError> for LiveIndexError {
    fn from(value: ResolveError) -> Self {
        Self::Resolve(value)
    }
}

/// Thread-safe v2 live-index facade.
///
/// Readers take a shared lock only for the short lookup/resolve operation. A disk
/// rebuild is performed completely outside the write lock. The write lock is held
/// only while the validated candidate SearchEngine is swapped into place and the
/// generation is advanced.
#[derive(Clone)]
pub struct SharedGenerationEngine {
    inner: Arc<RwLock<GenerationEngine>>,
}

impl SharedGenerationEngine {
    pub fn new(engine: GenerationEngine) -> Self {
        Self {
            inner: Arc::new(RwLock::new(engine)),
        }
    }

    pub fn from_parts(index: FileIndex, state: UsageState, state_path: PathBuf) -> Self {
        Self::new(GenerationEngine::new(index, state, state_path))
    }

    pub fn generation(&self) -> Result<u64, LiveIndexError> {
        let guard = self.inner.read().map_err(|_| LiveIndexError::LockPoisoned)?;
        Ok(guard.generation())
    }

    pub fn active_root(&self) -> Result<String, LiveIndexError> {
        let guard = self.inner.read().map_err(|_| LiveIndexError::LockPoisoned)?;
        Ok(guard.active_index().root.clone())
    }

    pub fn find_first_ref(&self, filename: &str) -> Result<Option<LeanFileRefV2>, LiveIndexError> {
        let guard = self.inner.read().map_err(|_| LiveIndexError::LockPoisoned)?;
        Ok(guard.find_first_ref(filename))
    }

    pub fn resolve(&self, reference: crate::FileRef) -> Result<ResolvedFile, LiveIndexError> {
        let guard = self.inner.read().map_err(|_| LiveIndexError::LockPoisoned)?;
        Ok(guard.resolve(reference)?)
    }

    /// Atomically replace a fully built candidate. Readers can observe the old or
    /// the new generation, never a partially replaced SearchEngine.
    pub fn replace_index_atomic(&self, candidate: FileIndex) -> Result<u64, LiveIndexError> {
        let mut guard = self.inner.write().map_err(|_| LiveIndexError::LockPoisoned)?;
        Ok(guard.replace_index(candidate)?)
    }

    /// Rebuild the active root from disk without blocking readers during the scan.
    ///
    /// The active generation is snapshotted before scanning. If another rebuild
    /// wins the race before this candidate is ready, this older candidate is
    /// rejected instead of overwriting the newer index.
    pub fn rebuild_from_disk(&self) -> Result<ReloadReport, LiveIndexError> {
        let (root, expected_generation) = {
            let guard = self.inner.read().map_err(|_| LiveIndexError::LockPoisoned)?;
            (guard.active_index().root.clone(), guard.generation())
        };

        validate_root(&root)?;
        let (candidate, scan) = FileIndex::scan(Path::new(&root)).map_err(|error| {
            LiveIndexError::RootUnavailable {
                root: root.clone(),
                message: error.to_string(),
            }
        })?;

        let mut guard = self.inner.write().map_err(|_| LiveIndexError::LockPoisoned)?;
        let active_generation = guard.generation();
        if active_generation != expected_generation {
            return Err(ReloadError::GenerationChangedDuringBuild {
                expected_generation,
                active_generation,
            }
            .into());
        }

        let generation = guard.replace_index(candidate)?;
        Ok(ReloadReport {
            previous_generation: expected_generation,
            generation,
            files_indexed: scan.files_indexed,
            directories_seen: scan.directories_seen,
            skipped_entries: scan.skipped_entries,
            scan_elapsed_micros: scan.elapsed.as_micros().min(u64::MAX as u128) as u64,
        })
    }
}

fn validate_root(root: &str) -> Result<(), LiveIndexError> {
    let metadata = fs::metadata(root).map_err(|error| LiveIndexError::RootUnavailable {
        root: root.to_owned(),
        message: error.to_string(),
    })?;
    if !metadata.is_dir() {
        return Err(LiveIndexError::RootUnavailable {
            root: root.to_owned(),
            message: "path is not a directory".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "skb-v2-live-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn shared_from_root(root: &Path) -> SharedGenerationEngine {
        let (index, _) = FileIndex::scan(root).unwrap();
        let state_path = root.join(".skb-v2-test-state.json");
        SharedGenerationEngine::from_parts(index, UsageState::default(), state_path)
    }

    #[test]
    fn disk_rebuild_sees_new_files_and_stales_old_references() {
        let root = temp_root("disk-rebuild");
        fs::write(root.join("alpha.txt"), b"alpha").unwrap();
        let shared = shared_from_root(&root);

        let old_alpha = shared
            .find_first_ref("alpha.txt")
            .unwrap()
            .unwrap()
            .reference;
        fs::write(root.join("beta.txt"), b"beta").unwrap();

        let report = shared.rebuild_from_disk().unwrap();
        assert_eq!(report.previous_generation, INITIAL_GENERATION);
        assert_eq!(report.generation, INITIAL_GENERATION + 1);
        assert_eq!(report.files_indexed, 2);

        assert!(matches!(
            shared.resolve(old_alpha),
            Err(LiveIndexError::Resolve(ResolveError::StaleReference { .. }))
        ));
        let beta = shared.find_first_ref("beta.txt").unwrap().unwrap();
        let resolved = shared.resolve(beta.reference).unwrap();
        assert_eq!(resolved.name, "beta.txt");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_root_does_not_advance_generation() {
        let root = temp_root("missing-root");
        fs::write(root.join("alpha.txt"), b"alpha").unwrap();
        let shared = shared_from_root(&root);
        let generation = shared.generation().unwrap();

        fs::remove_dir_all(&root).unwrap();
        assert!(matches!(
            shared.rebuild_from_disk(),
            Err(LiveIndexError::RootUnavailable { .. })
        ));
        assert_eq!(shared.generation().unwrap(), generation);
    }

    #[test]
    fn concurrent_readers_observe_only_complete_generations() {
        let state_path = std::env::temp_dir().join("skb-v2-live-concurrent-state.json");
        let shared = SharedGenerationEngine::from_parts(
            FileIndex::synthetic(128),
            UsageState::default(),
            state_path,
        );

        let reader = shared.clone();
        let handle = thread::spawn(move || {
            for _ in 0..20_000 {
                let hit = reader
                    .find_first_ref("FILE_00000042.DAT")
                    .unwrap()
                    .unwrap();
                match reader.resolve(hit.reference) {
                    Ok(file) => assert_eq!(file.name, "file_00000042.dat"),
                    Err(LiveIndexError::Resolve(ResolveError::StaleReference { .. })) => {
                        // A generation may change between the lookup and resolve calls.
                    }
                    Err(error) => panic!("unexpected live-index error: {error}"),
                }
            }
        });

        for count in [256, 512, 1_024, 2_048] {
            shared
                .replace_index_atomic(FileIndex::synthetic(count))
                .unwrap();
        }

        handle.join().unwrap();
        assert_eq!(shared.generation().unwrap(), INITIAL_GENERATION + 4);
    }
}
