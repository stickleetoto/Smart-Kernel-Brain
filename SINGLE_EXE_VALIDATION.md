# Single EXE validation checklist

After building `dist\SKB.exe` on Windows:

```powershell
.\dist\SKB.exe --version
python .\scripts\verify_core_freeze.py
```

## Clean first-run install

Run `dist\SKB.exe` with no arguments (double-click is acceptable).

Expected:

- `%LOCALAPPDATA%\Programs\SmartKernelBrain\SKB.exe` exists;
- `%LOCALAPPDATA%\SKB` exists;
- user PATH contains `%LOCALAPPDATA%\Programs\SmartKernelBrain`;

Open a **new** PowerShell and run:

```powershell
skb --version
skb status
skb mcp-config
```

## Core/CLI

```powershell
skb scan "."
skb find-id "README.md"
skb daemon-start
skb daemon-status
skb rfind-id "README.md"
skb daemon-stop
```

## MCP smoke

After scanning, this PowerShell snippet starts the same EXE in MCP mode and sends `initialize` + `tools/list`:

```powershell
@'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"smoke","version":"1"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
'@ | skb mcp
```

Expected: two JSON-RPC responses and an MCP tool list.

## Repair

```powershell
skb repair
skb status
```

## Uninstall, preserve data

```powershell
skb uninstall --quiet
```

Open a new terminal and verify the program PATH entry is gone while `%LOCALAPPDATA%\SKB` remains.

Reinstall, then optionally test destructive cleanup only if desired:

```powershell
skb uninstall --purge-data --quiet
```
## Source layout gate

The single-binary entrypoint `src/bin/skb.rs` declares `mod install;` and `mod mcp;`.
Therefore release builds require these exact sibling module paths:

- `src/bin/install.rs`
- `src/bin/mcp.rs`

`build-single-exe.ps1` checks these files before running Cargo so packaging errors fail immediately.

