use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct LargestFile {
    size: u64,
    path: PathBuf,
}

#[derive(Debug)]
struct ScanReport {
    largest: Vec<LargestFile>,
    files_seen: u64,
    directories_seen: u64,
    skipped_entries: u64,
    bytes_seen: u64,
    elapsed: Duration,
}

#[derive(Debug, Serialize)]
struct JsonLargestFile {
    rank: usize,
    size_bytes: u64,
    size_human: String,
    path: String,
}

#[derive(Debug, Serialize)]
struct JsonScanReport {
    root: String,
    limit: usize,
    files_seen: u64,
    directories_seen: u64,
    skipped_entries: u64,
    bytes_seen: u64,
    elapsed_seconds: f64,
    largest: Vec<JsonLargestFile>,
}

pub fn run(args: &[String]) -> io::Result<()> {
    let Some(root_raw) = args.first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: skb largest <root> [limit] [--json <file>]",
        ));
    };

    let mut limit = 100usize;
    let mut json_path: Option<&str> = None;
    let mut cursor = 1usize;

    if let Some(raw) = args.get(cursor) {
        if raw != "--json" {
            limit = raw.parse::<usize>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "limit must be a positive integer",
                )
            })?;
            cursor += 1;
        }
    }

    while cursor < args.len() {
        match args[cursor].as_str() {
            "--json" => {
                let Some(path) = args.get(cursor + 1) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--json requires an output file path",
                    ));
                };
                if json_path.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "--json may only be specified once",
                    ));
                }
                json_path = Some(path.as_str());
                cursor += 2;
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown largest option: {other}"),
                ));
            }
        }
    }

    if limit == 0 || limit > 10_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "limit must be between 1 and 10000",
        ));
    }

    let root = Path::new(root_raw);
    eprintln!("Scanning {} ...", root.display());
    let report = scan_largest(root, limit)?;

    println!("SKB largest files: {}", root.display());
    for (rank, file) in report.largest.iter().enumerate() {
        println!(
            "{:>4}. {:>10}  {}",
            rank + 1,
            human_size(file.size),
            file.path.display()
        );
    }
    println!();
    println!("files         : {}", report.files_seen);
    println!("directories   : {}", report.directories_seen);
    println!("total bytes   : {}", report.bytes_seen);
    println!("skipped       : {}", report.skipped_entries);
    println!("elapsed       : {:.3}s", report.elapsed.as_secs_f64());

    if let Some(output_path) = json_path {
        write_json_report(root, limit, &report, Path::new(output_path))?;
        println!("json          : {}", Path::new(output_path).display());
    }

    Ok(())
}

fn scan_largest(root: &Path, limit: usize) -> io::Result<ScanReport> {
    let root_meta = fs::metadata(root)?;
    if !root_meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a directory: {}", root.display()),
        ));
    }

    let started = Instant::now();
    let mut last_progress = Instant::now();
    let mut stack = vec![root.to_path_buf()];
    let mut heap: BinaryHeap<Reverse<(u64, PathBuf)>> = BinaryHeap::with_capacity(limit + 1);
    let mut files_seen = 0u64;
    let mut directories_seen = 0u64;
    let mut skipped_entries = 0u64;
    let mut bytes_seen = 0u64;

    while let Some(dir) = stack.pop() {
        directories_seen = directories_seen.saturating_add(1);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => {
                skipped_entries = skipped_entries.saturating_add(1);
                maybe_print_progress(
                    root,
                    files_seen,
                    directories_seen,
                    bytes_seen,
                    skipped_entries,
                    started,
                    &mut last_progress,
                );
                continue;
            }
        };

        for entry_result in entries {
            let entry = match entry_result {
                Ok(entry) => entry,
                Err(_) => {
                    skipped_entries = skipped_entries.saturating_add(1);
                    continue;
                }
            };

            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => {
                    skipped_entries = skipped_entries.saturating_add(1);
                    continue;
                }
            };

            // Do not follow symlinks/junction-like entries. This avoids recursive loops
            // and keeps the command a metadata-only local filesystem walk.
            if file_type.is_symlink() {
                skipped_entries = skipped_entries.saturating_add(1);
                continue;
            }

            if file_type.is_dir() {
                stack.push(entry.path());
                maybe_print_progress(
                    root,
                    files_seen,
                    directories_seen,
                    bytes_seen,
                    skipped_entries,
                    started,
                    &mut last_progress,
                );
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => {
                    skipped_entries = skipped_entries.saturating_add(1);
                    continue;
                }
            };
            let size = metadata.len();
            files_seen = files_seen.saturating_add(1);
            bytes_seen = bytes_seen.saturating_add(size);

            if heap.len() < limit {
                heap.push(Reverse((size, entry.path())));
            } else {
                let should_replace = heap
                    .peek()
                    .map(|Reverse((smallest_size, _))| size > *smallest_size)
                    .unwrap_or(true);
                if should_replace {
                    heap.pop();
                    heap.push(Reverse((size, entry.path())));
                }
            }

            maybe_print_progress(
                root,
                files_seen,
                directories_seen,
                bytes_seen,
                skipped_entries,
                started,
                &mut last_progress,
            );
        }
    }

    let elapsed = started.elapsed();
    eprintln!(
        "Scan complete: {} files, {} dirs, {}, skipped {}, {:.1}s",
        files_seen,
        directories_seen,
        human_size(bytes_seen),
        skipped_entries,
        elapsed.as_secs_f64()
    );

    let mut largest: Vec<LargestFile> = heap
        .into_iter()
        .map(|Reverse((size, path))| LargestFile { size, path })
        .collect();
    largest.sort_unstable_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.path.as_os_str().cmp(b.path.as_os_str()))
    });

    Ok(ScanReport {
        largest,
        files_seen,
        directories_seen,
        skipped_entries,
        bytes_seen,
        elapsed,
    })
}

#[allow(clippy::too_many_arguments)]
fn maybe_print_progress(
    root: &Path,
    files_seen: u64,
    directories_seen: u64,
    bytes_seen: u64,
    skipped_entries: u64,
    started: Instant,
    last_progress: &mut Instant,
) {
    if last_progress.elapsed() < Duration::from_secs(1) {
        return;
    }

    eprintln!(
        "Scanning {} | files {} | dirs {} | {} | skipped {} | {:.1}s",
        root.display(),
        files_seen,
        directories_seen,
        human_size(bytes_seen),
        skipped_entries,
        started.elapsed().as_secs_f64()
    );
    let _ = io::stderr().flush();
    *last_progress = Instant::now();
}

fn write_json_report(root: &Path, limit: usize, report: &ScanReport, output: &Path) -> io::Result<()> {
    let largest = report
        .largest
        .iter()
        .enumerate()
        .map(|(rank, file)| JsonLargestFile {
            rank: rank + 1,
            size_bytes: file.size,
            size_human: human_size(file.size),
            path: file.path.to_string_lossy().into_owned(),
        })
        .collect();

    let json_report = JsonScanReport {
        root: root.to_string_lossy().into_owned(),
        limit,
        files_seen: report.files_seen,
        directories_seen: report.directories_seen,
        skipped_entries: report.skipped_entries,
        bytes_seen: report.bytes_seen,
        elapsed_seconds: report.elapsed.as_secs_f64(),
        largest,
    };

    let file = fs::File::create(output)?;
    let writer = io::BufWriter::new(file);
    serde_json::to_writer_pretty(writer, &json_report).map_err(|err| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("failed to write JSON report: {err}"),
        )
    })?;
    Ok(())
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("skb-largest-test-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn keeps_only_largest_files_in_descending_order() {
        let root = temp_root();
        fs::create_dir_all(root.join("nested")).unwrap();
        write_bytes(&root.join("small.bin"), 3);
        write_bytes(&root.join("nested/large.bin"), 11);
        write_bytes(&root.join("medium.bin"), 7);

        let report = scan_largest(&root, 2).unwrap();
        assert_eq!(report.files_seen, 3);
        assert_eq!(report.largest.len(), 2);
        assert_eq!(report.largest[0].size, 11);
        assert_eq!(report.largest[1].size, 7);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(1_000), "1.00 KB");
        assert_eq!(human_size(1_000_000), "1.00 MB");
    }

    #[test]
    fn writes_json_report() {
        let root = temp_root();
        fs::create_dir_all(&root).unwrap();
        write_bytes(&root.join("large.bin"), 32);
        write_bytes(&root.join("small.bin"), 4);

        let report = scan_largest(&root, 2).unwrap();
        let output = root.join("report.json");
        write_json_report(&root, 2, &report, &output).unwrap();

        let raw = fs::read_to_string(&output).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["limit"], 2);
        assert_eq!(value["largest"][0]["size_bytes"], 32);
        assert!(value["largest"][0]["path"]
            .as_str()
            .unwrap()
            .ends_with("large.bin"));

        fs::remove_dir_all(root).unwrap();
    }

    fn write_bytes(path: &Path, count: usize) {
        let mut file = File::create(path).unwrap();
        file.write_all(&vec![0u8; count]).unwrap();
    }
}
