use crate::{GenerationEngine, MetadataFilter, SearchHitV2, SearchMode, SearchQuery};
use serde::{Deserialize, Serialize};

const DEFAULT_AUTO_LIMIT: usize = 20;
const MIN_PREFIX_CHARS: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoPlanKind {
    Empty,
    PathLike,
    SingleToken,
    FreeText,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoSearchRequest {
    pub query: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    pub path_contains: Option<String>,
    pub scope: Option<String>,
    #[serde(default)]
    pub metadata: MetadataFilter,
    #[serde(default)]
    pub include_metadata: bool,
    #[serde(default = "default_auto_limit")]
    pub limit: usize,
}

impl AutoSearchRequest {
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            extensions: Vec::new(),
            path_contains: None,
            scope: None,
            metadata: MetadataFilter::default(),
            include_metadata: false,
            limit: DEFAULT_AUTO_LIMIT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoSearchPlan {
    pub kind: AutoPlanKind,
    pub effective_term: String,
    pub inferred_path_contains: Option<String>,
    pub attempted_modes: Vec<SearchMode>,
    pub selected_mode: Option<SearchMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoSearchResponse {
    pub generation: u64,
    pub plan: AutoSearchPlan,
    pub hits: Vec<SearchHitV2>,
    pub truncated: bool,
}

impl GenerationEngine {
    pub fn search_auto(&self, request: &AutoSearchRequest) -> AutoSearchResponse {
        let planned = plan_input(request);
        if planned.term.is_empty() {
            return AutoSearchResponse {
                generation: self.generation(),
                plan: AutoSearchPlan {
                    kind: AutoPlanKind::Empty,
                    effective_term: String::new(),
                    inferred_path_contains: None,
                    attempted_modes: Vec::new(),
                    selected_mode: None,
                },
                hits: Vec::new(),
                truncated: false,
            };
        }

        let mut attempted_modes = Vec::new();
        match planned.kind {
            AutoPlanKind::PathLike | AutoPlanKind::SingleToken => {
                let exact = self.run_auto_stage(
                    request,
                    &planned.term,
                    planned.inferred_path_contains.as_deref(),
                    SearchMode::Exact,
                );
                attempted_modes.push(SearchMode::Exact);
                if !exact.hits.is_empty() {
                    return finish_auto(planned, attempted_modes, SearchMode::Exact, exact);
                }

                if planned.term.chars().count() >= MIN_PREFIX_CHARS {
                    let prefix = self.run_auto_stage(
                        request,
                        &planned.term,
                        planned.inferred_path_contains.as_deref(),
                        SearchMode::Prefix,
                    );
                    attempted_modes.push(SearchMode::Prefix);
                    if !prefix.hits.is_empty() {
                        return finish_auto(planned, attempted_modes, SearchMode::Prefix, prefix);
                    }
                }

                let fuzzy = self.run_auto_stage(
                    request,
                    &planned.term,
                    planned.inferred_path_contains.as_deref(),
                    SearchMode::Fuzzy,
                );
                attempted_modes.push(SearchMode::Fuzzy);
                finish_auto(planned, attempted_modes, SearchMode::Fuzzy, fuzzy)
            }
            AutoPlanKind::FreeText => {
                let fuzzy = self.run_auto_stage(
                    request,
                    &planned.term,
                    planned.inferred_path_contains.as_deref(),
                    SearchMode::Fuzzy,
                );
                attempted_modes.push(SearchMode::Fuzzy);
                finish_auto(planned, attempted_modes, SearchMode::Fuzzy, fuzzy)
            }
            AutoPlanKind::Empty => unreachable!("empty auto-search input returns before planning"),
        }
    }

    fn run_auto_stage(
        &self,
        request: &AutoSearchRequest,
        term: &str,
        inferred_path_contains: Option<&str>,
        mode: SearchMode,
    ) -> crate::SearchResponse {
        let mut query = match mode {
            SearchMode::Exact => SearchQuery::exact(term),
            SearchMode::Prefix => SearchQuery::prefix(term),
            SearchMode::Fuzzy => SearchQuery::fuzzy(term),
        };
        query.extensions = request.extensions.clone();
        query.path_contains = request
            .path_contains
            .clone()
            .or_else(|| inferred_path_contains.map(str::to_owned));
        query.scope = request.scope.clone();
        query.metadata = request.metadata.clone();
        query.include_metadata = request.include_metadata;
        query.limit = request.limit;
        self.search(&query)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedInput {
    kind: AutoPlanKind,
    term: String,
    inferred_path_contains: Option<String>,
}

fn default_auto_limit() -> usize {
    DEFAULT_AUTO_LIMIT
}

fn plan_input(request: &AutoSearchRequest) -> PlannedInput {
    let trimmed = request.query.trim();
    if trimmed.is_empty() {
        return PlannedInput {
            kind: AutoPlanKind::Empty,
            term: String::new(),
            inferred_path_contains: None,
        };
    }

    let normalized = trimmed.replace('\\', "/");
    if let Some((parent, basename)) = normalized.rsplit_once('/') {
        let basename = basename.trim();
        if !basename.is_empty() {
            let parent = parent.trim_matches('/').trim();
            let inferred_path_contains = if request.path_contains.is_none() && !parent.is_empty() {
                Some(parent.to_owned())
            } else {
                None
            };
            return PlannedInput {
                kind: AutoPlanKind::PathLike,
                term: basename.to_owned(),
                inferred_path_contains,
            };
        }
    }

    let kind = if normalized.split_whitespace().count() == 1 {
        AutoPlanKind::SingleToken
    } else {
        AutoPlanKind::FreeText
    };
    PlannedInput {
        kind,
        term: normalized,
        inferred_path_contains: None,
    }
}

fn finish_auto(
    planned: PlannedInput,
    attempted_modes: Vec<SearchMode>,
    selected_mode: SearchMode,
    response: crate::SearchResponse,
) -> AutoSearchResponse {
    AutoSearchResponse {
        generation: response.generation,
        plan: AutoSearchPlan {
            kind: planned.kind,
            effective_term: planned.term,
            inferred_path_contains: planned.inferred_path_contains,
            attempted_modes,
            selected_mode: Some(selected_mode),
        },
        hits: response.hits,
        truncated: response.truncated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use skb::state::UsageState;
    use skb::FileIndex;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root(label: &str) -> PathBuf {
        let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "skb-v2-auto-{label}-{}-{id}",
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
            root.join(".skb-v2-auto-state.json"),
        )
    }

    #[test]
    fn exact_hit_stops_before_expensive_fallbacks() {
        let root = temp_root("exact");
        fs::write(root.join("README.md"), b"").unwrap();
        let engine = engine_from_root(&root);

        let response = engine.search_auto(&AutoSearchRequest::new("README.md"));
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.plan.attempted_modes, vec![SearchMode::Exact]);
        assert_eq!(response.plan.selected_mode, Some(SearchMode::Exact));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn simple_token_falls_back_to_prefix_before_fuzzy() {
        let root = temp_root("prefix");
        fs::write(root.join("restore_config.rs"), b"").unwrap();
        let engine = engine_from_root(&root);

        let response = engine.search_auto(&AutoSearchRequest::new("rest"));
        assert_eq!(response.hits[0].name, "restore_config.rs");
        assert_eq!(
            response.plan.attempted_modes,
            vec![SearchMode::Exact, SearchMode::Prefix]
        );
        assert_eq!(response.plan.selected_mode, Some(SearchMode::Prefix));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn typo_reaches_fuzzy_fallback() {
        let root = temp_root("fuzzy");
        fs::write(root.join("restore.rs"), b"").unwrap();
        let engine = engine_from_root(&root);

        let response = engine.search_auto(&AutoSearchRequest::new("resotre"));
        assert_eq!(response.hits[0].name, "restore.rs");
        assert_eq!(
            response.plan.attempted_modes,
            vec![SearchMode::Exact, SearchMode::Prefix, SearchMode::Fuzzy]
        );
        assert_eq!(response.plan.selected_mode, Some(SearchMode::Fuzzy));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn free_text_goes_directly_to_fuzzy() {
        let root = temp_root("free-text");
        fs::write(root.join("restore_config.rs"), b"").unwrap();
        let engine = engine_from_root(&root);

        let response = engine.search_auto(&AutoSearchRequest::new("restor conf"));
        assert_eq!(response.hits[0].name, "restore_config.rs");
        assert_eq!(response.plan.kind, AutoPlanKind::FreeText);
        assert_eq!(response.plan.attempted_modes, vec![SearchMode::Fuzzy]);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn path_like_query_infers_path_filter_and_basename() {
        let root = temp_root("path-like");
        fs::create_dir_all(root.join("src/core")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("src/core/config.rs"), b"").unwrap();
        fs::write(root.join("docs/config.rs"), b"").unwrap();
        let engine = engine_from_root(&root);

        let response = engine.search_auto(&AutoSearchRequest::new("src/core/config.rs"));
        assert_eq!(response.hits.len(), 1);
        assert_eq!(response.plan.kind, AutoPlanKind::PathLike);
        assert_eq!(response.plan.effective_term, "config.rs");
        assert_eq!(
            response.plan.inferred_path_contains.as_deref(),
            Some("src/core")
        );
        assert!(response.hits[0].path.replace('\\', "/").contains("src/core"));

        fs::remove_dir_all(root).unwrap();
    }
}
