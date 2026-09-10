# Smart Kernel Brain (SKB) v1

**High-performance local file discovery for humans and AI agents.**

SKB is a filename-only file locator written in Rust. It keeps file discovery fast, local, and simple while exposing the same search core through a CLI, resident daemon, and MCP server.

The public release line is **`SKB v1`**. The current stable Cargo package version is `1.1.0`.

## Why SKB

| Capability | What it means |
|---|---|
| **Fast local lookup** | The validated search core is optimized for filename lookup and batched resolution. |
| **One executable** | The same `SKB.exe` installs, searches, runs the daemon, serves MCP, repairs, and uninstalls. |
| **Agent-ready MCP** | AI agents can discover and resolve local files without needing a separate service binary. |
| **Resident mode** | A daemon keeps the search path warm for repeated local queries. |
| **Frozen core** | The validated search/index/IPC core is hash-checked so interface work cannot silently change it. |
| **User-local install** | Windows installation requires no administrator rights. |

## Quick start

Install by double-clicking `SKB.exe`, or run:

```powershell
.\SKB.exe install
```

Then open a new terminal:

```powershell
skb --version
skb scan "D:\Projects"
skb daemon-start
skb rfind-id README.md
```

Optional first scan and daemon start during installation:

```powershell
.\SKB.exe install --scan "D:\Projects" --start-daemon
```

Default locations:

```text
Program: %LOCALAPPDATA%\Programs\SmartKernelBrain\SKB.exe
Data:    %LOCALAPPDATA%\SKB
```

## One binary, multiple roles

```text
SKB.exe
  ├─ first-run installer
  ├─ CLI
  ├─ resident daemon
  ├─ MCP stdio server
  ├─ status / repair
  └─ uninstaller
```

There is no separate setup executable and no separate `skb-mcp.exe`.

## Performance

Previous Windows validation used a mixed real-name workload with 553 unique names, 10% misses, and ~0.2% hot-cache hits:

| Batch | Amortized RTT/file | Effective lookups/sec |
|---:|---:|---:|
| 1 | 7,823 ns | 127,828 |
| 100 | 199.9 ns | 5,001,901 |
| 1000 | 105.8 ns | 9,455,675 |
| 4096 | 99.6 ns | 10,041,426 |

Batch values are amortized per-file costs, not independent single-file IPC latency. See [BENCHMARKS.md](BENCHMARKS.md) for the benchmark details.

## MCP

Run the MCP server from the same executable:

```powershell
skb mcp
```

Generate a ready-to-copy MCP configuration:

```powershell
skb mcp-config
```

Available tools:

- `skb_find_id`
- `skb_find_ids`
- `skb_find_paths`
- `skb_find_refs`
- `skb_resolve_paths`
- `skb_find`
- `skb_hot_files`
- `skb_stats`

### Agent batching

`skb_find_ids` accepts up to **4096 filenames per MCP call**. Use `compact: true` when the agent only needs file IDs; the result becomes an input-ordered array of `file_id | null` values and the text payload is serialized without pretty-print whitespace.

When paths are definitely required, `skb_find_paths` combines first-hit lookup and path resolution into one MCP tool call. It also accepts up to 4096 filenames and supports `compact: true`, returning `path | null` values in input order.

`skb_resolve_paths` also accepts up to 4096 file IDs, so a large ID batch can be resolved without being split into many MCP calls.

The generated descriptor points to the installed executable with `args: ["mcp"]`.

## Build

On Windows with Rust installed:

```powershell
.\build-single-exe.bat
```

or:

```powershell
powershell -ExecutionPolicy Bypass -File .\build-single-exe.ps1
```

The build performs the frozen-core hash check, Rust tests, a release build, and a version smoke check.

Output:

```text
dist\SKB.exe
dist\SKB.exe.sha256.txt
```

## Core freeze

The validated search core is byte-for-byte frozen in:

```text
src/engine.rs
src/hash.rs
src/index.rs
src/lib.rs
src/paths.rs
src/resident.rs
src/state.rs
```

CI checks the SHA-256 hashes of these files before tests and builds. See [CORE_FREEZE.md](CORE_FREEZE.md).

## Maintenance

```powershell
skb status
skb repair
skb uninstall
skb uninstall --purge-data
```

Normal uninstall preserves `%LOCALAPPDATA%\SKB`. `--purge-data` removes the index and state data as well.

## v1 status

`SKB v1` is the single-EXE release line. The search core remains frozen while Windows installation, PATH registration, daemon launch, MCP launch, repair, self-uninstall, and agent-facing batching evolve around it.

## License

MIT.
