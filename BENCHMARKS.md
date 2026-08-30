# SKB benchmark record

This document separates **core lookup latency**, **single resident IPC**, **repeated-hot batch amortization**, and **mixed real-name batch amortization**. Do not merge these into one latency claim.

## In-process compact-index benchmark

Recorded at 1,000,000 synthetic entries:

```text
payload/entry : 38.3 bytes lower bound
L0 lean/ref   : 27.4 ns
L1 lean/ref   : 169.9 ns
```

The synthetic benchmark creates metadata in memory; it does not create one million files on disk.

## Persistent single resident lookup

Windows Named Pipe + compact binary protocol:

```text
queries       : 10000
avg IPC RTT   : 7770.9 ns
avg server    : 84.9 ns
IPC overhead  : 7686.0 ns
queries/sec   : 128685
```

This is one request/response per filename over one persistent connection.

## Repeated-hot batch

Every item in each batch is the same hot `README.md` filename. These are **amortized per-file costs**, not independent single-request latencies.

| Batch size | avg/file RTT | server/file | IPC/file | Effective lookups/sec |
|---:|---:|---:|---:|---:|
| 10 | 819.7 ns | 40.9 ns | 778.8 ns | 1,219,899 |
| 100 | 120.8 ns | 31.7 ns | 89.1 ns | 8,281,368 |
| 1000 | 44.5 ns | 30.5 ns | 14.0 ns | 22,461,310 |

## Mixed real-name batch

Command:

```powershell
.\target\release\skb.exe resident-batch-sweep 500 10
```

Recorded workload:

- 553 unique real filenames from the live index;
- 10% deterministic misses;
- ~0.2% hot-cache hits;
- one persistent Windows Named Pipe connection;
- compact binary batch frames.

| Batch | RTT/file | server/file | IPC/file | Effective lookups/sec | Hot |
|---:|---:|---:|---:|---:|---:|
| 1 | 7,823.0 ns | 224.4 ns | 7,598.6 ns | 127,828 | 0.2% |
| 10 | 995.4 ns | 130.8 ns | 864.6 ns | 1,004,601 | 0.2% |
| 100 | 199.9 ns | 95.4 ns | 104.6 ns | 5,001,901 | 0.2% |
| 1000 | 105.8 ns | 87.7 ns | 18.0 ns | 9,455,675 | 0.2% |
| 4096 | 99.6 ns | 86.9 ns | 12.7 ns | 10,041,426 | 0.2% |

The mixed benchmark is the preferred result for general project discussion because it avoids repeating one hot filename.

## Reporting rules

When publishing results, include:

- SKB version;
- CPU / RAM / OS when available;
- indexed file count;
- hot vs mixed workload;
- batch size;
- hit/miss ratio;
- whether the connection was persistent;
- whether the result is independent request latency or amortized per-file cost.

Do **not** write “SKB finds one file across processes in 44.5 ns.” The 44.5 ns result is a repeated-hot 1000-item batch amortized cost.
