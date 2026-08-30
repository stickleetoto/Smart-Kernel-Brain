# Generation-aware references before live reload

## Problem

SKB v0.4+ can return a compact numeric `file_id` and delay path materialization. This is safe while the loaded compact index is immutable.

A filesystem watcher changes that assumption. Rebuilding the index can reorder entries:

```text
old generation: file_id 42 -> A\foo.rs
new generation: file_id 42 -> B\bar.rs
```

A stale reference must never silently resolve to the new file at the same numeric ID.

## Proposed post-v1.0 reference

```rust
struct IndexRef {
    generation: u64,
    file_id: u32,
}
```

The resident core owns a monotonically increasing generation. A successful rebuild/reload increments it.

## Resolution rule

```text
request generation == resident generation
    -> resolve file_id

request generation != resident generation
    -> STALE_REFERENCE
```

Never auto-resolve a stale numeric ID against a different generation.

## Watcher sequence

```text
filesystem change event
      |
      v
build new compact index off to the side
      |
validate index
      |
atomic resident swap
      |
generation++
      |
old IndexRef values become explicitly stale
```

The first watcher can rebuild the whole compact index after an OS change notification. A later delta-overlay design can make updates incremental without changing the stale-reference rule.

## Why this is required before automatic reload

SKB optimizes filename location, but correctness outranks a few microseconds. A fast locator that occasionally returns the wrong path after a rebuild is unusable for CLI, MCP and automation clients.
