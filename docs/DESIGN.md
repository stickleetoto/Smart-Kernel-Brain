# SKB v1 Design Notes — Batch Lookup

## Goal

Amortize resident IPC overhead across multiple exact filename lookups without changing the compact locator core.

## Windows transport

```text
\\.\pipe\smart-kernel-brain-v1
```

Transport remains synchronous local Named Pipe + length-prefixed binary frames. Remote pipe clients are rejected.

## Batch opcode

`OP_FIND_BATCH = 6`

Request payload:

```text
u32 count
repeat:
  u16 filename_len
  filename UTF-8 bytes
```

Response payload:

```text
u32 count
repeat:
  u8 found
  u32 file_id
  u8 hot_cache_hit
```

The normal response envelope already contains one `server_ns`, so per-item timing fields are intentionally omitted.

## Properties

- preserves input order
- 1..4096 resident filenames per request
- request frame <= 1 MiB
- no path materialization
- no adaptive metadata materialization
- reusable client request/output buffers
- same L0 -> L1 routing as single LeanRef lookup

## Benchmark methodology

`resident-batch-bench <filename> <batch_size> <batches>` reuses one persistent connection and caller-owned result buffer. The benchmark repeats the filename to control workload while still sending and parsing every filename entry.

Report both batch-level and amortized per-file numbers. Do not compare `avg/file RTT` directly with a single query latency without stating that batching changes the request model.


## v1.0 validation layer

The repeated-name batch benchmark is now complemented by a mixed-real-name workload and duplicate-name diagnostics. Automatic index reload remains deferred until lazy references carry an index generation, because bare numeric file IDs are not safe across rebuilds.
