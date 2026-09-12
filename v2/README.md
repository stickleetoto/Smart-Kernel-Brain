# SKB v2 development crate

This directory contains the isolated SKB v2 layer while the validated v1 locator core remains frozen.

## Current slice

Implemented in `src/lib.rs`:

- generation-aware `FileRef { generation, file_id }`;
- `GenerationEngine` wrapper around the v1 `SearchEngine`;
- explicit `STALE_REFERENCE` rejection;
- candidate root validation before activation;
- generation increment only after successful replacement;
- ordered batch resolution that preserves stale/missing errors;
- regression tests for stale-reference safety.

## Validation

Run:

```bash
cargo fmt --manifest-path v2/Cargo.toml -- --check
cargo clippy --manifest-path v2/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path v2/Cargo.toml
```

The root CI also verifies the v1 frozen-core hashes before running v2 checks.

## Next slice

1. rebuild a candidate `FileIndex` from the active root off to the side;
2. validate and atomically activate the candidate;
3. expose generation-safe references over a v2 MCP surface;
4. add filesystem change detection only after reload correctness is established.
