# Core freeze — SKB v1

For the first public release, the validated search/index core is frozen. The following files are byte-for-byte identical to the tested v0.7.1 source line:

| Protected file | SHA-256 |
|---|---|
| `src/engine.rs` | `8ef8bbcf36fdb7b6688bf3ab39670cc8aecb24fe9828410fb14a2f56814e3df7` |
| `src/hash.rs` | `37c03c3505daf87827e65b1a49b0b0d837f6bdda0c608109b268cd51dad73e27` |
| `src/index.rs` | `88e3f4c774d45149428834c5573601656d947d1265084f786af80e1fc83c9052` |
| `src/lib.rs` | `1f229eec8b4eef7a408451e3bcdeb3720b6020b1a22129b97a0424f0313aeaf0` |
| `src/paths.rs` | `a1080e8e23a90536dc834d41f324be1d725b6b8c2f35b29a049f479bd80ded7c` |
| `src/state.rs` | `354b51c3efc9b671ccce9a4c242b64ff1753d9b84a1501a281bd63e55c9fd12b` |

Public-release changes are limited to documentation, repository/release metadata, CI, packaging, and version/endpoint display metadata in interface files. No search algorithm, compact-index layout, hash implementation, adaptive-weight formula, or core lookup path was changed.

`src/resident.rs` keeps the validated IPC logic; only public version/endpoint constants are updated for the v1 release. CLI/MCP source changes are version/help text only.

Any future change to a protected file should be treated as a new core change and must rerun the benchmark/validation suite.
