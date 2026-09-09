# Smart Kernel Brain v1.1.0 — Hardened Persistence Release

SKB v1.1.0 hardens index persistence and loading while keeping the validated search/hash/index lookup hot path byte-for-byte frozen.

## Highlights

- staged same-directory index writes before activation
- explicit file sync before replacing the active index
- Windows replacement path with rollback-safe fallback
- compact-index preflight before the allocating loader runs
- rejection of truncated, trailing, overflowed, or structurally impossible index files
- pinned Rust 1.98.1 toolchain and direct serde dependency versions
- expanded CI with frozen-core verification, hardening rustfmt, Clippy, tests, release build, and version smoke

## Packaging

Release assets are built from the release commit by GitHub Actions:

- `skb-v1.1.0-windows-x86_64.zip`
- `skb-v1.1.0-linux-x86_64.tar.gz`
- `SHA256SUMS.txt`

Each platform package includes the SKB executable/binary, `README.md`, `LICENSE`, and these release notes.

## Validation

The v1.1 hardening PR and the resulting `main` commit both passed the Windows and Ubuntu CI matrix: frozen-core verification, hardening-layer formatting, Clippy, tests, release build, and `--version` smoke.

## Compatibility boundary

The compact index remains version 2 and the existing search lookup contract is unchanged. This release does not introduce generation-aware file references or cross-process adaptive-state merge semantics.

## Known limitations

- no file-content search
- no file edit/delete operations
- no live watcher yet
- `file_id` is not generation-aware yet
- cross-process adaptive state still needs a dedicated merge/locking design
- duplicate filename path-specific ranking is not claimed

See `README.md`, `BENCHMARKS.md`, `CORE_FREEZE.md`, and `CHANGELOG.md` for details.
