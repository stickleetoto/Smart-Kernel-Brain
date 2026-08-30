# Contributing

SKB v1 keeps its validated search/index core frozen.

Before changing core lookup/index files, open an issue describing:

1. the measured bottleneck;
2. the proposed change;
3. the benchmark that will prove improvement;
4. memory and correctness tradeoffs.

For ordinary changes:

```powershell
cargo fmt --check
cargo test
cargo build --release
```

Performance changes should include before/after numbers and clearly distinguish single-request latency from batched amortized cost.
