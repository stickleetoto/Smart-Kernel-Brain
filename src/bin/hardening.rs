use skb::{FileIndex, SkbPaths};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const INDEX_MAGIC: &[u8; 8] = b"SKBIDX2\0";
const INDEX_VERSION: u32 = 2;
const MAX_ROOT_BYTES: u64 = 16 * 1024 * 1024;
const FIXED_HEADER_BYTES: u64 = 44;
const BYTES_PER_ENTRY_ON_DISK: u64 = 20;
const BYTES_PER_DIRECTORY_OFFSET: u64 = 4;

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn run_scan(args: &[String]) -> io::Result<()> {
    let root = args.first().map(String::as_str).unwrap_or(".");
    let paths = SkbPaths::discover()?;
    paths.ensure_home()?;

    println!("SKB compact scan: {root}");
    let (index, report) = FileIndex::scan(Path::new(root))?;
    let payload = index.payload_bytes();
    let per_file = bytes_per_file(payload, index.entry_count());

    save_index_atomic(&index, &paths.index)?;
    preflight_index(&paths.index)?;

    println!("indexed       : {} files", report.files_indexed);
    println!("directories   : {}", report.directories_seen);
    println!("skipped       : {}", report.skipped_entries);
    println!("elapsed       : {:.3}s", report.elapsed.as_secs_f64());
    println!("compact RAM   : {payload} bytes lowerbound");
    println!("RAM/file      : {per_file:.1} bytes lowerbound");
    println!("index         : {}", paths.index.display());
    Ok(())
}

pub fn preflight_default_index_for(command: &str) -> io::Result<()> {
    if !command_requires_index(command) {
        return Ok(());
    }
    let paths = SkbPaths::discover()?;
    preflight_index(&paths.index).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "index preflight failed for {}: {e}; run `skb scan <root>` to rebuild it",
                paths.index.display()
            ),
        )
    })
}

fn command_requires_index(command: &str) -> bool {
    matches!(
        command,
        "find"
            | "find-id"
            | "find-ref"
            | "path"
            | "resolve"
            | "hot"
            | "stats"
            | "daemon"
            | "daemon-start"
            | "duplicates"
            | "benchmark"
            | "resident-mixed-batch-bench"
            | "resident-batch-sweep"
            | "mcp"
    )
}

fn bytes_per_file(total: usize, count: usize) -> f64 {
    if count == 0 {
        0.0
    } else {
        total as f64 / count as f64
    }
}

fn save_index_atomic(index: &FileIndex, destination: &Path) -> io::Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }

    let temporary = temporary_sibling(destination);
    let result = (|| {
        index.save(&temporary)?;
        File::open(&temporary)?.sync_all()?;
        replace_file(&temporary, destination)?;
        sync_parent_best_effort(destination);
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn preflight_index(path: &Path) -> io::Result<()> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();

    let mut magic = [0u8; 8];
    file.read_exact(&mut magic)?;
    if &magic != INDEX_MAGIC {
        return Err(invalid_data("invalid SKB index magic"));
    }

    let version = read_u32(&mut file)?;
    if version != INDEX_VERSION {
        return Err(invalid_data(format!("unsupported SKB index version {version}")));
    }

    let root_len = read_u32(&mut file)? as u64;
    if root_len > MAX_ROOT_BYTES {
        return Err(invalid_data("root string exceeds 16 MiB"));
    }
    let root_skip = i64::try_from(root_len)
        .map_err(|_| invalid_data("root length cannot be represented by this process"))?;
    file.seek(SeekFrom::Current(root_skip))?;

    let entry_count = read_u64(&mut file)?;
    if entry_count > u32::MAX as u64 {
        return Err(invalid_data("index entry count exceeds u32 file-id space"));
    }
    let directory_count = read_u32(&mut file)? as u64;
    let name_pool_len = read_u64(&mut file)?;
    let directory_pool_len = read_u64(&mut file)?;
    if name_pool_len > u32::MAX as u64 || directory_pool_len > u32::MAX as u64 {
        return Err(invalid_data("index string pool exceeds 4 GiB"));
    }

    let expected_len = FIXED_HEADER_BYTES
        .checked_add(root_len)
        .and_then(|v| v.checked_add(name_pool_len))
        .and_then(|v| v.checked_add(directory_pool_len))
        .and_then(|v| {
            directory_count
                .checked_mul(BYTES_PER_DIRECTORY_OFFSET)
                .and_then(|n| v.checked_add(n))
        })
        .and_then(|v| {
            entry_count
                .checked_mul(BYTES_PER_ENTRY_ON_DISK)
                .and_then(|n| v.checked_add(n))
        })
        .ok_or_else(|| invalid_data("index length arithmetic overflow"))?;

    if expected_len != file_len {
        return Err(invalid_data(format!(
            "index length mismatch: header requires {expected_len} bytes, file has {file_len} bytes"
        )));
    }
    Ok(())
}

fn read_u32<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64<R: Read>(reader: &mut R) -> io::Result<u64> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn temporary_sibling(destination: &Path) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = destination
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("index.skb");
    destination.with_file_name(format!(
        ".{name}.tmp-{}-{stamp}-{sequence}",
        std::process::id()
    ))
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x0000_0001;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x0000_0008;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }

    let source_wide: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let ok = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

fn sync_parent_best_effort(destination: &Path) {
    #[cfg(unix)]
    if let Some(parent) = destination.parent() {
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
    }

    #[cfg(not(unix))]
    let _ = destination;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "skb-hardening-{label}-{}-{stamp}",
            std::process::id()
        ))
    }

    #[test]
    fn preflight_accepts_valid_index() {
        let root = temp_root("valid");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("index.skb");
        FileIndex::synthetic(128).save(&path).unwrap();
        preflight_index(&path).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preflight_rejects_truncated_index() {
        let root = temp_root("truncated");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("index.skb");
        FileIndex::synthetic(128).save(&path).unwrap();
        let len = fs::metadata(&path).unwrap().len();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(len - 1)
            .unwrap();
        let error = preflight_index(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn atomic_save_replaces_existing_index() {
        let root = temp_root("replace");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("index.skb");

        save_index_atomic(&FileIndex::synthetic(3), &path).unwrap();
        save_index_atomic(&FileIndex::synthetic(7), &path).unwrap();

        preflight_index(&path).unwrap();
        let loaded = FileIndex::load(&path).unwrap();
        assert_eq!(loaded.entry_count(), 7);
        fs::remove_dir_all(root).unwrap();
    }
}
