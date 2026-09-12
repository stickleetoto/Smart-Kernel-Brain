pub mod live;
pub mod watcher;

pub use live::{LiveIndexError, ReloadReport, SharedGenerationEngine};
pub use watcher::{LiveWatcher, WatcherConfig, WatcherError, WatcherStatus};

use serde::{Deserialize, Serialize};
use skb::state::UsageState;
use skb::{FileIndex, ResolvedFile, SearchEngine};
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

pub const INITIAL_GENERATION: u64 = 1;

/// Stable v2 reference to one file inside one specific index generation.
///
/// A bare numeric file_id is intentionally insufficient once live reload exists:
/// the same numeric id may point at a different file after an index rebuild.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileRef {
    pub generation: u64,
    pub file_id: u32,
}

/// Lowest-overhead v2 locator result. It preserves the v1 hot-cache signal while
/// attaching the generation required for safe deferred path resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanFileRefV2 {
    pub reference: FileRef,
    pub hot_cache_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    StaleReference {
        active_generation: u64,
        reference_generation: u64,
    },
    MissingFile {
        file_id: u32,
    },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleReference {
                active_generation,
                reference_generation,
            } => write!(
                f,
                "STALE_REFERENCE: active generation is {active_generation}, reference generation is {reference_generation}"
            ),
            Self::MissingFile { file_id } => {
                write!(f, "missing file_id {file_id} in active generation")
            }
        }
    }
}

impl Error for ResolveError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReloadError {
    RootChanged {
        active_root: String,
        candidate_root: String,
    },
    GenerationChangedDuringBuild {
        expected_generation: u64,
        active_generation: u64,
    },
    GenerationExhausted,
}

impl fmt::Display for ReloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootChanged {
                active_root,
                candidate_root,
            } => write!(
                f,
                "candidate index root changed from {active_root:?} to {candidate_root:?}"
            ),
            Self::GenerationChangedDuringBuild {
                expected_generation,
                active_generation,
            } => write!(
                f,
                "candidate was built from generation {expected_generation}, but active generation is now {active_generation}"
            ),
            Self::GenerationExhausted => write!(f, "index generation counter exhausted"),
        }
    }
}

impl Error for ReloadError {}

/// v2 correctness wrapper around the frozen v1 locator core.
///
/// The v1 SearchEngine remains untouched. This wrapper owns the active generation
/// and converts v1 numeric ids into generation-aware references. Replacing the
/// index constructs the candidate engine first and only then swaps it into place.
pub struct GenerationEngine {
    generation: u64,
    engine: SearchEngine,
    state_path: PathBuf,
}

impl GenerationEngine {
    pub fn new(index: FileIndex, state: UsageState, state_path: PathBuf) -> Self {
        Self {
            generation: INITIAL_GENERATION,
            engine: SearchEngine::new(index, state, state_path.clone()),
            state_path,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn active_index(&self) -> &FileIndex {
        &self.engine.index
    }

    pub fn find_first_ref(&self, filename: &str) -> Option<LeanFileRefV2> {
        self.engine
            .find_first_ref(filename)
            .map(|hit| LeanFileRefV2 {
                reference: FileRef {
                    generation: self.generation,
                    file_id: hit.file_id,
                },
                hot_cache_hit: hit.hot_cache_hit,
            })
    }

    pub fn resolve(&self, reference: FileRef) -> Result<ResolvedFile, ResolveError> {
        if reference.generation != self.generation {
            return Err(ResolveError::StaleReference {
                active_generation: self.generation,
                reference_generation: reference.generation,
            });
        }

        self.engine
            .resolve_file(reference.file_id)
            .ok_or(ResolveError::MissingFile {
                file_id: reference.file_id,
            })
    }

    /// Resolve in input order. Each stale/missing reference is reported explicitly
    /// instead of being silently dropped.
    pub fn resolve_many(&self, references: &[FileRef]) -> Vec<Result<ResolvedFile, ResolveError>> {
        references
            .iter()
            .copied()
            .map(|reference| self.resolve(reference))
            .collect()
    }

    /// Validate and activate a fully-built candidate index.
    ///
    /// The candidate must represent the same root. Generation advances only after
    /// all validation and candidate SearchEngine construction have succeeded.
    pub fn replace_index(&mut self, candidate: FileIndex) -> Result<u64, ReloadError> {
        if candidate.root != self.engine.index.root {
            return Err(ReloadError::RootChanged {
                active_root: self.engine.index.root.clone(),
                candidate_root: candidate.root,
            });
        }

        let next_generation = self
            .generation
            .checked_add(1)
            .ok_or(ReloadError::GenerationExhausted)?;

        let replacement = SearchEngine::new(
            candidate,
            self.engine.state.clone(),
            self.state_path.clone(),
        );

        self.engine = replacement;
        self.generation = next_generation;
        Ok(self.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with(count: usize, suffix: &str) -> GenerationEngine {
        let index = FileIndex::synthetic(count);
        let state_path = std::env::temp_dir().join(format!("skb-v2-generation-{suffix}.json"));
        GenerationEngine::new(index, UsageState::default(), state_path)
    }

    #[test]
    fn same_generation_reference_resolves() {
        let engine = engine_with(32, "same-generation");
        let hit = engine.find_first_ref("FILE_00000007.DAT").unwrap();

        assert_eq!(hit.reference.generation, INITIAL_GENERATION);
        assert_eq!(hit.reference.file_id, 7);

        let resolved = engine.resolve(hit.reference).unwrap();
        assert_eq!(resolved.file_id, 7);
        assert_eq!(resolved.name, "file_00000007.dat");
    }

    #[test]
    fn old_reference_becomes_stale_after_successful_reload() {
        let mut engine = engine_with(32, "stale-after-reload");
        let old = engine
            .find_first_ref("FILE_00000007.DAT")
            .unwrap()
            .reference;

        let next = engine.replace_index(FileIndex::synthetic(64)).unwrap();
        assert_eq!(next, INITIAL_GENERATION + 1);

        let error = engine.resolve(old).unwrap_err();
        assert_eq!(
            error,
            ResolveError::StaleReference {
                active_generation: INITIAL_GENERATION + 1,
                reference_generation: INITIAL_GENERATION,
            }
        );
    }

    #[test]
    fn stale_reference_never_resolves_reused_numeric_id() {
        let mut engine = engine_with(10, "reused-id");
        let old = engine
            .find_first_ref("FILE_00000003.DAT")
            .unwrap()
            .reference;
        assert_eq!(old.file_id, 3);

        engine.replace_index(FileIndex::synthetic(10)).unwrap();

        assert!(matches!(
            engine.resolve(old),
            Err(ResolveError::StaleReference { .. })
        ));

        let fresh = engine
            .find_first_ref("FILE_00000003.DAT")
            .unwrap()
            .reference;
        assert_ne!(fresh.generation, old.generation);
        assert_eq!(fresh.file_id, old.file_id);
        assert!(engine.resolve(fresh).is_ok());
    }

    #[test]
    fn rejected_candidate_leaves_generation_and_index_untouched() {
        let mut engine = engine_with(10, "reject-candidate");
        let old_generation = engine.generation();
        let old_root = engine.active_index().root.clone();
        let old = engine
            .find_first_ref("FILE_00000004.DAT")
            .unwrap()
            .reference;

        let err = engine
            .replace_index(FileIndex::empty("synthetic://different-root"))
            .unwrap_err();
        assert!(matches!(err, ReloadError::RootChanged { .. }));
        assert_eq!(engine.generation(), old_generation);
        assert_eq!(engine.active_index().root, old_root);
        assert!(engine.resolve(old).is_ok());
    }

    #[test]
    fn batch_resolution_preserves_errors_in_input_order() {
        let mut engine = engine_with(10, "batch-order");
        let first = engine
            .find_first_ref("FILE_00000001.DAT")
            .unwrap()
            .reference;
        engine.replace_index(FileIndex::synthetic(10)).unwrap();
        let second = engine
            .find_first_ref("FILE_00000002.DAT")
            .unwrap()
            .reference;

        let results = engine.resolve_many(&[first, second]);
        assert!(matches!(
            &results[0],
            Err(ResolveError::StaleReference { .. })
        ));
        assert_eq!(results[1].as_ref().unwrap().file_id, 2);
    }
}
