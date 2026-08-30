use crate::hash::{filename_eq, filename_hash};
use crate::index::FileIndex;
use crate::state::{now_ms, UsageRecord, UsageState};
use serde::Serialize;
use std::cmp::Ordering;
use std::io;
use std::path::PathBuf;
use std::time::Instant;

const DEFAULT_HOT_CAPACITY: usize = 1024;
const HOT_WAYS: usize = 4;
const HALF_LIFE_MS: f64 = 7.0 * 24.0 * 60.0 * 60.0 * 1000.0;

/// v0.4 lazy search result. The expensive full path String is intentionally absent.
/// Consumers can keep/use the compact file_id and resolve a path only when needed.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct FileRefHit {
    pub file_id: u32,
    pub score: f64,
    pub access_count: u64,
    pub hot_cache_hit: bool,
}

/// v0.4.1 lean locator result. No path, score, usage clone, or heap allocation is
/// required to return one of these from the fast path.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct LeanFileRef {
    pub file_id: u32,
    pub hot_cache_hit: bool,
}

/// Fully materialized compatibility result used by the human CLI and legacy MCP tool.
#[derive(Debug, Clone, Serialize)]
pub struct SearchHit {
    pub file_id: u32,
    pub name: String,
    pub path: String,
    pub score: f64,
    pub access_count: u64,
    pub hot_cache_hit: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedFile {
    pub file_id: u32,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HotName {
    pub name: String,
    pub score: f64,
    pub access_count: u64,
    pub last_access_ms: u64,
    pub matching_paths: usize,
}

#[derive(Debug, Clone)]
struct HotEntry {
    hash: u64,
    ids: Vec<u32>,
    usage: UsageRecord,
}

/// Tiny fixed-layout L0. No HashMap, no second hash function and no String key.
///
/// Each filename hash maps to one set and up to four entries are checked. The
/// real filename is always verified through the compact index, so 64-bit hash
/// collisions cannot create false matches.
#[derive(Debug)]
struct HotCache {
    slots: Vec<Option<HotEntry>>,
    set_mask: usize,
    names: usize,
}

impl HotCache {
    fn new(capacity: usize) -> Self {
        let requested_sets = capacity.max(HOT_WAYS).div_ceil(HOT_WAYS);
        let sets = requested_sets.next_power_of_two();
        Self {
            slots: vec![None; sets * HOT_WAYS],
            set_mask: sets - 1,
            names: 0,
        }
    }

    #[inline]
    fn set_base(&self, hash: u64) -> usize {
        let folded = hash ^ (hash >> 32);
        ((folded as usize) & self.set_mask) * HOT_WAYS
    }

    fn insert_ranked(&mut self, entry: HotEntry) -> bool {
        let base = self.set_base(entry.hash);
        for slot in &mut self.slots[base..base + HOT_WAYS] {
            if slot.is_none() {
                *slot = Some(entry);
                self.names += 1;
                return true;
            }
        }
        false
    }

    #[inline]
    fn find<'a>(&'a self, index: &FileIndex, filename: &str, hash: u64) -> Option<&'a HotEntry> {
        let base = self.set_base(hash);
        for slot in &self.slots[base..base + HOT_WAYS] {
            let Some(entry) = slot.as_ref() else { continue };
            if entry.hash != hash {
                continue;
            }
            let Some(&first_id) = entry.ids.first() else { continue };
            if filename_eq(index.entry_name(first_id), filename) {
                return Some(entry);
            }
        }
        None
    }

    fn entry_count(&self) -> usize {
        self.slots
            .iter()
            .filter_map(Option::as_ref)
            .map(|entry| entry.ids.len())
            .sum()
    }

    fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

pub struct SearchEngine {
    pub index: FileIndex,
    pub state: UsageState,
    state_path: PathBuf,
    hot_capacity: usize,
    hot: HotCache,
}

impl SearchEngine {
    pub fn new(index: FileIndex, state: UsageState, state_path: PathBuf) -> Self {
        let mut engine = Self {
            index,
            state,
            state_path,
            hot_capacity: DEFAULT_HOT_CAPACITY,
            hot: HotCache::new(DEFAULT_HOT_CAPACITY),
        };
        engine.rebuild_hot_cache();
        engine
    }

    pub fn set_hot_capacity(&mut self, capacity: usize) {
        self.hot_capacity = capacity.max(1);
        self.rebuild_hot_cache();
    }

    /// Update adaptive state without touching disk. Intended for controlled benchmarks.
    pub fn learn_in_memory(&mut self, filename: &str) {
        self.state.record_filename(filename);
        self.rebuild_hot_cache();
    }

    /// L0-only, pre-hashed, allocation-free filename -> first file-id lookup.
    #[inline]
    pub fn l0_lookup_first_id_with_hash(&self, filename: &str, hash: u64) -> Option<u32> {
        self.hot
            .find(&self.index, filename, hash)
            .and_then(|entry| entry.ids.first().copied())
    }

    /// L0-only lookup including SKB's filename hash.
    #[inline]
    pub fn l0_lookup_first_id(&self, filename: &str) -> Option<u32> {
        self.l0_lookup_first_id_with_hash(filename, filename_hash(filename))
    }

    /// L1-only, pre-hashed, allocation-free filename -> first file-id lookup.
    #[inline]
    pub fn l1_lookup_first_id_with_hash(&self, filename: &str, hash: u64) -> Option<u32> {
        self.index.lookup_first_id_with_hash(filename, hash)
    }

    /// Fast filename -> first file-id route. L0 is tried first, then compact L1.
    #[inline]
    pub fn lookup_first_id(&self, filename: &str) -> Option<u32> {
        let hash = filename_hash(filename);
        self.l0_lookup_first_id_with_hash(filename, hash)
            .or_else(|| self.l1_lookup_first_id_with_hash(filename, hash))
    }


    /// Lowest-overhead practical locator result: filename -> compact file ref.
    /// No heap allocation, path construction, score calculation, or state lookup.
    #[inline]
    pub fn find_first_ref(&self, filename: &str) -> Option<LeanFileRef> {
        let hash = filename_hash(filename);
        self.find_first_ref_with_hash(filename, hash)
    }

    /// Pre-hashed form of `find_first_ref` for callers that already computed the hash.
    #[inline]
    pub fn find_first_ref_with_hash(&self, filename: &str, hash: u64) -> Option<LeanFileRef> {
        if let Some(file_id) = self.l0_lookup_first_id_with_hash(filename, hash) {
            return Some(LeanFileRef {
                file_id,
                hot_cache_hit: true,
            });
        }
        self.l1_lookup_first_id_with_hash(filename, hash)
            .map(|file_id| LeanFileRef {
                file_id,
                hot_cache_hit: false,
            })
    }

    /// Fill a caller-owned result buffer. If `out` already has enough capacity,
    /// repeated lookups allocate nothing. This is the multi-hit companion to
    /// `find_first_ref`. It deliberately omits adaptive score metadata.
    pub fn find_refs_reuse(
        &self,
        filename: &str,
        limit: usize,
        out: &mut Vec<LeanFileRef>,
    ) -> usize {
        out.clear();
        let limit = limit.max(1);
        if out.capacity() < limit {
            out.reserve(limit);
        }

        let hash = filename_hash(filename);
        if let Some(entry) = self.hot.find(&self.index, filename, hash) {
            for &file_id in entry.ids.iter().take(limit) {
                out.push(LeanFileRef {
                    file_id,
                    hot_cache_hit: true,
                });
            }
            return out.len();
        }

        self.index.visit_exact_candidates_with_hash(filename, hash, limit, |file_id| {
            out.push(LeanFileRef {
                file_id,
                hot_cache_hit: false,
            });
        });
        out.len()
    }

    /// Untimed v0.4 lazy path API used by low-overhead callers and benchmarks.
    /// It returns compact references and never materializes a full path String.
    pub fn find_refs(
        &mut self,
        filename: &str,
        limit: usize,
        learn: bool,
    ) -> io::Result<Vec<FileRefHit>> {
        self.find_lazy_inner(filename, limit, learn)
    }

    /// Timed lazy path API for CLI/MCP reporting. Returns file IDs and adaptive
    /// metadata without building any path String.
    pub fn find_lazy(
        &mut self,
        filename: &str,
        limit: usize,
        learn: bool,
    ) -> io::Result<(Vec<FileRefHit>, u128)> {
        let start = Instant::now();
        let refs = self.find_lazy_inner(filename, limit, learn)?;
        Ok((refs, start.elapsed().as_nanos()))
    }

    fn find_lazy_inner(
        &mut self,
        filename: &str,
        limit: usize,
        learn: bool,
    ) -> io::Result<Vec<FileRefHit>> {
        let hash = filename_hash(filename);

        let hot_match = self.hot.find(&self.index, filename, hash).map(|entry| {
            (entry.ids.clone(), entry.usage.clone())
        });

        let (mut ids, hot_hit, mut usage) = if let Some((ids, usage)) = hot_match {
            (ids, true, usage)
        } else {
            let ids = self.index.exact_candidates_with_hash(filename, hash);
            let usage = if self.state.records.is_empty() {
                UsageRecord::default()
            } else {
                self.state.get_for_filename(filename)
            };
            (ids, false, usage)
        };

        // No path sorting here: the lookup table already provides deterministic
        // file-id order. Lazy path means no String path is constructed just to rank.
        ids.truncate(limit.max(1));

        if learn && !ids.is_empty() {
            usage = self.state.record_filename(filename).clone();
            self.rebuild_hot_cache();
            self.state.save(&self.state_path)?;
        }

        let score = weight(usage.access_count, usage.last_access_ms);
        Ok(ids
            .into_iter()
            .map(|file_id| FileRefHit {
                file_id,
                score,
                access_count: usage.access_count,
                hot_cache_hit: hot_hit,
            })
            .collect())
    }

    /// Resolve one compact file ID to a human-readable path only when requested.
    pub fn resolve_file(&self, file_id: u32) -> Option<ResolvedFile> {
        if file_id as usize >= self.index.entry_count() {
            return None;
        }
        Some(ResolvedFile {
            file_id,
            name: self.index.entry_name(file_id).to_owned(),
            path: self.index.entry_path(file_id),
        })
    }

    /// Resolve a batch of lazy references. Invalid IDs are ignored.
    pub fn resolve_files(&self, file_ids: &[u32]) -> Vec<ResolvedFile> {
        file_ids
            .iter()
            .copied()
            .filter_map(|file_id| self.resolve_file(file_id))
            .collect()
    }

    /// Compatibility full find. v0.4 performs the lazy lookup first and only then
    /// materializes name/path Strings for the small returned result set.
    pub fn find(&mut self, filename: &str, limit: usize, learn: bool) -> io::Result<(Vec<SearchHit>, u128)> {
        let start = Instant::now();
        let refs = self.find_lazy_inner(filename, limit, learn)?;
        let hits = refs
            .into_iter()
            .map(|hit| SearchHit {
                file_id: hit.file_id,
                name: self.index.entry_name(hit.file_id).to_owned(),
                path: self.index.entry_path(hit.file_id),
                score: hit.score,
                access_count: hit.access_count,
                hot_cache_hit: hit.hot_cache_hit,
            })
            .collect();
        Ok((hits, start.elapsed().as_micros()))
    }

    pub fn hot_names(&self, limit: usize) -> Vec<HotName> {
        let mut ranked: Vec<(&String, &crate::state::UsageRecord)> = self
            .state
            .records
            .iter()
            .filter(|(_, usage)| usage.access_count > 0)
            .collect();
        ranked.sort_by(|a, b| {
            let sa = weight(a.1.access_count, a.1.last_access_ms);
            let sb = weight(b.1.access_count, b.1.last_access_ms);
            sb.partial_cmp(&sa).unwrap_or(Ordering::Equal)
        });

        ranked
            .into_iter()
            .filter_map(|(normalized, usage)| {
                let ids = self.index.exact_candidates(normalized);
                let first = *ids.first()?;
                Some(HotName {
                    name: self.index.entry_name(first).to_owned(),
                    score: weight(usage.access_count, usage.last_access_ms),
                    access_count: usage.access_count,
                    last_access_ms: usage.last_access_ms,
                    matching_paths: ids.len(),
                })
            })
            .take(limit)
            .collect()
    }

    pub fn hot_entry_count(&self) -> usize {
        self.hot.entry_count()
    }

    pub fn hot_name_count(&self) -> usize {
        self.hot.names
    }

    pub fn hot_slot_count(&self) -> usize {
        self.hot.slot_count()
    }

    fn rebuild_hot_cache(&mut self) {
        let mut ranked: Vec<(String, f64)> = self
            .state
            .records
            .iter()
            .filter_map(|(name, usage)| {
                (usage.access_count > 0).then(|| {
                    (name.clone(), weight(usage.access_count, usage.last_access_ms))
                })
            })
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        ranked.truncate(self.hot_capacity);

        let mut hot = HotCache::new(self.hot_capacity);
        for (normalized, _) in ranked {
            let hash = filename_hash(&normalized);
            let ids = self.index.exact_candidates_with_hash(&normalized, hash);
            if ids.is_empty() {
                continue;
            }
            let usage = self.state.get_normalized(&normalized);
            let _ = hot.insert_ranked(HotEntry { hash, ids, usage });
        }
        self.hot = hot;
    }
}

/// Adaptive weight: logarithmic frequency + exponentially decaying recency.
/// Old popularity fades instead of pinning a filename forever.
pub fn weight(access_count: u64, last_access_ms: u64) -> f64 {
    if access_count == 0 || last_access_ms == 0 {
        return 0.0;
    }
    let frequency = (access_count as f64 + 1.0).ln();
    let age = now_ms().saturating_sub(last_access_ms) as f64;
    let recency = 2.0_f64.powf(-age / HALF_LIFE_MS);
    0.70 * frequency + 0.30 * recency
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::UsageState;

    #[test]
    fn in_memory_learning_promotes_filename_to_hot_cache() {
        let index = FileIndex::synthetic(100);
        let state_path = std::env::temp_dir().join("skb-v04-test-state-unused.json");
        let mut engine = SearchEngine::new(index, UsageState::default(), state_path);
        assert_eq!(engine.hot_name_count(), 0);

        engine.learn_in_memory("file_00000042.dat");
        assert_eq!(engine.hot_name_count(), 1);
        assert_eq!(engine.l0_lookup_first_id("FILE_00000042.DAT"), Some(42));

        let (hits, _) = engine.find("FILE_00000042.DAT", 1, false).unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].hot_cache_hit);
    }

    #[test]
    fn l1_fast_path_returns_first_id_without_materializing_path() {
        let index = FileIndex::synthetic(1_000);
        let state_path = std::env::temp_dir().join("skb-v04-test-state-unused-2.json");
        let engine = SearchEngine::new(index, UsageState::default(), state_path);
        let name = "FILE_00000777.DAT";
        let hash = filename_hash(name);
        assert_eq!(engine.l1_lookup_first_id_with_hash(name, hash), Some(777));
    }

    #[test]
    fn lazy_find_returns_file_id_without_path_materialization() {
        let index = FileIndex::synthetic(10_000);
        let state_path = std::env::temp_dir().join("skb-v04-test-state-unused-3.json");
        let mut engine = SearchEngine::new(index, UsageState::default(), state_path);
        let (refs, _) = engine.find_lazy("FILE_00004321.DAT", 1, false).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].file_id, 4_321);
        let resolved = engine.resolve_file(refs[0].file_id).unwrap();
        assert_eq!(resolved.name, "file_00004321.dat");
        assert!(resolved.path.ends_with("file_00004321.dat"));
    }

    #[test]
    fn lean_first_ref_returns_compact_id_without_allocating_result_vec() {
        let index = FileIndex::synthetic(1_000);
        let state_path = std::env::temp_dir().join("skb-v041-test-state-unused-lean.json");
        let engine = SearchEngine::new(index, UsageState::default(), state_path);
        assert_eq!(
            engine.find_first_ref("FILE_00000777.DAT"),
            Some(LeanFileRef { file_id: 777, hot_cache_hit: false })
        );
    }

    #[test]
    fn reusable_ref_buffer_keeps_capacity_between_queries() {
        let index = FileIndex::synthetic(1_000);
        let state_path = std::env::temp_dir().join("skb-v041-test-state-unused-reuse.json");
        let engine = SearchEngine::new(index, UsageState::default(), state_path);
        let mut out = Vec::with_capacity(4);
        let before = out.capacity();
        assert_eq!(engine.find_refs_reuse("FILE_00000042.DAT", 1, &mut out), 1);
        assert_eq!(out[0].file_id, 42);
        assert_eq!(out.capacity(), before);
        assert_eq!(engine.find_refs_reuse("FILE_00000043.DAT", 1, &mut out), 1);
        assert_eq!(out[0].file_id, 43);
        assert_eq!(out.capacity(), before);
    }

    #[test]
    fn invalid_file_id_does_not_panic_when_resolving() {
        let index = FileIndex::synthetic(10);
        let state_path = std::env::temp_dir().join("skb-v04-test-state-unused-4.json");
        let engine = SearchEngine::new(index, UsageState::default(), state_path);
        assert!(engine.resolve_file(999).is_none());
    }
}
