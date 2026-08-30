mod install;
mod mcp;

use skb::hash::filename_hash;
use skb::state::UsageState;
use skb::{resident_addr, FileIndex, ResidentClient, SearchEngine, SkbPaths};
use skb::resident::run_daemon;
use std::collections::{HashMap, HashSet};
use std::env;
use std::hint::black_box;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const V02_BASELINE_BYTES_PER_ENTRY: f64 = 231.0;

fn main() {
    if let Err(e) = run() {
        eprintln!("skb: {e}");
        std::process::exit(1);
    }
}

fn run() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let Some(cmd) = args.get(1).map(String::as_str) else {
        if install::handle_no_args()? {
            return Ok(());
        }
        print_help();
        return Ok(());
    };

    match cmd {
        "install" => return install::install(&args[2..], false, false),
        "repair" => return install::install(&args[2..], true, false),
        "uninstall" => return install::uninstall(&args[2..]),
        "status" | "install-status" => return install::status(),
        "mcp-config" => return install::print_mcp_config(),
        "mcp" => return mcp::run(),
        "version" | "--version" | "-V" => {
            println!("SKB {}", install::PRODUCT_VERSION);
            return Ok(());
        }
        _ => {}
    }

    let paths = SkbPaths::discover()?;

    match cmd {
        "scan" => {
            let root = args.get(2).map(String::as_str).unwrap_or(".");
            paths.ensure_home()?;
            println!("SKB compact scan: {}", root);
            let (index, report) = FileIndex::scan(Path::new(root))?;
            let payload = index.payload_bytes();
            let per_file = bytes_per_file(payload, index.entry_count());
            index.save(&paths.index)?;
            println!("indexed       : {} files", report.files_indexed);
            println!("directories   : {}", report.directories_seen);
            println!("skipped       : {}", report.skipped_entries);
            println!("elapsed       : {:.3}s", report.elapsed.as_secs_f64());
            println!("compact RAM   : {} bytes lowerbound", payload);
            println!("RAM/file      : {:.1} bytes lowerbound", per_file);
            println!("index         : {}", paths.index.display());
        }
        "find" => {
            let Some(name) = args.get(2) else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: skb find <filename> [limit]"));
            };
            let limit = args.get(3).and_then(|v| v.parse::<usize>().ok()).unwrap_or(20).clamp(1, 1000);
            let mut engine = load_engine(&paths)?;
            let (hits, elapsed_us) = engine.find(name, limit, true)?;
            println!("query         : {name}");
            println!("latency       : {elapsed_us} us");
            if hits.is_empty() {
                println!("not found");
            } else {
                for (i, hit) in hits.iter().enumerate() {
                    println!("{:>2}. {}  [score {:.4}, count {}, hot={}]", i + 1, hit.path, hit.score, hit.access_count, hit.hot_cache_hit);
                }
            }
        }
        "find-id" | "find-ref" => {
            let Some(name) = args.get(2) else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: skb find-id <filename> [limit]"));
            };
            let limit = args.get(3).and_then(|v| v.parse::<usize>().ok()).unwrap_or(20).clamp(1, 1000);
            let mut engine = load_engine(&paths)?;
            let (refs, elapsed_ns) = engine.find_lazy(name, limit, true)?;
            println!("query         : {name}");
            println!("lazy latency  : {elapsed_ns} ns");
            if refs.is_empty() {
                println!("not found");
            } else {
                for (i, hit) in refs.iter().enumerate() {
                    println!("{:>2}. file_id={}  [score {:.4}, count {}, hot={}]", i + 1, hit.file_id, hit.score, hit.access_count, hit.hot_cache_hit);
                }
                println!("hint          : resolve with `skb path <file_id>` only when a path is needed");
            }
        }
        "path" | "resolve" => {
            let Some(raw_id) = args.get(2) else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: skb path <file_id>"));
            };
            let file_id = raw_id.parse::<u32>().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "file_id must be a u32 integer")
            })?;
            let engine = load_engine(&paths)?;
            match engine.resolve_file(file_id) {
                Some(file) => {
                    println!("file_id       : {}", file.file_id);
                    println!("name          : {}", file.name);
                    println!("path          : {}", file.path);
                }
                None => println!("invalid file_id: {file_id}"),
            }
        }
        "hot" => {
            let limit = args.get(2).and_then(|v| v.parse::<usize>().ok()).unwrap_or(20).clamp(1, 1000);
            let engine = load_engine(&paths)?;
            for (i, name) in engine.hot_names(limit).iter().enumerate() {
                println!("{:>2}. {:>8.4}  {:>6}x  {:>3} paths  {}", i + 1, name.score, name.access_count, name.matching_paths, name.name);
            }
        }
        "stats" => {
            let engine = load_engine(&paths)?;
            let index_bytes = std::fs::metadata(&paths.index).map(|m| m.len()).unwrap_or(0);
            let payload = engine.index.payload_bytes();
            println!("root          : {}", engine.index.root);
            println!("files         : {}", engine.index.entry_count());
            println!("directories   : {}", engine.index.directory_count());
            println!("index bytes   : {}", index_bytes);
            println!("compact RAM   : {} bytes lowerbound", payload);
            println!("RAM/file      : {:.1} bytes lowerbound", bytes_per_file(payload, engine.index.entry_count()));
            println!("fixed/file    : {} bytes", FileIndex::fixed_metadata_bytes_per_file());
            println!("prefix table  : {} bits / {} bytes", engine.index.prefix_bits(), engine.index.prefix_table_bytes());
            println!("hot names     : {}", engine.hot_name_count());
            println!("hot entries   : {}", engine.hot_entry_count());
            println!("hot slots     : {} ({}-way)", engine.hot_slot_count(), 4);
            println!("state records : {}", engine.state.records.len());
            println!("SKB_HOME      : {}", paths.home.display());
        }
        "daemon" => {
            let addr = resident_addr();
            run_daemon(&paths, &addr)?;
        }
        "daemon-start" => {
            start_daemon()?;
        }
        "daemon-stop" => {
            let endpoint = resident_addr();
            let mut client = ResidentClient::connect(&endpoint)?;
            let stopped = client.shutdown()?;
            println!("resident      : {}", if stopped { "stopping" } else { "unknown" });
            println!("endpoint      : {endpoint}");
        }
        "daemon-status" => {
            let endpoint = resident_addr();
            match ResidentClient::connect(&endpoint).and_then(|mut c| c.ping()) {
                Ok(response) => {
                    println!("resident      : ready");
                    println!("endpoint      : {endpoint}");
                    println!("transport     : {}", response.transport);
                    println!("version       : {}", response.version);
                    println!("files         : {}", response.files);
                }
                Err(e) => {
                    println!("resident      : not running");
                    println!("endpoint      : {endpoint}");
                    println!("error         : {e}");
                }
            }
        }
        "rfind-id" | "resident-find-id" => {
            let Some(name) = args.get(2) else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: skb rfind-id <filename>"));
            };
            let endpoint = resident_addr();
            let mut client = ResidentClient::connect(&endpoint)?;
            let start = Instant::now();
            let response = client.find_first(name)?;
            let rtt = start.elapsed();
            println!("query         : {name}");
            println!("ipc RTT       : {:.1} us", rtt.as_nanos() as f64 / 1000.0);
            match response {
                Some(hit) => {
                    println!("server lookup : {} ns", hit.server_ns);
                    println!("file_id       : {}", hit.file_id);
                    println!("hot           : {}", hit.hot_cache_hit);
                }
                None => println!("not found"),
            }
        }
        "rfind-ids" | "resident-find-ids" => {
            if args.len() < 3 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: skb rfind-ids <filename> [filename ...]"));
            }
            let names: Vec<&str> = args[2..].iter().map(String::as_str).collect();
            if names.len() > 4096 {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "rfind-ids accepts at most 4096 filenames per batch"));
            }
            let endpoint = resident_addr();
            let mut client = ResidentClient::connect(&endpoint)?;
            let start = Instant::now();
            let response = client.find_batch(&names)?;
            let rtt = start.elapsed();
            println!("batch size    : {}", names.len());
            println!("ipc RTT       : {:.1} us", rtt.as_nanos() as f64 / 1000.0);
            println!("server batch  : {} ns", response.server_ns);
            for (name, hit) in names.iter().zip(response.results.iter()) {
                match hit {
                    Some(hit) => println!("{name} -> file_id={} hot={}", hit.file_id, hit.hot_cache_hit),
                    None => println!("{name} -> not found"),
                }
            }
        }
        "rpath" | "resident-path" => {
            let Some(raw_id) = args.get(2) else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: skb rpath <file_id>"));
            };
            let file_id = raw_id.parse::<u32>().map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "file_id must be a u32 integer"))?;
            let endpoint = resident_addr();
            let mut client = ResidentClient::connect(&endpoint)?;
            let start = Instant::now();
            let response = client.resolve(file_id)?;
            println!("ipc RTT       : {:.1} us", start.elapsed().as_nanos() as f64 / 1000.0);
            match response {
                Some(file) => {
                    println!("server resolve: {} ns", file.server_ns);
                    println!("file_id       : {}", file.file_id);
                    println!("name          : {}", file.name);
                    println!("path          : {}", file.path);
                }
                None => println!("invalid file_id: {file_id}"),
            }
        }
        "resident-stats" => {
            let endpoint = resident_addr();
            let mut client = ResidentClient::connect(&endpoint)?;
            let response = client.stats()?;
            println!("{}", serde_json::to_string_pretty(&response)
                .unwrap_or_else(|_| format!("{response:?}")));
        }
        "resident-bench" => {
            let Some(name) = args.get(2) else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: skb resident-bench <filename> [queries]"));
            };
            let queries = args.get(3).and_then(|v| v.parse::<usize>().ok()).unwrap_or(10_000).clamp(1, 1_000_000);
            resident_benchmark(name, queries)?;
        }
        "resident-batch-bench" => {
            let Some(name) = args.get(2) else {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "usage: skb resident-batch-bench <filename> [batch_size] [batches]"));
            };
            let batch_size = args.get(3).and_then(|v| v.parse::<usize>().ok()).unwrap_or(100).clamp(1, 4096);
            let batches = args.get(4).and_then(|v| v.parse::<usize>().ok()).unwrap_or(1000).clamp(1, 100_000);
            resident_batch_benchmark(name, batch_size, batches)?;
        }
        "resident-mixed-batch-bench" => {
            let batch_size = args.get(2).and_then(|v| v.parse::<usize>().ok()).unwrap_or(100).clamp(1, 4096);
            let batches = args.get(3).and_then(|v| v.parse::<usize>().ok()).unwrap_or(1000).clamp(1, 100_000);
            let miss_percent = args.get(4).and_then(|v| v.parse::<u32>().ok()).unwrap_or(10).min(100);
            resident_mixed_batch_benchmark(&paths, batch_size, batches, miss_percent)?;
        }
        "resident-batch-sweep" => {
            let batches = args.get(2).and_then(|v| v.parse::<usize>().ok()).unwrap_or(500).clamp(1, 100_000);
            let miss_percent = args.get(3).and_then(|v| v.parse::<u32>().ok()).unwrap_or(10).min(100);
            resident_batch_sweep(&paths, batches, miss_percent)?;
        }
        "duplicates" => {
            let limit = args.get(2).and_then(|v| v.parse::<usize>().ok()).unwrap_or(20).clamp(1, 1000);
            duplicate_report(&paths, limit)?;
        }
        "benchmark" => {
            let queries = args.get(2).and_then(|v| v.parse::<usize>().ok()).unwrap_or(100_000).clamp(1, 10_000_000);
            benchmark(&paths, queries)?;
        }
        "synthetic" => {
            let entries = args.get(2).and_then(|v| v.parse::<usize>().ok()).unwrap_or(100_000).clamp(1, 10_000_000);
            let queries = args.get(3).and_then(|v| v.parse::<usize>().ok()).unwrap_or(1_000_000).clamp(1, 50_000_000);
            synthetic_benchmark(entries, queries)?;
        }
        "help" | "--help" | "-h" => print_help(),
        _ => {
            print_help();
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("unknown command: {cmd}")));
        }
    }
    Ok(())
}

fn load_engine(paths: &SkbPaths) -> io::Result<SearchEngine> {
    let index = FileIndex::load(&paths.index).map_err(|e| {
        io::Error::new(e.kind(), format!("cannot load compact index (run `skb scan <root>` first): {e}"))
    })?;
    let state = UsageState::load(&paths.state)?;
    Ok(SearchEngine::new(index, state, paths.state.clone()))
}

fn benchmark(paths: &SkbPaths, queries: usize) -> io::Result<()> {
    let mut engine = load_engine(paths)?;
    if engine.index.entry_count() == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "index is empty"));
    }

    let sample_count = engine.index.entry_count().min(4096).max(1);
    let names: Vec<String> = (0..sample_count)
        .map(|i| {
            let idx = i.saturating_mul(engine.index.entry_count()) / sample_count;
            engine.index.entry_name(idx.min(engine.index.entry_count() - 1) as u32).to_owned()
        })
        .collect();
    let hashes: Vec<u64> = names.iter().map(|name| filename_hash(name)).collect();

    let mut rng = 0x9e3779b97f4a7c15u64;
    let start = Instant::now();
    let mut found = 0usize;
    for _ in 0..queries {
        xorshift64(&mut rng);
        let slot = (rng as usize) % names.len();
        found += usize::from(
            engine
                .l1_lookup_first_id_with_hash(black_box(&names[slot]), black_box(hashes[slot]))
                .is_some(),
        );
    }
    let raw_elapsed = start.elapsed();

    let lean_queries = queries.min(1_000_000);
    let mut rng = 0x6a09e667f3bcc909u64;
    let start = Instant::now();
    let mut lean_found = 0usize;
    for _ in 0..lean_queries {
        xorshift64(&mut rng);
        let name = &names[(rng as usize) % names.len()];
        lean_found += usize::from(engine.find_first_ref(black_box(name)).is_some());
    }
    let lean_elapsed = start.elapsed();

    let mut reuse = Vec::with_capacity(1);
    let mut rng = 0xbb67ae8584caa73bu64;
    let start = Instant::now();
    let mut reuse_found = 0usize;
    for _ in 0..lean_queries {
        xorshift64(&mut rng);
        let name = &names[(rng as usize) % names.len()];
        reuse_found += usize::from(engine.find_refs_reuse(black_box(name), 1, &mut reuse) != 0);
        black_box(&reuse);
    }
    let reuse_elapsed = start.elapsed();

    let lazy_queries = queries.min(1_000_000);
    let mut rng = 0x94d049bb133111ebu64;
    let start = Instant::now();
    let mut lazy_found = 0usize;
    for _ in 0..lazy_queries {
        xorshift64(&mut rng);
        let name = &names[(rng as usize) % names.len()];
        let refs = engine.find_refs(black_box(name), 1, false)?;
        lazy_found += usize::from(!black_box(refs).is_empty());
    }
    let lazy_elapsed = start.elapsed();

    let full_queries = queries.min(1_000_000);
    let mut rng = 0xd1b54a32d192ed03u64;
    let start = Instant::now();
    let mut full_found = 0usize;
    for _ in 0..full_queries {
        xorshift64(&mut rng);
        let name = &names[(rng as usize) % names.len()];
        let (hits, _) = engine.find(black_box(name), 1, false)?;
        full_found += usize::from(!black_box(hits).is_empty());
    }
    let full_elapsed = start.elapsed();

    println!("queries       : {queries}");
    println!("raw found     : {found}");
    print_metric("L1 raw/core", raw_elapsed, queries);
    println!("lean found    : {lean_found}");
    print_metric("L1 lean/ref", lean_elapsed, lean_queries);
    println!("reuse found   : {reuse_found}");
    print_metric("L1 reuse/ref", reuse_elapsed, lean_queries);
    println!("lazy found    : {lazy_found}");
    print_metric("L1 lazy/ref", lazy_elapsed, lazy_queries);
    println!("full queries  : {full_queries}");
    println!("full found    : {full_found}");
    print_metric("L1 full/find", full_elapsed, full_queries);
    println!("note          : lazy/ref returns file_id metadata without path Strings; full/find resolves paths for compatibility");
    Ok(())
}

fn synthetic_benchmark(entries: usize, queries: usize) -> io::Result<()> {
    let build_start = Instant::now();
    let index = FileIndex::synthetic(entries);
    let build_elapsed = build_start.elapsed();
    let payload_bytes = index.payload_bytes();
    let prefix_table_bytes = index.prefix_table_bytes();
    let prefix_bits = index.prefix_bits();

    let sample_count = entries.min(4096).max(1);
    let mut samples = Vec::with_capacity(sample_count);
    for i in 0..sample_count {
        let idx = i.saturating_mul(entries) / sample_count;
        samples.push(format!("file_{:08}.dat", idx.min(entries - 1)));
    }
    let hashes: Vec<u64> = samples.iter().map(|name| filename_hash(name)).collect();

    let dummy_state = std::env::temp_dir().join("skb-synthetic-state-unused.json");
    let mut engine = SearchEngine::new(index, UsageState::default(), dummy_state);

    // 1) Hash cost by itself. A checksum keeps the work observable to the optimizer.
    let mut rng = 0x13198a2e03707344u64;
    let hash_start = Instant::now();
    let mut hash_checksum = 0u64;
    for _ in 0..queries {
        xorshift64(&mut rng);
        let name = &samples[(rng as usize) % samples.len()];
        hash_checksum ^= black_box(filename_hash(black_box(name)));
    }
    let hash_elapsed = hash_start.elapsed();
    black_box(hash_checksum);

    // 2) L1 core: pre-hashed, allocation-free name -> file-id.
    let mut rng = 0x243f6a8885a308d3u64;
    let l1_core_start = Instant::now();
    let mut l1_core_found = 0usize;
    for _ in 0..queries {
        xorshift64(&mut rng);
        let slot = (rng as usize) % samples.len();
        l1_core_found += usize::from(
            engine
                .l1_lookup_first_id_with_hash(black_box(&samples[slot]), black_box(hashes[slot]))
                .is_some(),
        );
    }
    let l1_core_elapsed = l1_core_start.elapsed();

    // 3) L1 raw: hash + allocation-free name -> file-id, no path/SearchHit.
    let mut rng = 0xa4093822299f31d0u64;
    let l1_raw_start = Instant::now();
    let mut l1_raw_found = 0usize;
    for _ in 0..queries {
        xorshift64(&mut rng);
        let name = &samples[(rng as usize) % samples.len()];
        l1_raw_found += usize::from(engine.index.lookup_first_id(black_box(name)).is_some());
    }
    let l1_raw_elapsed = l1_raw_start.elapsed();

    // 4) v0.4.1 lean result: hash + core lookup + tiny Copy result, no Vec/score/state/path.
    let lean_queries = queries.min(1_000_000);
    let mut rng = 0x6a09e667f3bcc909u64;
    let l1_lean_start = Instant::now();
    let mut l1_lean_found = 0usize;
    for _ in 0..lean_queries {
        xorshift64(&mut rng);
        let name = &samples[(rng as usize) % samples.len()];
        l1_lean_found += usize::from(engine.find_first_ref(black_box(name)).is_some());
    }
    let l1_lean_elapsed = l1_lean_start.elapsed();

    // 5) Caller-owned reusable result buffer. Capacity is allocated once, outside timing.
    let mut reuse = Vec::with_capacity(1);
    let mut rng = 0xbb67ae8584caa73bu64;
    let l1_reuse_start = Instant::now();
    let mut l1_reuse_found = 0usize;
    for _ in 0..lean_queries {
        xorshift64(&mut rng);
        let name = &samples[(rng as usize) % samples.len()];
        l1_reuse_found += usize::from(engine.find_refs_reuse(black_box(name), 1, &mut reuse) != 0);
        black_box(&reuse);
    }
    let l1_reuse_elapsed = l1_reuse_start.elapsed();

    // 6) v0.4 enriched lazy result: candidate collection + FileRefHit metadata.
    let lazy_queries = queries.min(1_000_000);
    let mut rng = 0x3bd39e10cb0ef593u64;
    let l1_lazy_start = Instant::now();
    let mut l1_lazy_found = 0usize;
    for _ in 0..lazy_queries {
        xorshift64(&mut rng);
        let name = &samples[(rng as usize) % samples.len()];
        let refs = engine.find_refs(black_box(name), 1, false)?;
        l1_lazy_found += usize::from(!black_box(refs).is_empty());
    }
    let l1_lazy_elapsed = l1_lazy_start.elapsed();

    // 5) Path materialization cost by itself. Cap this phase because it allocates Strings.
    let materialize_queries = queries.min(1_000_000);
    let sample_ids: Vec<u32> = samples
        .iter()
        .zip(hashes.iter().copied())
        .map(|(name, hash)| engine.index.lookup_first_id_with_hash(name, hash).expect("synthetic sample missing"))
        .collect();
    let mut rng = 0x082efa98ec4e6c89u64;
    let path_start = Instant::now();
    let mut path_bytes = 0usize;
    for _ in 0..materialize_queries {
        xorshift64(&mut rng);
        let file_id = sample_ids[(rng as usize) % sample_ids.len()];
        path_bytes = path_bytes.wrapping_add(black_box(engine.index.entry_path(black_box(file_id))).len());
    }
    let path_elapsed = path_start.elapsed();
    black_box(path_bytes);

    // 5) Full cold/L1 find includes hash, candidate Vec, sorting, path and SearchHit.
    let mut rng = 0x452821e638d01377u64;
    let l1_full_start = Instant::now();
    let mut l1_full_found = 0usize;
    for _ in 0..materialize_queries {
        xorshift64(&mut rng);
        let name = &samples[(rng as usize) % samples.len()];
        let (hits, _) = engine.find(black_box(name), 1, false)?;
        l1_full_found += usize::from(!black_box(hits).is_empty());
    }
    let l1_full_elapsed = l1_full_start.elapsed();

    // Promote one filename. L0 uses a fixed 4-way set-associative array.
    let hot_name = samples[0].clone();
    let hot_hash = hashes[0];
    for _ in 0..8 {
        engine.learn_in_memory(&hot_name);
    }

    // 6) L0 core: pre-hashed direct cache lookup + real filename verification.
    let l0_core_start = Instant::now();
    let mut l0_core_found = 0usize;
    for _ in 0..queries {
        l0_core_found += usize::from(
            engine
                .l0_lookup_first_id_with_hash(black_box(&hot_name), black_box(hot_hash))
                .is_some(),
        );
    }
    let l0_core_elapsed = l0_core_start.elapsed();

    // 7) L0 raw: includes SKB's filename hash, still no path/SearchHit allocation.
    let l0_raw_start = Instant::now();
    let mut l0_raw_found = 0usize;
    for _ in 0..queries {
        l0_raw_found += usize::from(engine.l0_lookup_first_id(black_box(&hot_name)).is_some());
    }
    let l0_raw_elapsed = l0_raw_start.elapsed();

    // 9) Lean hot result: no Vec, score calculation, usage clone, or path.
    let l0_lean_start = Instant::now();
    let mut l0_lean_found = 0usize;
    for _ in 0..lean_queries {
        l0_lean_found += usize::from(engine.find_first_ref(black_box(&hot_name)).is_some());
    }
    let l0_lean_elapsed = l0_lean_start.elapsed();

    // 10) Hot result into a caller-owned buffer; no per-query heap allocation.
    let mut hot_reuse = Vec::with_capacity(1);
    let l0_reuse_start = Instant::now();
    let mut l0_reuse_found = 0usize;
    for _ in 0..lean_queries {
        l0_reuse_found += usize::from(engine.find_refs_reuse(black_box(&hot_name), 1, &mut hot_reuse) != 0);
        black_box(&hot_reuse);
    }
    let l0_reuse_elapsed = l0_reuse_start.elapsed();

    // 11) Enriched lazy hot find returns FileRefHit; still no path/name String materialization.
    let l0_lazy_start = Instant::now();
    let mut l0_lazy_found = 0usize;
    for _ in 0..lazy_queries {
        let refs = engine.find_refs(black_box(&hot_name), 1, false)?;
        l0_lazy_found += usize::from(!black_box(refs).is_empty());
    }
    let l0_lazy_elapsed = l0_lazy_start.elapsed();

    // 10) Full hot find for apples-to-apples comparison with older releases.
    let l0_full_start = Instant::now();
    let mut l0_full_found = 0usize;
    for _ in 0..materialize_queries {
        let (hits, _) = engine.find(black_box(&hot_name), 1, false)?;
        l0_full_found += usize::from(!black_box(hits).is_empty());
    }
    let l0_full_elapsed = l0_full_start.elapsed();

    let bytes_per_entry = bytes_per_file(payload_bytes, entries);
    let reduction = if bytes_per_entry > 0.0 {
        (1.0 - bytes_per_entry / V02_BASELINE_BYTES_PER_ENTRY) * 100.0
    } else {
        0.0
    };

    println!("synthetic entries : {entries}");
    println!("queries           : {queries}");
    println!("materialize q     : {materialize_queries}");
    println!("build elapsed     : {:.6}s", build_elapsed.as_secs_f64());
    println!("compact lowerbound: {} bytes", payload_bytes);
    println!("payload/entry     : {:.1} bytes", bytes_per_entry);
    println!("fixed metadata    : {} bytes/entry", FileIndex::fixed_metadata_bytes_per_file());
    println!("prefix table      : {} bits / {} bytes", prefix_bits, prefix_table_bytes);
    println!("hot cache         : {} slots / 4-way", engine.hot_slot_count());
    println!("vs v0.2 231 B     : {:.1}% smaller", reduction.max(0.0));
    println!("hash checksum     : {hash_checksum}");
    print_metric("HASH only", hash_elapsed, queries);
    println!("L1 core found     : {l1_core_found}");
    print_metric("L1 raw/core", l1_core_elapsed, queries);
    println!("L1 raw found      : {l1_raw_found}");
    print_metric("L1 name->id", l1_raw_elapsed, queries);
    println!("L1 lean found     : {l1_lean_found}");
    print_metric("L1 lean/ref", l1_lean_elapsed, lean_queries);
    println!("L1 reuse found    : {l1_reuse_found}");
    print_metric("L1 reuse/ref", l1_reuse_elapsed, lean_queries);
    println!("L1 lazy found     : {l1_lazy_found}");
    print_metric("L1 lazy/ref", l1_lazy_elapsed, lazy_queries);
    print_metric("PATH materialize", path_elapsed, materialize_queries);
    println!("L1 full found     : {l1_full_found}");
    print_metric("L1 full/find", l1_full_elapsed, materialize_queries);
    println!("L0 core found     : {l0_core_found}");
    print_metric("L0 raw/core", l0_core_elapsed, queries);
    println!("L0 raw found      : {l0_raw_found}");
    print_metric("L0 name->id", l0_raw_elapsed, queries);
    println!("L0 lean found     : {l0_lean_found}");
    print_metric("L0 lean/ref", l0_lean_elapsed, lean_queries);
    println!("L0 reuse found    : {l0_reuse_found}");
    print_metric("L0 reuse/ref", l0_reuse_elapsed, lean_queries);
    println!("L0 lazy found     : {l0_lazy_found}");
    print_metric("L0 lazy/ref", l0_lazy_elapsed, lazy_queries);
    println!("L0 full found     : {l0_full_found}");
    print_metric("L0 full/find", l0_full_elapsed, materialize_queries);
    println!("note              : lean/ref is allocation-free single-hit; reuse/ref reuses caller storage; lazy/ref keeps adaptive metadata");
    Ok(())
}


fn start_daemon() -> io::Result<()> {
    let endpoint = resident_addr();
    if let Ok(mut client) = ResidentClient::connect(&endpoint) {
        if client.ping().is_ok() {
            println!("resident      : already running");
            println!("endpoint      : {endpoint}");
            return Ok(());
        }
    }

    let exe = env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    command.spawn()?;

    for _ in 0..40 {
        thread::sleep(Duration::from_millis(25));
        if let Ok(mut client) = ResidentClient::connect(&endpoint) {
            if client.ping().is_ok() {
                println!("resident      : started");
                println!("endpoint      : {endpoint}");
                return Ok(());
            }
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "resident core did not become ready"))
}

fn resident_benchmark(name: &str, queries: usize) -> io::Result<()> {
    let endpoint = resident_addr();
    let mut client = ResidentClient::connect(&endpoint)?;
    let ping = client.ping()?;

    for _ in 0..100.min(queries) {
        let _ = client.find_first(name)?;
    }

    let start = Instant::now();
    let mut found = 0usize;
    let mut server_ns_total = 0u128;
    for _ in 0..queries {
        let response = client.find_first(black_box(name))?;
        if let Some(hit) = response {
            found += 1;
            server_ns_total += hit.server_ns as u128;
            black_box(hit);
        }
    }
    let elapsed = start.elapsed();
    let avg_rtt_ns = elapsed.as_nanos() as f64 / queries.max(1) as f64;
    let avg_server_ns = server_ns_total as f64 / queries.max(1) as f64;
    let qps = queries as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);

    println!("resident ep   : {endpoint}");
    println!("transport     : {}", ping.transport);
    println!("query         : {name}");
    println!("queries       : {queries}");
    println!("found         : {found}");
    println!("avg IPC RTT   : {:.1} ns", avg_rtt_ns);
    println!("avg server    : {:.1} ns", avg_server_ns);
    println!("IPC overhead  : {:.1} ns", (avg_rtt_ns - avg_server_ns).max(0.0));
    println!("queries/sec   : {:.0}", qps);
    println!("note          : one persistent connection; v1 uses binary framing and Windows Named Pipe on Windows");
    Ok(())
}

fn resident_batch_benchmark(name: &str, batch_size: usize, batches: usize) -> io::Result<()> {
    let endpoint = resident_addr();
    let mut client = ResidentClient::connect(&endpoint)?;
    let ping = client.ping()?;
    let names = vec![name; batch_size];
    let mut results = Vec::with_capacity(batch_size);

    for _ in 0..20.min(batches) {
        let _ = client.find_batch_reuse(&names, &mut results)?;
    }

    let start = Instant::now();
    let mut found = 0usize;
    let mut server_ns_total = 0u128;
    for _ in 0..batches {
        let server_ns = client.find_batch_reuse(black_box(&names), &mut results)?;
        server_ns_total += server_ns as u128;
        found += results.iter().filter(|item| item.is_some()).count();
        black_box(&results);
    }
    let elapsed = start.elapsed();
    let total_lookups = batch_size.saturating_mul(batches).max(1);
    let avg_batch_rtt_ns = elapsed.as_nanos() as f64 / batches.max(1) as f64;
    let avg_file_rtt_ns = elapsed.as_nanos() as f64 / total_lookups as f64;
    let avg_server_batch_ns = server_ns_total as f64 / batches.max(1) as f64;
    let avg_server_file_ns = server_ns_total as f64 / total_lookups as f64;
    let batches_per_sec = batches as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    let lookups_per_sec = total_lookups as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);

    println!("resident ep   : {endpoint}");
    println!("transport     : {} + batch", ping.transport);
    println!("query         : {name}");
    println!("batch size    : {batch_size}");
    println!("batches       : {batches}");
    println!("lookups       : {total_lookups}");
    println!("found         : {found}");
    println!("avg batch RTT : {:.1} ns", avg_batch_rtt_ns);
    println!("avg/file RTT  : {:.1} ns", avg_file_rtt_ns);
    println!("server/batch  : {:.1} ns", avg_server_batch_ns);
    println!("server/file   : {:.1} ns", avg_server_file_ns);
    println!("IPC/file      : {:.1} ns", (avg_file_rtt_ns - avg_server_file_ns).max(0.0));
    println!("batches/sec   : {:.0}", batches_per_sec);
    println!("lookups/sec   : {:.0}", lookups_per_sec);
    println!("note          : one binary Named Pipe frame carries the whole batch; filenames are repeated only for benchmark control");
    Ok(())
}


#[derive(Debug, Clone, Copy)]
struct MixedBatchMetrics {
    batch_size: usize,
    batches: usize,
    total_lookups: usize,
    requested_misses: usize,
    found: usize,
    hot_hits: usize,
    avg_batch_rtt_ns: f64,
    avg_file_rtt_ns: f64,
    avg_server_batch_ns: f64,
    avg_server_file_ns: f64,
    ipc_file_ns: f64,
    lookups_per_sec: f64,
}

fn build_real_name_pool(index: &FileIndex, max_names: usize) -> Vec<String> {
    let target = max_names.max(1).min(index.entry_count().max(1));
    let mut seen = HashSet::<String>::with_capacity(target.saturating_mul(2));
    let mut names = Vec::with_capacity(target);
    if index.entry_count() == 0 {
        return names;
    }
    // Spread samples across the whole index instead of only taking the first directory.
    for i in 0..index.entry_count() {
        let id = i.wrapping_mul(2654435761usize) % index.entry_count();
        let name = index.entry_name(id as u32);
        let key = name.to_lowercase();
        if seen.insert(key) {
            names.push(name.to_owned());
            if names.len() >= target { break; }
        }
    }
    // The multiplicative walk is not guaranteed to cover all IDs when count is composite.
    if names.len() < target {
        for id in 0..index.entry_count() {
            let name = index.entry_name(id as u32);
            let key = name.to_lowercase();
            if seen.insert(key) {
                names.push(name.to_owned());
                if names.len() >= target { break; }
            }
        }
    }
    names
}

fn fill_mixed_batch<'a>(
    hit_pool: &'a [String],
    miss_pool: &'a [String],
    batch_size: usize,
    miss_percent: u32,
    batch_no: usize,
    names: &mut Vec<&'a str>,
) {
    names.clear();
    for slot in 0..batch_size {
        let selector = ((slot.wrapping_mul(37) + batch_no.wrapping_mul(17)) % 100) as u32;
        if selector < miss_percent {
            let idx = (slot.wrapping_mul(131) + batch_no.wrapping_mul(29)) % miss_pool.len();
            names.push(miss_pool[idx].as_str());
        } else {
            let idx = (slot.wrapping_mul(67) + batch_no.wrapping_mul(31)) % hit_pool.len();
            names.push(hit_pool[idx].as_str());
        }
    }
}

fn mixed_batch_measure(
    client: &mut ResidentClient,
    hit_pool: &[String],
    miss_pool: &[String],
    batch_size: usize,
    batches: usize,
    miss_percent: u32,
) -> io::Result<MixedBatchMetrics> {
    if hit_pool.is_empty() && miss_percent < 100 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "index contains no filenames for mixed benchmark"));
    }
    let mut names = Vec::<&str>::with_capacity(batch_size);
    let mut results = Vec::with_capacity(batch_size);

    for warm in 0..20.min(batches) {
        fill_mixed_batch(hit_pool, miss_pool, batch_size, miss_percent, warm, &mut names);
        let _ = client.find_batch_reuse(&names, &mut results)?;
    }

    let mut found = 0usize;
    let mut hot_hits = 0usize;
    let mut requested_misses = 0usize;
    let mut server_ns_total = 0u128;
    let mut rtt_ns_total = 0u128;
    for batch_no in 0..batches {
        fill_mixed_batch(hit_pool, miss_pool, batch_size, miss_percent, batch_no, &mut names);
        requested_misses += names.iter().filter(|name| name.starts_with("__skb_missing_")).count();
        let call_start = Instant::now();
        let server_ns = client.find_batch_reuse(black_box(&names), &mut results)?;
        rtt_ns_total += call_start.elapsed().as_nanos();
        server_ns_total += server_ns as u128;
        for item in &results {
            if let Some(hit) = item {
                found += 1;
                hot_hits += usize::from(hit.hot_cache_hit);
            }
        }
        black_box(&results);
    }
    let total_lookups = batch_size.saturating_mul(batches).max(1);
    let avg_batch_rtt_ns = rtt_ns_total as f64 / batches.max(1) as f64;
    let avg_file_rtt_ns = rtt_ns_total as f64 / total_lookups as f64;
    let avg_server_batch_ns = server_ns_total as f64 / batches.max(1) as f64;
    let avg_server_file_ns = server_ns_total as f64 / total_lookups as f64;
    Ok(MixedBatchMetrics {
        batch_size,
        batches,
        total_lookups,
        requested_misses,
        found,
        hot_hits,
        avg_batch_rtt_ns,
        avg_file_rtt_ns,
        avg_server_batch_ns,
        avg_server_file_ns,
        ipc_file_ns: (avg_file_rtt_ns - avg_server_file_ns).max(0.0),
        lookups_per_sec: total_lookups as f64 * 1_000_000_000.0 / (rtt_ns_total.max(1) as f64),
    })
}

fn resident_mixed_batch_benchmark(
    paths: &SkbPaths,
    batch_size: usize,
    batches: usize,
    miss_percent: u32,
) -> io::Result<()> {
    let index = FileIndex::load(&paths.index)?;
    let pool_target = index.entry_count().min(4096).max(1);
    let hit_pool = build_real_name_pool(&index, pool_target);
    let miss_pool: Vec<String> = (0..4096usize.min(batch_size.max(128)))
        .map(|i| format!("__skb_missing_{i:08}.not-a-real-file"))
        .collect();
    let endpoint = resident_addr();
    let mut client = ResidentClient::connect(&endpoint)?;
    let ping = client.ping()?;
    let m = mixed_batch_measure(&mut client, &hit_pool, &miss_pool, batch_size, batches, miss_percent)?;

    println!("resident ep   : {endpoint}");
    println!("transport     : {} + mixed-batch", ping.transport);
    println!("batch size    : {}", m.batch_size);
    println!("batches       : {}", m.batches);
    println!("lookups       : {}", m.total_lookups);
    println!("unique hits   : {} real filenames in pool", hit_pool.len());
    println!("miss target   : {}%", miss_percent);
    println!("miss requests : {}", m.requested_misses);
    println!("found         : {}", m.found);
    println!("hot hits      : {}", m.hot_hits);
    println!("avg batch RTT : {:.1} ns", m.avg_batch_rtt_ns);
    println!("avg/file RTT  : {:.1} ns", m.avg_file_rtt_ns);
    println!("server/batch  : {:.1} ns", m.avg_server_batch_ns);
    println!("server/file   : {:.1} ns", m.avg_server_file_ns);
    println!("IPC/file      : {:.1} ns", m.ipc_file_ns);
    println!("lookups/sec   : {:.0}", m.lookups_per_sec);
    println!("note          : mixed mode rotates across real indexed filenames; unlike resident-batch-bench it does not repeat one hot name");
    Ok(())
}

fn resident_batch_sweep(paths: &SkbPaths, batches: usize, miss_percent: u32) -> io::Result<()> {
    let index = FileIndex::load(&paths.index)?;
    let hit_pool = build_real_name_pool(&index, index.entry_count().min(4096).max(1));
    let miss_pool: Vec<String> = (0..4096usize)
        .map(|i| format!("__skb_missing_{i:08}.not-a-real-file"))
        .collect();
    let endpoint = resident_addr();
    let mut client = ResidentClient::connect(&endpoint)?;
    let ping = client.ping()?;
    println!("SKB resident mixed batch sweep");
    println!("transport     : {}", ping.transport);
    println!("real pool     : {} unique filenames", hit_pool.len());
    println!("miss target   : {}%", miss_percent);
    println!("batches/size  : {batches}");
    println!();
    println!("{:>6} {:>12} {:>12} {:>12} {:>14} {:>9}", "batch", "RTT/file ns", "server ns", "IPC ns", "lookups/sec", "hot %");
    for size in [1usize, 10, 100, 1000, 4096] {
        let m = mixed_batch_measure(&mut client, &hit_pool, &miss_pool, size, batches, miss_percent)?;
        let hot_pct = if m.found == 0 { 0.0 } else { m.hot_hits as f64 * 100.0 / m.found as f64 };
        println!("{:>6} {:>12.1} {:>12.1} {:>12.1} {:>14.0} {:>8.1}%", size, m.avg_file_rtt_ns, m.avg_server_file_ns, m.ipc_file_ns, m.lookups_per_sec, hot_pct);
    }
    println!("note          : batch=1 is the realistic distinct-name baseline; larger batches amortize one IPC frame across multiple lookups");
    Ok(())
}

fn duplicate_report(paths: &SkbPaths, limit: usize) -> io::Result<()> {
    let index = FileIndex::load(&paths.index)?;
    let mut groups: HashMap<String, Vec<u32>> = HashMap::new();
    let mut display: HashMap<String, String> = HashMap::new();
    for file_id in 0..index.entry_count() {
        let name = index.entry_name(file_id as u32);
        let key = name.to_lowercase();
        display.entry(key.clone()).or_insert_with(|| name.to_owned());
        groups.entry(key).or_default().push(file_id as u32);
    }
    let mut dupes: Vec<(String, Vec<u32>)> = groups.into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .collect();
    dupes.sort_unstable_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
    let duplicate_files: usize = dupes.iter().map(|(_, ids)| ids.len()).sum();
    println!("files         : {}", index.entry_count());
    println!("duplicate names: {}", dupes.len());
    println!("files in dupes: {}", duplicate_files);
    for (rank, (key, ids)) in dupes.into_iter().take(limit).enumerate() {
        let name = display.get(&key).map(String::as_str).unwrap_or(key.as_str());
        println!("{:>2}. {:>4} paths  {}", rank + 1, ids.len(), name);
        for file_id in ids.iter().take(3) {
            println!("      id={:<8} {}", file_id, index.entry_path(*file_id));
        }
        if ids.len() > 3 { println!("      ... {} more", ids.len() - 3); }
    }
    println!("note          : v1.0 reports duplicates before adding path-specific ranking; filename-only usage weight cannot safely distinguish same-name paths yet");
    Ok(())
}

fn print_metric(label: &str, elapsed: Duration, queries: usize) {
    let avg_ns = elapsed.as_nanos() as f64 / queries.max(1) as f64;
    let qps = queries as f64 / elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    println!("{:<17}: {:>9.1} ns  {:>12.0} q/s", label, avg_ns, qps);
}

fn xorshift64(state: &mut u64) {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
}

fn bytes_per_file(bytes: usize, entries: usize) -> f64 {
    if entries == 0 { 0.0 } else { bytes as f64 / entries as f64 }
}

fn print_help() {
    println!(r#"Smart Kernel Brain (SKB) v1
Single-binary filename locator: installer + CLI + resident core + MCP.
File contents are never read by the locator core.

INSTALL / MCP:
  SKB.exe                                First run outside install path: per-user install
  skb install [--scan <root>]            Install/refresh this single binary
  skb repair                             Repair PATH/config using this binary
  skb status                             Show install + resident status
  skb mcp                                Run MCP stdio server from this same EXE
  skb mcp-config                         Print MCP JSON using `SKB.exe mcp`
  skb uninstall [--purge-data]           Remove program; preserve data by default

USAGE:
  skb scan <root>                         Build/rebuild compact v2 index
  skb find <filename> [limit]             Local compatibility lookup; loads index per process
  skb find-id <filename> [limit]          Local lazy lookup
  skb path <file_id>                      Local path resolve

  skb daemon                              Run Resident Core in foreground
  skb daemon-start                        Start Resident Core as a background child
  skb daemon-status                       Check Resident Core
  skb daemon-stop                         Stop Resident Core
  skb rfind-id <filename>                 Resident filename -> LeanRef over local IPC
  skb rfind-ids <name> [name ...]         Batch filename -> LeanRefs in one IPC request
  skb rpath <file_id>                     Resident file_id -> path over local IPC
  skb resident-stats                      Resident Core stats
  skb resident-bench <filename> [q]       Persistent-connection IPC benchmark
  skb resident-batch-bench <name> [n] [b] Repeated-hot batch IPC benchmark
  skb resident-mixed-batch-bench [n] [b] [miss%] Mixed real-filename batch benchmark
  skb resident-batch-sweep [batches] [miss%] Sweep 1/10/100/1000/4096 mixed batches
  skb duplicates [limit]                     Report duplicate filename groups

  skb hot [limit]                         Show highest-weight files
  skb stats                               Show compact index/cache statistics
  skb benchmark [queries]                 Real-index lookup benchmark
  skb synthetic <entries> [q]             Synthetic core benchmark
  skb --version

ENV:
  SKB_HOME                                Override index/state directory
  SKB_PIPE_NAME                           Override Windows named-pipe endpoint
  SKB_DAEMON_ADDR                         Compatibility override / non-Windows TCP endpoint

DESIGN:
  v1 single-binary shell keeps the SKBIDX2/version-2 compact index and frozen search core unchanged. On Windows the resident
  core uses a local Named Pipe with a compact length-prefixed binary protocol.
  The hot path avoids TCP, JSON parsing, JSON allocation and newline framing.
  Non-Windows builds retain the v0.5 TCP/JSON path as a compatibility fallback.

EXAMPLES:
  skb scan D:\Projects
  skb daemon-start
  skb daemon-status
  skb rfind-id README.md
  skb rfind-ids README.md Cargo.toml skb.rs
  skb resident-bench README.md 10000
  skb resident-batch-bench README.md 100 1000
  skb resident-mixed-batch-bench 100 1000 10
  skb resident-batch-sweep 500 10
  skb duplicates 20
  skb daemon-stop
"#);
}
