use crate::{FileRef, GenerationEngine};
use serde::{Deserialize, Serialize};
use skb::FileIndex;
use std::path::Path;

const DEFAULT_LIMIT: usize = 32;
const MAX_LIMIT: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Exact,
    Prefix,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub term: String,
    pub mode: SearchMode,
    #[serde(default)]
    pub extensions: Vec<String>,
    pub path_contains: Option<String>,
    pub scope: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl SearchQuery {
    pub fn exact(term: impl Into<String>) -> Self {
        Self {
            term: term.into(),
            mode: SearchMode::Exact,
            extensions: Vec::new(),
            path_contains: None,
            scope: None,
            limit: DEFAULT_LIMIT,
        }
    }

    pub fn prefix(term: impl Into<String>) -> Self {
        Self {
            term: term.into(),
            mode: SearchMode::Prefix,
            extensions: Vec::new(),
            path_contains: None,
            scope: None,
            limit: DEFAULT_LIMIT,
        }
    }

    pub fn effective_limit(&self) -> usize {
        self.limit.clamp(1, MAX_LIMIT)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchHitV2 {
    pub reference: FileRef,
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub generation: u64,
    pub hits: Vec<SearchHitV2>,
    pub truncated: bool,
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
    let extension_filters = normalize_extensions(&query.extensions);
    let path_filter = query.path_contains.as_deref().map(normalize_path_text);
    let scope_filter = query.scope.as_deref().map(normalize_scope_text);
    let term_folded = query.term.to_lowercase();

    let mut hits = Vec::with_capacity(limit.min(64));
    let mut truncated = false;

    match query.mode {
        SearchMode::Exact => {
            for file_id in index.exact_candidates(&query.term) {
                if candidate_matches(
                    index,
                    file_id,
                    &extension_filters,
                    path_filter.as_deref(),
                    scope_filter.as_deref(),
                ) {
                    if hits.len() == limit {
                        truncated = true;
                        break;
                    }
                    hits.push(materialize_hit(index, generation, file_id));
                }
            }
        }
        SearchMode::Prefix => {
            for raw_id in 0..index.entry_count() {
                let file_id = raw_id as u32;
                if !index.entry_name(file_id).to_lowercase().starts_with(&term_folded) {
                    continue;
                }
                if !candidate_matches(
                    index,
                    file_id,
                    &extension_filters,
                    path_filter.as_deref(),
                    scope_filter.as_deref(),
                ) {
                    continue;
                }
                if hits.len() == limit {
                    truncated = true;
                    break;
                }
                hits.push(materialize_hit(index, generation, file_id));
            }
        }
    }

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
) -> bool {
    let name = index.entry_name(file_id);
    if !extensions.is_empty() && !matches_extension(name, extensions) {
        return false;
    }

    if path_contains.is_none() && scope.is_none() {
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

    true
}

fn materialize_hit(index: &FileIndex, generation: u64, file_id: u32) -> SearchHitV2 {
    SearchHitV2 {
        reference: FileRef {
            generation,
            file_id,
        },
        name: index.entry_name(file_id).to_owned(),
        path: index.entry_path(file_id),
    }
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

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "skb-v2-search-{label}-{}-{id}",
            std::process::id()
        ));
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
}
