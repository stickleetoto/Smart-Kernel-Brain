use crate::{FileRef, GenerationEngine};
use serde::{Deserialize, Serialize};
use skb::FileIndex;
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

const DEFAULT_LIMIT: usize = 32;
const MAX_LIMIT: usize = 4096;
const FUZZY_MIN_SCORE: u16 = 700;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Exact,
    Prefix,
    Fuzzy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMatchKind {
    Exact,
    Prefix,
    Substring,
    Fuzzy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MetadataFilter {
    pub min_size_bytes: Option<u64>,
    pub max_size_bytes: Option<u64>,
    pub modified_after_unix_secs: Option<u64>,
    pub modified_before_unix_secs: Option<u64>,
}

impl MetadataFilter {
    pub fn is_active(&self) -> bool {
        self.min_size_bytes.is_some()
            || self.max_size_bytes.is_some()
            || self.modified_after_unix_secs.is_some()
            || self.modified_before_unix_secs.is_some()
    }

    fn matches(&self, metadata: &FileMetadataV2) -> bool {
        if let Some(minimum) = self.min_size_bytes {
            if metadata.size_bytes < minimum {
                return false;
            }
        }
        if let Some(maximum) = self.max_size_bytes {
            if metadata.size_bytes > maximum {
                return false;
            }
        }
        if let Some(after) = self.modified_after_unix_secs {
            if !metadata
                .modified_unix_secs
                .map(|modified| modified > after)
                .unwrap_or(false)
            {
                return false;
            }
        }
        if let Some(before) = self.modified_before_unix_secs {
            if !metadata
                .modified_unix_secs
                .map(|modified| modified < before)
                .unwrap_or(false)
            {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileMetadataV2 {
    pub size_bytes: u64,
    pub modified_unix_secs: Option<u64>,
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub term: String,
    pub mode: SearchMode,
    #[serde(default)]
    pub extensions: Vec<String>,
    pub path_contains: Option<String>,
    pub scope: Option<String>,
    #[serde(default)]
    pub metadata: MetadataFilter,
    #[serde(default)]
    pub include_metadata: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl SearchQuery {
    pub fn exact(term: impl Into<String>) -> Self {
        Self::new(term, SearchMode::Exact)
    }

    pub fn prefix(term: impl Into<String>) -> Self {
        Self::new(term, SearchMode::Prefix)
    }

    pub fn fuzzy(term: impl Into<String>) -> Self {
        Self::new(term, SearchMode::Fuzzy)
    }

    pub fn effective_limit(&self) -> usize {
        self.limit.clamp(1, MAX_LIMIT)
    }

    fn new(term: impl Into<String>, mode: SearchMode) -> Self {
        Self {
            term: term.into(),
            mode,
            extensions: Vec::new(),
            path_contains: None,
            scope: None,
            metadata: MetadataFilter::default(),
            include_metadata: false,
            limit: DEFAULT_LIMIT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHitV2 {
    pub reference: FileRef,
    pub name: String,
    pub path: String,
    pub score: u16,
    pub match_kind: SearchMatchKind,
    pub metadata: Option<FileMetadataV2>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub generation: u64,
    pub hits: Vec<SearchHitV2>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RankedCandidate {
    score: u16,
    file_id: u32,
    match_kind: SearchMatchKind,
}

impl Ord for RankedCandidate {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score
            .cmp(&other.score)
            .then_with(|| other.file_id.cmp(&self.file_id))
    }
}

impl PartialOrd for RankedCandidate {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl GenerationEngine {
    pub fn search(&self, query: &SearchQuery) -> SearchResponse {
        search_index(self.active_index(), self.generation(), query)
    }
}

fn default_limit() -> usize {
    DEFAULT_LIMIT
}

fn search_index(index: &FileIndex, generation: u64, query: &SearchQuery) -> SearchResponse {
    let limit = query.effective_limit();
    if query.term.trim().is_empty() {
        return SearchResponse {
            generation,
            hits: Vec::new(),
            truncated: false,
        };
    }

    let extension_filters = normalize_extensions(&query.extensions);
    let path_filter = query.path_contains.as_deref().map(normalize_path_text);
    let scope_filter = query.scope.as_deref().map(normalize_scope_text);
    let term_folded = query.term.to_lowercase();

    match query.mode {
        SearchMode::Exact => search_exact(
            index,
            generation,
            query,
            limit,
            &extension_filters,
            path_filter.as_deref(),
            scope_filter.as_deref(),
        ),
        SearchMode::Prefix => search_prefix(
            index,
            generation,
            &term_folded,
            limit,
            &extension_filters,
            path_filter.as_deref(),
            scope_filter.as_deref(),
            &query.metadata,
            query.include_metadata,
        ),
        SearchMode::Fuzzy => search_fuzzy(
            index,
            generation,
            &query.term,
            limit,
            &extension_filters,
            path_filter.as_deref(),
            scope_filter.as_deref(),
            &query.metadata,
            query.include_metadata,
        ),
    }
}

fn search_exact(
    index: &FileIndex,
    generation: u64,
    query: &SearchQuery,
    limit: usize,
    extension_filters: &[String],
    path_filter: Option<&str>,
    scope_filter: Option<&str>,
) -> SearchResponse {
    let mut hits = Vec::with_capacity(limit.min(64));
    let mut truncated = false;

    for file_id in index.exact_candidates(&query.term) {
        if !candidate_matches(
            index,
            file_id,
            extension_filters,
            path_filter,
            scope_filter,
            &query.metadata,
        ) {
            continue;
        }
        if hits.len() == limit {
            truncated = true;
            break;
        }
        hits.push(materialize_hit(
            index,
            generation,
            file_id,
            1000,
            SearchMatchKind::Exact,
            query.include_metadata,
        ));
    }

    SearchResponse {
        generation,
        hits,
        truncated,
    }
}

#[allow(clippy::too_many_arguments)]
fn search_prefix(
    index: &FileIndex,
    generation: u64,
    term_folded: &str,
    limit: usize,
    extension_filters: &[String],
    path_filter: Option<&str>,
    scope_filter: Option<&str>,
    metadata_filter: &MetadataFilter,
    include_metadata: bool,
) -> SearchResponse {
    let mut ranked = BinaryHeap::with_capacity(limit.saturating_add(1));
    let mut matching = 0usize;

    for raw_id in 0..index.entry_count() {
        let file_id = raw_id as u32;
        let name_folded = index.entry_name(file_id).to_lowercase();
        if !name_folded.starts_with(term_folded) {
            continue;
        }
        if !candidate_matches(
            index,
            file_id,
            extension_filters,
            path_filter,
            scope_filter,
            metadata_filter,
        ) {
            continue;
        }

        matching = matching.saturating_add(1);
        let score = prefix_score(term_folded, &name_folded);
        push_ranked(
            &mut ranked,
            limit,
            RankedCandidate {
                score,
                file_id,
                match_kind: SearchMatchKind::Prefix,
            },
        );
    }

    finalize_ranked(
        index,
        generation,
        ranked,
        matching > limit,
        include_metadata,
    )
}

#[allow(clippy::too_many_arguments)]
fn search_fuzzy(
    index: &FileIndex,
    generation: u64,
    term: &str,
    limit: usize,
    extension_filters: &[String],
    path_filter: Option<&str>,
    scope_filter: Option<&str>,
    metadata_filter: &MetadataFilter,
    include_metadata: bool,
) -> SearchResponse {
    let query_normalized = normalize_fuzzy_text(term);
    if query_normalized.is_empty() {
        return SearchResponse {
            generation,
            hits: Vec::new(),
            truncated: false,
        };
    }

    let mut ranked = BinaryHeap::with_capacity(limit.saturating_add(1));
    let mut matching = 0usize;

    for raw_id in 0..index.entry_count() {
        let file_id = raw_id as u32;
        let Some((score, match_kind)) = fuzzy_score(&query_normalized, index.entry_name(file_id))
        else {
            continue;
        };
        if score < FUZZY_MIN_SCORE {
            continue;
        }
        if !candidate_matches(
            index,
            file_id,
            extension_filters,
            path_filter,
            scope_filter,
            metadata_filter,
        ) {
            continue;
        }

        matching = matching.saturating_add(1);
        push_ranked(
            &mut ranked,
            limit,
            RankedCandidate {
                score,
                file_id,
                match_kind,
            },
        );
    }

    finalize_ranked(
        index,
        generation,
        ranked,
        matching > limit,
        include_metadata,
    )
}

fn push_ranked(
    heap: &mut BinaryHeap<Reverse<RankedCandidate>>,
    limit: usize,
    candidate: RankedCandidate,
) {
    heap.push(Reverse(candidate));
    if heap.len() > limit {
        let _ = heap.pop();
    }
}

fn finalize_ranked(
    index: &FileIndex,
    generation: u64,
    heap: BinaryHeap<Reverse<RankedCandidate>>,
    truncated: bool,
    include_metadata: bool,
) -> SearchResponse {
    let mut candidates: Vec<RankedCandidate> = heap.into_iter().map(|item| item.0).collect();
    candidates.sort_unstable_by(|left, right| right.cmp(left));
    let hits = candidates
        .into_iter()
        .map(|candidate| {
            materialize_hit(
                index,
                generation,
                candidate.file_id,
                candidate.score,
                candidate.match_kind,
                include_metadata,
            )
        })
        .collect();

    SearchResponse {
        generation,
        hits,
        truncated,
    }
}

fn candidate_matches(
    index: &FileIndex,
    file_id: u32,
    extensions: &[String],
    path_contains: Option<&str>,
    scope: Option<&str>,
    metadata_filter: &MetadataFilter,
) -> bool {
    let name = index.entry_name(file_id);
    if !extensions.is_empty() && !matches_extension(name, extensions) {
        return false;
    }

    if path_contains.is_none() && scope.is_none() && !metadata_filter.is_active() {
        return true;
    }

    let path = index.entry_path(file_id);
    let normalized_path = normalize_path_text(&path);

    if let Some(needle) = path_contains {
        if !normalized_path.contains(needle) {
            return false;
        }
    }

    if let Some(scope) = scope {
        let relative = relative_normalized_path(&index.root, &path);
        if !path_is_in_scope(&relative, scope) {
            return false;
        }
    }

    if metadata_filter.is_active() {
        let Some(metadata) = read_file_metadata(&path) else {
            return false;
        };
        if !metadata_filter.matches(&metadata) {
            return false;
        }
    }

    true
}

fn materialize_hit(
    index: &FileIndex,
    generation: u64,
    file_id: u32,
    score: u16,
    match_kind: SearchMatchKind,
    include_metadata: bool,
) -> SearchHitV2 {
    let path = index.entry_path(file_id);
    let metadata = if include_metadata {
        read_file_metadata(&path)
    } else {
        None
    };

    SearchHitV2 {
        reference: FileRef {
            generation,
            file_id,
        },
        name: index.entry_name(file_id).to_owned(),
        path,
        score,
        match_kind,
        metadata,
    }
}

fn read_file_metadata(path: &str) -> Option<FileMetadataV2> {
    let metadata = fs::metadata(path).ok()?;
    let modified_unix_secs = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());

    Some(FileMetadataV2 {
        size_bytes: metadata.len(),
        modified_unix_secs,
        readonly: metadata.permissions().readonly(),
    })
}

fn prefix_score(term: &str, name: &str) -> u16 {
    let extra = name.chars().count().saturating_sub(term.chars().count());
    950u16.saturating_sub(extra.min(50) as u16)
}

fn fuzzy_score(query_normalized: &str, name: &str) -> Option<(u16, SearchMatchKind)> {
    let name_folded = name.to_lowercase();
    let query_folded = query_normalized.to_lowercase();

    if name_folded == query_folded {
        return Some((1000, SearchMatchKind::Exact));
    }
    if name_folded.starts_with(&query_folded) {
        return Some((970, SearchMatchKind::Prefix));
    }
    if name_folded.contains(&query_folded) {
        return Some((930, SearchMatchKind::Substring));
    }

    let name_normalized = normalize_fuzzy_text(&name_folded);
    if name_normalized == query_normalized {
        return Some((995, SearchMatchKind::Exact));
    }
    if name_normalized.starts_with(query_normalized) {
        return Some((965, SearchMatchKind::Prefix));
    }
    if name_normalized.contains(query_normalized) {
        return Some((925, SearchMatchKind::Substring));
    }

    let query_tokens: Vec<&str> = query_normalized.split_whitespace().collect();
    let name_tokens: Vec<&str> = name_normalized.split_whitespace().collect();
    if query_tokens.is_empty() || name_tokens.is_empty() {
        return None;
    }

    let mut token_total = 0u32;
    for query_token in &query_tokens {
        let best = name_tokens
            .iter()
            .map(|name_token| token_similarity(query_token, name_token))
            .max()
            .unwrap_or(0);
        let required = if query_token.chars().count() <= 2 {
            90
        } else {
            55
        };
        if best < required {
            return None;
        }
        token_total = token_total.saturating_add(best as u32);
    }

    let token_average = (token_total / query_tokens.len() as u32) as u16;
    let token_score = 520u16.saturating_add(token_average.saturating_mul(4));

    let query_compact: String = query_normalized.chars().filter(|ch| *ch != ' ').collect();
    let name_compact: String = name_normalized.chars().filter(|ch| *ch != ' ').collect();
    let whole_similarity = similarity_percent(&query_compact, &name_compact) as u16;
    let whole_score = 480u16.saturating_add(whole_similarity.saturating_mul(4));

    Some((
        token_score.max(whole_score).min(920),
        SearchMatchKind::Fuzzy,
    ))
}

fn token_similarity(query: &str, candidate: &str) -> u8 {
    if query == candidate {
        return 100;
    }
    if candidate.starts_with(query) {
        return 94;
    }
    if query.starts_with(candidate) && candidate.chars().count() >= 3 {
        return 88;
    }
    if query.chars().count() >= 3 && candidate.contains(query) {
        return 86;
    }
    similarity_percent(query, candidate)
}

fn similarity_percent(left: &str, right: &str) -> u8 {
    let left_chars: Vec<char> = left.chars().collect();
    let right_chars: Vec<char> = right.chars().collect();
    let longest = left_chars.len().max(right_chars.len());
    if longest == 0 {
        return 100;
    }

    let distance = levenshtein_chars(&left_chars, &right_chars);
    (((longest.saturating_sub(distance)) * 100) / longest) as u8
}

fn levenshtein_chars(left: &[char], right: &[char]) -> usize {
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }

    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0usize; right.len() + 1];

    for (left_index, left_char) in left.iter().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_char) in right.iter().enumerate() {
            let substitution = previous[right_index] + usize::from(left_char != right_char);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            current[right_index + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right.len()]
}

fn normalize_fuzzy_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_separator = true;

    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            normalized.push(character);
            previous_separator = false;
        } else if !previous_separator {
            normalized.push(' ');
            previous_separator = true;
        }
    }

    if normalized.ends_with(' ') {
        normalized.pop();
    }
    normalized
}

fn normalize_extensions(extensions: &[String]) -> Vec<String> {
    extensions
        .iter()
        .map(|extension| extension.trim().trim_start_matches('.').to_lowercase())
        .filter(|extension| !extension.is_empty())
        .collect()
}

fn matches_extension(name: &str, extensions: &[String]) -> bool {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            extensions
                .iter()
                .any(|expected| extension.eq_ignore_ascii_case(expected))
        })
        .unwrap_or(false)
}

fn normalize_path_text(value: &str) -> String {
    value.replace('\\', "/").to_lowercase()
}

fn normalize_scope_text(value: &str) -> String {
    normalize_path_text(value).trim_matches('/').to_owned()
}

fn relative_normalized_path(root: &str, path: &str) -> String {
    let root_path = Path::new(root);
    let file_path = Path::new(path);
    file_path
        .strip_prefix(root_path)
        .map(|relative| normalize_path_text(&relative.to_string_lossy()))
        .unwrap_or_else(|_| normalize_path_text(path))
        .trim_matches('/')
        .to_owned()
}

fn path_is_in_scope(relative_path: &str, scope: &str) -> bool {
    if scope.is_empty() {
        return true;
    }
    relative_path == scope
        || relative_path.starts_with(&format!("{scope}/"))
        || relative_path.contains(&format!("/{scope}/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use skb::state::UsageState;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("skb-v2-search-{label}-{}-{id}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn engine_from_root(root: &Path) -> GenerationEngine {
        let (index, _) = FileIndex::scan(root).unwrap();
        GenerationEngine::new(
            index,
            UsageState::default(),
            root.join(".skb-v2-search-state.json"),
        )
    }

    #[test]
    fn exact_search_uses_generation_safe_refs() {
        let root = temp_root("exact");
        fs::write(root.join("README.md"), b"readme").unwrap();
        let engine = engine_from_root(&root);

        let response = engine.search(&SearchQuery::exact("readme.md"));
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].reference.generation, engine.generation());
        assert_eq!(response.hits[0].name, "README.md");
        assert_eq!(response.hits[0].score, 1000);
        assert_eq!(response.hits[0].match_kind, SearchMatchKind::Exact);
        assert!(response.hits[0].metadata.is_none());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prefix_search_combines_extension_and_path_filters() {
        let root = temp_root("prefix-filter");
        fs::create_dir_all(root.join("src/core")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("src/core/restore.rs"), b"").unwrap();
        fs::write(root.join("src/core/restore.md"), b"").unwrap();
        fs::write(root.join("docs/restore.rs"), b"").unwrap();
        let engine = engine_from_root(&root);

        let mut query = SearchQuery::prefix("rest");
        query.extensions = vec!["rs".to_string()];
        query.path_contains = Some("src".to_string());
        let response = engine.search(&query);

        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].name, "restore.rs");
        assert!(normalize_path_text(&response.hits[0].path).contains("src/core"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scope_limits_results_to_relative_subtree() {
        let root = temp_root("scope");
        fs::create_dir_all(root.join("projects/bio/src")).unwrap();
        fs::create_dir_all(root.join("projects/wayline/src")).unwrap();
        fs::write(root.join("projects/bio/src/config.rs"), b"").unwrap();
        fs::write(root.join("projects/wayline/src/config.rs"), b"").unwrap();
        let engine = engine_from_root(&root);

        let mut query = SearchQuery::exact("config.rs");
        query.scope = Some("projects/bio".to_string());
        let response = engine.search(&query);

        assert_eq!(response.hits.len(), 1);
        assert!(normalize_path_text(&response.hits[0].path).contains("projects/bio"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn result_limit_sets_truncated_flag() {
        let root = temp_root("limit");
        for index in 0..5 {
            fs::write(root.join(format!("config-{index}.toml")), b"").unwrap();
        }
        let engine = engine_from_root(&root);

        let mut query = SearchQuery::prefix("config-");
        query.limit = 2;
        let response = engine.search(&query);

        assert_eq!(response.hits.len(), 2);
        assert!(response.truncated);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn empty_terms_never_expand_to_every_file() {
        let root = temp_root("empty");
        fs::write(root.join("one.rs"), b"").unwrap();
        fs::write(root.join("two.rs"), b"").unwrap();
        let engine = engine_from_root(&root);

        assert!(engine.search(&SearchQuery::prefix("  ")).hits.is_empty());
        assert!(engine.search(&SearchQuery::fuzzy("  ")).hits.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fuzzy_search_ranks_multi_token_intent() {
        let root = temp_root("fuzzy-ranking");
        fs::write(root.join("restore_config.rs"), b"").unwrap();
        fs::write(root.join("restore_cache.rs"), b"").unwrap();
        fs::write(root.join("report_config.rs"), b"").unwrap();
        fs::write(root.join("unrelated.txt"), b"").unwrap();
        let engine = engine_from_root(&root);

        let response = engine.search(&SearchQuery::fuzzy("restor conf"));

        assert!(!response.hits.is_empty());
        assert_eq!(response.hits[0].name, "restore_config.rs");
        assert_eq!(response.hits[0].match_kind, SearchMatchKind::Fuzzy);
        assert!(response.hits[0].score >= FUZZY_MIN_SCORE);
        assert!(response.hits.iter().all(|hit| hit.name != "unrelated.txt"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fuzzy_search_tolerates_small_typo() {
        let root = temp_root("fuzzy-typo");
        fs::write(root.join("restore.rs"), b"").unwrap();
        fs::write(root.join("resource.rs"), b"").unwrap();
        let engine = engine_from_root(&root);

        let response = engine.search(&SearchQuery::fuzzy("restor"));

        assert!(!response.hits.is_empty());
        assert_eq!(response.hits[0].name, "restore.rs");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fuzzy_search_respects_extension_and_scope_filters() {
        let root = temp_root("fuzzy-filters");
        fs::create_dir_all(root.join("bio/src")).unwrap();
        fs::create_dir_all(root.join("wayline/src")).unwrap();
        fs::write(root.join("bio/src/restore_config.rs"), b"").unwrap();
        fs::write(root.join("bio/src/restore_config.md"), b"").unwrap();
        fs::write(root.join("wayline/src/restore_config.rs"), b"").unwrap();
        let engine = engine_from_root(&root);

        let mut query = SearchQuery::fuzzy("restor conf");
        query.extensions = vec!["rs".to_string()];
        query.scope = Some("bio".to_string());
        let response = engine.search(&query);

        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].name, "restore_config.rs");
        assert!(normalize_path_text(&response.hits[0].path).contains("/bio/src/"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fuzzy_top_k_is_bounded_and_reports_truncation() {
        let root = temp_root("fuzzy-limit");
        for index in 0..8 {
            fs::write(root.join(format!("config_restore_{index}.rs")), b"").unwrap();
        }
        let engine = engine_from_root(&root);

        let mut query = SearchQuery::fuzzy("config restore");
        query.limit = 3;
        let response = engine.search(&query);

        assert_eq!(response.hits.len(), 3);
        assert!(response.truncated);
        assert!(response
            .hits
            .windows(2)
            .all(|pair| pair[0].score >= pair[1].score));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_size_filter_is_opt_in_and_projects_metadata() {
        let root = temp_root("metadata-size");
        fs::write(root.join("artifact-small.bin"), vec![0u8; 4]).unwrap();
        fs::write(root.join("artifact-large.bin"), vec![0u8; 32]).unwrap();
        let engine = engine_from_root(&root);

        let mut query = SearchQuery::prefix("artifact-");
        query.metadata.min_size_bytes = Some(16);
        query.include_metadata = true;
        let response = engine.search(&query);

        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.hits[0].name, "artifact-large.bin");
        assert_eq!(response.hits[0].metadata.as_ref().unwrap().size_bytes, 32);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_future_modified_filter_rejects_existing_file() {
        let root = temp_root("metadata-time");
        fs::write(root.join("recent.txt"), b"recent").unwrap();
        let engine = engine_from_root(&root);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let mut query = SearchQuery::exact("recent.txt");
        query.metadata.modified_after_unix_secs = Some(now.saturating_add(3600));
        let response = engine.search(&query);

        assert!(response.hits.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn levenshtein_similarity_handles_unicode_without_panicking() {
        assert_eq!(similarity_percent("복구", "복구"), 100);
        assert!(similarity_percent("복구", "복귀") >= 50);
    }
}
