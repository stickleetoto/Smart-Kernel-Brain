# Smart Kernel Brain (SKB) v1

SKB is a high-performance, filename-only file locator written in Rust.

**Public release name: `SKB v1`.** Rust package metadata uses `1.0.0` internally because Cargo requires a Semantic Versioning-compatible version string; this is not exposed as the product release name.

This branch adds a **single-binary Windows shell** around the already validated v1 search core. The search/index/IPC core is frozen; the new work is installation and interface consolidation.

## One download, one executable

The intended Windows user flow is:

```text
Download SKB.exe
      |
      v
Double-click once
      |
      +--> copies itself to %LOCALAPPDATA%\Programs\SmartKernelBrain\SKB.exe
      +--> creates %LOCALAPPDATA%\SKB for index/state data
      +--> adds the install directory to the current-user PATH
      |
      v
New terminal: `skb ...`
```

The **same `SKB.exe`** is used as:

- first-run installer;
- CLI;
- resident daemon executable;
- MCP stdio server (`skb mcp`);
- repair/status/uninstaller.

There is no separate `skb-mcp.exe` and no separate setup executable.

## Build the single EXE

On Windows with Rust installed:

```powershell
.\build-single-exe.bat
```

or:

```powershell
powershell -ExecutionPolicy Bypass -File .\build-single-exe.ps1
```

The build runs the frozen-core hash check, Rust tests, a release build, and a version smoke check. Output:

```text
dist\SKB.exe
dist\SKB.exe.sha256.txt
```

## Installation

Double-click an uninstalled copy of `SKB.exe`, or run:

```powershell
.\SKB.exe install
```

Optional first scan and daemon start:

```powershell
.\SKB.exe install --scan "D:\Projects" --start-daemon
```

Default locations:

```text
Program: %LOCALAPPDATA%\Programs\SmartKernelBrain\SKB.exe
Data:    %LOCALAPPDATA%\SKB
```

No administrator rights are required.

After installation, open a new terminal:

```powershell
skb --version
skb status
skb scan "D:\Projects"
skb daemon-start
skb rfind-id README.md
```

## MCP

The MCP server is now a mode of the same executable:

```powershell
skb mcp
```

Print a ready-to-copy MCP configuration:

```powershell
skb mcp-config
```

It points to the installed executable with `args: ["mcp"]`.

Existing MCP tools remain:

- `skb_find_id`
- `skb_find_ids`
- `skb_find_refs`
- `skb_resolve_paths`
- `skb_find`
- `skb_hot_files`
- `skb_stats`

## Maintenance commands

```powershell
skb repair
skb status
skb uninstall
skb uninstall --purge-data
```

Normal uninstall preserves `%LOCALAPPDATA%\SKB`. `--purge-data` removes the index/state directory too.

## Core freeze

The following files are byte-for-byte frozen from the validated core used before this installer work:

```text
src/engine.rs
src/hash.rs
src/index.rs
src/lib.rs
src/paths.rs
src/resident.rs
src/state.rs
```

CI checks their SHA-256 hashes before tests/build. See [CORE_FREEZE.md](CORE_FREEZE.md).

## Previously measured core performance

The single-EXE work does not alter the measured lookup path. Previous Windows validation included a mixed real-name workload with 553 unique names, 10% misses, and ~0.2% hot-cache hits:

| Batch | Amortized RTT/file | Effective lookups/sec |
|---:|---:|---:|
| 1 | 7,823 ns | 127,828 |
| 100 | 199.9 ns | 5,001,901 |
| 1000 | 105.8 ns | 9,455,675 |
| 4096 | 99.6 ns | 10,041,426 |

Batch values are amortized per-file costs, not independent single-file IPC latency. See [BENCHMARKS.md](BENCHMARKS.md).

## v1 status

`SKB v1` is the single-EXE release line. The search core remains frozen; Windows first-run install, PATH registration, MCP launch, daemon launch, repair, and self-uninstall should be validated on the release build before publishing.

## License

MIT.
