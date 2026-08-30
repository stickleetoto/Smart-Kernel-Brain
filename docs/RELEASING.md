# Releasing SKB v1

## Validate

On Windows:

```powershell
python .\scripts\verify_core_freeze.py
cargo test
cargo build --release --bin skb
.\target\release\skb.exe --version
```

After creating an index, smoke-test MCP through the same binary:

```powershell
python .\scripts\mcp_smoke.py .\target\release\skb.exe
```

## Build distribution executable

```powershell
.\build-single-exe.bat
```

Publish only the user-facing executable plus checksum:

```text
dist\SKB.exe
dist\SKB.exe.sha256.txt
```

The source archive can remain a separate GitHub source asset if desired. Users do not need a second MCP or setup executable.

## Version label

The public tag/release name is `v1`. Cargo package metadata remains `1.0.0` only because Cargo requires a SemVer-compatible package version.
