# Smart Kernel Brain v1 — First Public Release

SKB is a compact, adaptive, filename-only file locator written in Rust.

## Highlights

- Exact filename lookup without indexing file contents
- ~38.3 B/file compact payload lower bound at 1M synthetic entries
- Allocation-free LeanRef lookup path
- Resident Windows core using Named Pipe + binary IPC
- Batch lookup of up to 4096 filenames/request
- MCP tools for filename lookup and path resolution
- Mixed real-name benchmark mode with configurable misses

## Recorded validation highlights

On the initial Windows test machine:

- persistent single resident lookup: ~7.77 us/request
- mixed real-name workload, 10% misses, batch 100: ~199.9 ns/file amortized
- mixed real-name workload, 10% misses, batch 1000: ~105.8 ns/file amortized
- mixed real-name workload, 10% misses, batch 4096: ~99.6 ns/file amortized, ~10.04M effective lookups/sec
- repeated-hot batch 1000 best-case: ~44.5 ns/file amortized, ~22.46M effective lookups/sec

Batch numbers are amortized per-file costs and are not independent single-file IPC latency.

## Release boundary

The search/index core is frozen from the validated v0.7.1 line. v1 public-release work is repository packaging, documentation, release metadata, CI, and version/endpoint metadata only.

## Known limitations

- No file-content search
- No file edit/delete operations
- No live watcher yet
- `file_id` is not generation-aware yet
- Duplicate filename path-specific ranking is not claimed

See `README.md`, `BENCHMARKS.md`, and `CORE_FREEZE.md` for details.
