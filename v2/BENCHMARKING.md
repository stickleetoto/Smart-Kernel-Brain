# SKB v2 retrieval benchmark protocol

The v1 benchmark focuses on exact filename lookup and IPC. v2 also needs retrieval-oriented measurements because prefix and fuzzy search currently scan the active compact index.

## Runner

Use the dedicated release-mode runner:

```powershell
cargo run --release --manifest-path v2/Cargo.toml --bin search_bench -- 100000 20 3
```

Arguments are:

1. synthetic file count;
2. prefix-search rounds;
3. fuzzy-search rounds.

For a larger sweep:

```powershell
cargo run --release --manifest-path v2/Cargo.toml --bin search_bench -- 1000000 20 3
```

The runner reports synthetic index construction time plus average prefix and fuzzy query latency. It intentionally does not impose a CI pass/fail threshold yet; hardware and runner variance should be measured first.

## What to record

For each machine, record:

- CPU model;
- RAM;
- OS;
- Rust toolchain;
- index entry count;
- prefix average latency;
- fuzzy average latency;
- returned hit count;
- release build mode.

Do not compare these scan-based timings directly with v1 exact-hash lookup timings. They answer different questions.

## Decision gate for an auxiliary v2 search index

Collect results at 100k and 1M entries before adding another permanent index structure. Add a prefix/trigram/token auxiliary index only if measured retrieval latency is high enough to matter for agent workflows. Any auxiliary index must remain generation-bound so a live index swap cannot mix search data from different generations.

## Metadata queries

Metadata filters are deliberately opt-in. A query that does not request metadata filtering or metadata projection should perform no filesystem `metadata()` calls. Benchmark metadata-filtered queries separately on a real filesystem because synthetic entries do not correspond to real files.
