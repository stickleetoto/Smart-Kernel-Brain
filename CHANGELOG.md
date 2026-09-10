# Changelog

## Unreleased — Agent batching extensions

### MCP / agent throughput

- raise `skb_find_ids` from 100 to 4096 filenames per MCP call
- add `compact: true` mode to `skb_find_ids`, returning input-ordered `file_id | null` values with compact JSON text output
- add `skb_find_paths` for one-call batch filename-to-path resolution, with the same 4096-item limit and optional compact path-only output
- raise `skb_resolve_paths` from 100 to 4096 file IDs per MCP call
- report the MCP server version directly from Cargo package metadata instead of a stale hard-coded `v1` string
- expose the active MCP batch limit in `skb_stats`

These changes are implemented outside the frozen search/index/resident/state core.

## v1.1.0 — 2026-09-10

### Reliability

- route `skb scan` through same-directory temporary persistence
- flush and sync the completed temporary index before activation
- use replace-existing semantics on Windows and atomic rename semantics on Unix-like systems
- use a rollback-safe fallback when Windows replacement cannot activate the staged index directly
- preflight compact index headers and exact on-disk length before commands enter the allocating loader
- reject truncated, trailing, overflowed, or structurally impossible index files with a rebuild hint

### Build validation

- pin the validated Rust 1.98.1 toolchain
- pin direct `serde` / `serde_json` dependency versions used by the successful CI run
- run rustfmt validation on the new hardening layer without rewriting the frozen core
- run Clippy across all targets as an additional lint/build check
- add regression tests for valid preflight, truncated-index rejection, and replacement of an existing index

### Packaging

- publish Windows x86_64 and Linux x86_64 release packages
- include README, license, and release notes with each package
- publish SHA-256 checksums alongside release assets

The validated search/hash/index lookup hot path remains byte-for-byte frozen. This hardening layer is implemented around the existing core.

## v1 — First public release

Public release based on the validated v0.7.1 core line.

### Included

- compact filename-only index
- adaptive hot cache
- adaptive prefix table
- LeanRef / reusable result fast paths
- lazy path materialization
- resident core
- Windows Named Pipe binary IPC
- single and batch filename lookup
- mixed real-name benchmark and batch sweep
- duplicate filename diagnostics
- MCP interface

### Public-release polish

- repository metadata
- documentation and benchmark caveats
- CI workflow
- security and contribution notes
- GitMake publishing bundle
- core-freeze hashes

### Not included

- content search
- file modification/deletion
- generation-aware refs / live watcher
- path-specific duplicate ranking
