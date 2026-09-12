# SKB v2 AI Foundation

## Mission

SKB v2 turns Smart Kernel Brain from a fast filename locator into a lightweight local context retrieval engine for AI agents.

The goal is **not** to embed an LLM into SKB. SKB should remain a fast, local, deterministic infrastructure layer that helps external AI systems find, select, and retrieve the smallest useful amount of local context.

## Product definition

- **SKB v1:** fast local filename locator
- **SKB v2:** fast local context retrieval engine for AI agents

SKB v2 should serve GPT, Claude, Codex, local LLMs, and other MCP clients without depending on any specific model vendor.

## Non-goals

SKB v2 should not become:

- a full LLM runtime;
- a general-purpose vector database;
- a code editor;
- an autonomous agent with unrestricted filesystem write access;
- a replacement for the validated v1 locator core.

## Core principles

1. **Preserve the v1 locator advantage.** Existing exact-name lookup, compact index, resident IPC, batching, and lazy path resolution remain valuable primitives.
2. **Correctness before live speed.** References must remain safe across index rebuilds. Generation-aware references are required before automatic reload.
3. **Cheap retrieval before semantic retrieval.** Exact, prefix, fuzzy, path, metadata, and text/symbol filtering should narrow candidates before optional embedding-based reranking.
4. **Bounded context by default.** AI-facing reads must support explicit byte, line, and token-style budgets instead of returning whole files blindly.
5. **Find and read are separate permissions.** A file may be discoverable without being readable by an AI client.
6. **Local-first.** Indexing and retrieval remain local unless a client explicitly exports returned context.
7. **Model-agnostic MCP surface.** High-level tools should be useful to any compatible AI client.
8. **Measure end-to-end retrieval quality.** Nanosecond lookup speed remains useful, but v2 benchmarks must also measure relevance, payload size, and context efficiency.

## Target architecture

```text
AI / Codex / Claude / GPT / Local LLM
                 |
            MCP / Local API
                 |
        +---------------------+
        |   SKB Agent Layer   |
        | query planning      |
        | context budgeting   |
        | policy enforcement  |
        | result packaging    |
        +----------+----------+
                   |
        +---------------------+
        | Retrieval Layer     |
        | exact filename      |
        | prefix / fuzzy      |
        | path / project      |
        | metadata filters    |
        | text / symbols      |
        | optional semantic   |
        +----------+----------+
                   |
        +---------------------+
        | SKB Locator Core    |
        | compact index       |
        | hash lookup         |
        | hot cache           |
        | batching            |
        | resident IPC        |
        +---------------------+
```

## Reference v2

Bare numeric `file_id` values are not sufficient once live reload exists.

The v2 reference model should begin with:

```rust
pub struct FileRef {
    pub generation: u64,
    pub file_id: u32,
}
```

Resolution rule:

```text
reference.generation == resident.generation
    -> resolve file_id

reference.generation != resident.generation
    -> STALE_REFERENCE
```

A stale reference must never silently resolve to another file.

## Planned retrieval capabilities

### Tier 0 — locator primitives

Keep and reuse:

- exact filename lookup;
- batched lookup;
- compact references;
- lazy path materialization;
- resident daemon;
- hot-file information.

### Tier 1 — metadata search

Add searchable metadata such as:

- extension;
- file size;
- modified time;
- directory / path scope;
- workspace / project identity;
- detected language where cheap and deterministic.

### Tier 2 — lexical retrieval

Add:

- prefix filename search;
- fuzzy filename search;
- path substring search;
- exact text search;
- bounded grep-style context;
- code symbol extraction/search where feasible.

### Tier 3 — context retrieval

Add AI-facing bounded reads:

- line ranges;
- matching-line windows;
- file heads/tails;
- symbol bodies;
- maximum bytes;
- maximum lines;
- context-pack output containing several selected fragments.

### Tier 4 — optional semantic reranking

Semantic search is optional and should remain outside the locator hot path.

Preferred flow:

```text
large local corpus
    -> cheap SKB filters
    -> small candidate set
    -> optional embedding rerank
    -> bounded context pack
```

Embeddings must not become mandatory for basic SKB operation.

## Security model

The AI layer changes SKB's trust boundary because v1 only locates files while v2 may read their contents.

Required policy concepts:

- `allow_roots`;
- `deny_roots`;
- `deny_globs`;
- binary-file rejection by default;
- maximum readable file size;
- separate `find` and `read` capability checks;
- sensitive defaults for credential/key/config locations;
- no edit/delete/write tools in the initial v2 retrieval milestone.

Examples of paths/content classes that should require denial or explicit opt-in include secrets, private keys, credential stores, browser profiles, and environment files.

## MCP v2 direction

Low-level v1 tools may remain for compatibility.

Candidate high-level v2 tools:

```text
skb_search
skb_read
skb_context
skb_changes
skb_project_map
skb_stats
```

### `skb_search`

One structured search entry point with optional fields such as:

```json
{
  "query": "restore",
  "scope": "D:/Projects/BIO",
  "extensions": ["rs", "py", "md"],
  "mode": "auto",
  "limit": 20
}
```

### `skb_read`

Bounded file-fragment retrieval.

```json
{
  "ref": {"generation": 12, "file_id": 841},
  "start_line": 120,
  "end_line": 180,
  "max_bytes": 32768
}
```

### `skb_context`

Build a compact context pack from a query and budget.

```json
{
  "query": "restore flow and validation",
  "scope": "D:/Projects/BIO",
  "max_files": 8,
  "max_bytes": 131072
}
```

## Development phases

### Phase 0 — v2 foundation

- create isolated `v2-ai` branch;
- keep v1 `main` stable;
- define architecture and invariants;
- establish v2 benchmark categories.

Exit condition: v2 work can proceed without silently changing v1 behavior.

### Phase 1 — safe live index

- add generation-aware references;
- define index generation persistence/runtime ownership;
- add stale-reference errors;
- implement watcher-triggered rebuild;
- validate rebuilt index before swap;
- atomically swap resident index;
- increment generation only after successful swap.

Exit condition: newly created/deleted/renamed files become visible automatically without stale IDs resolving incorrectly.

### Phase 2 — metadata/query engine

- add metadata representation;
- path/project scoping;
- extension filters;
- prefix search;
- fuzzy filename search;
- ranking rules;
- batch query support.

Exit condition: AI clients no longer need to know an exact filename in advance.

### Phase 3 — bounded content retrieval

- text/binary detection;
- bounded line-range reads;
- grep-style matches with context windows;
- byte/line budgets;
- sensitive-path policy layer;
- MCP `skb_read`.

Exit condition: an AI can safely retrieve only the relevant fragment instead of ingesting whole files.

### Phase 4 — context builder

- multi-file candidate ranking;
- duplicate/redundancy suppression;
- context budgeting;
- compact source metadata;
- MCP `skb_context`.

Exit condition: a natural-language task can produce a small, relevant context pack suitable for an LLM.

### Phase 5 — semantic reranking (optional)

- pluggable embedding backend;
- chunk cache;
- candidate-only reranking;
- semantic benchmark against lexical baseline.

Exit condition: semantic mode provides measurable retrieval gains large enough to justify its resource cost.

## Benchmark plan

Keep existing locator benchmarks, and add v2 metrics:

- index build time;
- live-update visibility latency;
- stale-reference correctness;
- metadata-search p50/p95;
- text-search p50/p95;
- end-to-end MCP latency;
- response payload bytes;
- bytes of source context returned;
- recall@k on a fixed retrieval set;
- irrelevant-file rate;
- context-pack redundancy;
- memory use at 100k / 1M / larger real-file indexes.

## First implementation slice

Do **not** start with embeddings.

The first coding milestone should be:

```text
FileRef { generation, file_id }
        +
resident generation ownership
        +
stale-reference-safe resolution
        +
watcher rebuild -> validate -> atomic swap
```

Only after this passes correctness and regression tests should metadata/query work begin.

## v1 compatibility rule

Until v2 is proven, `main` remains the stable v1 release line.

Changes on `v2-ai` may redesign core interfaces, but they must either:

1. retain compatibility with existing v1 behavior, or
2. explicitly document why a new v2 core validation baseline is required.

Existing v1 benchmark claims must not be reused for modified v2 core code without rerunning validation.

## Definition of success

SKB v2 succeeds when an AI client can ask for local project context without knowing filenames, receive a small relevant set of safe source fragments, and do so with lower latency and lower context/token waste than naive recursive scanning and whole-file loading.
