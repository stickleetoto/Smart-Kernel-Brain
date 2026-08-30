# SKB v1 validation status

## User-validated lineage

The v1 public release freezes the search/index core from the v0.7.1 line that was built and exercised on Windows.

Observed tests included:

- Rust release build sufficient to run the validated binaries;
- resident daemon startup/status;
- exact resident lookup;
- Windows Named Pipe binary IPC;
- repeated-hot batch benchmark;
- mixed real-name batch benchmark with 10% misses;
- batch-size sweep through 4096 items.

Representative mixed batch-sweep result:

```text
 batch  RTT/file ns    server ns       IPC ns    lookups/sec     hot %
     1       7823.0        224.4       7598.6         127828      0.2%
    10        995.4        130.8        864.6        1004601      0.2%
   100        199.9         95.4        104.6        5001901      0.2%
  1000        105.8         87.7         18.0        9455675      0.2%
  4096         99.6         86.9         12.7       10041426      0.2%
```

Workload: 553 unique real filenames, deterministic 10% misses, one persistent Named Pipe connection.

## Public-release modifications

No protected search/index core file changed. See `CORE_FREEZE.md`.

The v1 packaging pass changes documentation, Cargo/repository version metadata, CLI/MCP display versions, the resident endpoint/version constants, CI, and release files.

## Still required before claiming broad comparative superiority

- record CPU/RAM/Windows version for the benchmark machine;
- test a much larger real NTFS index;
- compare under matched conditions with established filename search tools;
- validate cold-start and index-update behavior separately;
- implement generation-safe references before live index reload/watcher support.
