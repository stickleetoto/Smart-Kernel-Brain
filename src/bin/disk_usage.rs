use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs;
use std::io;
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

pub fn run(args: &[String]) -> io::Result<()> {
    let Some(root_raw) = args.first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: skb largest <root> [limit]",
        ));
    };

    let limit = match args.get(1) {
        Some(raw) => raw.parse::<usize>().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "limit must be a positive integer",
            )
        })?,
        None => 100,
    };
    if limit == 0 || limit > 10_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "limit must be between 1 and 10000",
        ));
    }

    let root = Path::new(root_raw);
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
                continue;
            }

            let should_replace = heap
                .peek()
                .map(|Reverse((smallest_size, _))| size > *smallest_size)
                .unwrap_or(true);
            if should_replace {
                heap.pop();
                heap.push(Reverse((size, entry.path())));
            }
        }
    }

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
        elapsed: started.elapsed(),
    })
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
    use std::io::Write;
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

    fn write_bytes(path: &Path, count: usize) {
        let mut file = File::create(path).unwrap();
        file.write_all(&vec![0u8; count]).unwrap();
    }
}
