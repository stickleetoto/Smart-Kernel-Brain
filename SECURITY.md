# Security policy

SKB v1 is a local filename locator. It intentionally does not inspect file contents and does not expose file edit/delete operations.

## Report a vulnerability

Please open a GitHub security advisory or a minimal issue that does not disclose exploitable details publicly.

## Current security boundaries

- Windows resident IPC uses a local Named Pipe and rejects remote pipe clients.
- MCP tools in v1 are locator/read-oriented; they do not modify filesystem contents.
- Index files contain filenames and full paths and should be treated as local metadata. Do not publish a personal `index.skb`.
- `.gitignore` excludes SKB index/state files by default.
