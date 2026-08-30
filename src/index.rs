use crate::hash::{filename_eq, filename_hash};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAGIC: &[u8; 8] = b"SKBIDX2\0";
const VERSION: u32 = 2;
const MAX_POOL_BYTES: usize = u32::MAX as usize;
const MAX_INDEX_ENTRIES: usize = u32::MAX as usize;
const PREFIX_BITS_SMALL: u8 = 8;
const PREFIX_BITS_MEDIUM: u8 = 12;
const PREFIX_BITS_LARGE: u8 = 16;
const PREFIX_MEDIUM_THRESHOLD: usize = 16_384;
const PREFIX_LARGE_THRESHOLD: usize = 262_144;

/// v0.3 compact file metadata. Names and directory paths live in shared byte pools.
#[derive(Debug, Clone, Copy)]
struct CompactEntry {
    name_offset: u32,
    parent_id: u32,
}

#[derive(Debug, Clone)]
pub struct FileIndex {
    pub root: String,
    entries: Vec<CompactEntry>,
    name_pool: Vec<u8>,
    dir_pool: Vec<u8>,
    dir_offsets: Vec<u32>,
    // Structure-of-arrays lookup table sorted by hash. This costs 12 bytes/file
    // instead of a giant per-name HashMap and avoids tuple padding at steady state.
    lookup_hashes: Vec<u64>,
    lookup_ids: Vec<u32>,
    // Adaptive 8/12/16-bit hash-prefix directory. Each bucket stores a start
    // offset into the sorted lookup arrays, shrinking L1 binary search from the
    // whole index to hashes sharing the same high prefix bits.
    prefix_bits: u8,
    prefix_starts: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct ScanReport {
    pub files_indexed: usize,
    pub directories_seen: usize,
    pub skipped_entries: usize,
    pub elapsed: Duration,
}

impl FileIndex {
    pub fn empty(root: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            entries: Vec::new(),
            name_pool: Vec::new(),
            dir_pool: Vec::new(),
            dir_offsets: Vec::new(),
            lookup_hashes: Vec::new(),
            lookup_ids: Vec::new(),
            prefix_bits: PREFIX_BITS_SMALL,
            prefix_starts: vec![0; (1usize << PREFIX_BITS_SMALL) + 1],
        }
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn directory_count(&self) -> usize {
        self.dir_offsets.len()
    }

    pub fn entry_name(&self, file_id: u32) -> &str {
        let entry = self.entries[file_id as usize];
        pool_str(&self.name_pool, entry.name_offset)
    }

    pub fn entry_path(&self, file_id: u32) -> String {
        let entry = self.entries[file_id as usize];
        let parent_offset = self.dir_offsets[entry.parent_id as usize];
        let parent = pool_str(&self.dir_pool, parent_offset);
        PathBuf::from(parent)
            .join(self.entry_name(file_id))
            .to_string_lossy()
            .into_owned()
    }

    pub fn synthetic(count: usize) -> Self {
        assert!(count <= MAX_INDEX_ENTRIES, "synthetic index exceeds u32 file-id space");
        let mut index = Self::empty("synthetic://skb");
        index.entries.reserve(count);
        index.name_pool.reserve(count.saturating_mul(18));

        let dir_count = count.saturating_add(999) / 1000;
        index.dir_offsets.reserve(dir_count);
        for bucket in 0..dir_count {
            let dir = format!("S:\\synthetic\\bucket_{bucket:06}");
            let offset = push_pool_string(&mut index.dir_pool, &dir)
                .expect("synthetic directory pool exceeds 4 GiB");
            index.dir_offsets.push(offset);
        }

        let mut pairs = Vec::<(u64, u32)>::with_capacity(count);
        for i in 0..count {
            let name = format!("file_{i:08}.dat");
            let name_offset = push_pool_string(&mut index.name_pool, &name)
                .expect("synthetic name pool exceeds 4 GiB");
            let file_id = i as u32;
            index.entries.push(CompactEntry {
                name_offset,
                parent_id: (i / 1000) as u32,
            });
            pairs.push((filename_hash(&name), file_id));
        }
        index.install_sorted_lookup(pairs);
        index
    }

    /// Steady-state bytes owned directly by the compact arrays/pools.
    /// Allocator bookkeeping and the small adaptive L0/state maps are excluded.
    pub fn payload_bytes(&self) -> usize {
        self.entries.capacity() * std::mem::size_of::<CompactEntry>()
            + self.name_pool.capacity()
            + self.dir_pool.capacity()
            + self.dir_offsets.capacity() * std::mem::size_of::<u32>()
            + self.lookup_hashes.capacity() * std::mem::size_of::<u64>()
            + self.lookup_ids.capacity() * std::mem::size_of::<u32>()
            + self.prefix_starts.capacity() * std::mem::size_of::<u32>()
    }

    pub fn fixed_metadata_bytes_per_file() -> usize {
        std::mem::size_of::<CompactEntry>()
            + std::mem::size_of::<u64>()
            + std::mem::size_of::<u32>()
    }

    pub fn prefix_table_bytes(&self) -> usize {
        self.prefix_starts.capacity() * std::mem::size_of::<u32>()
    }

    pub fn prefix_bits(&self) -> u8 {
        self.prefix_bits
    }

    pub fn scan(root: &Path) -> io::Result<(Self, ScanReport)> {
        let start = Instant::now();
        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let root_string = canonical_root.to_string_lossy().into_owned();
        let mut index = Self::empty(root_string);
        let mut stack = vec![canonical_root];
        let mut dirs = 0usize;
        let mut skipped = 0usize;
        let mut dir_ids = HashMap::<String, u32>::new();
        let mut pairs = Vec::<(u64, u32)>::new();

        while let Some(dir) = stack.pop() {
            dirs += 1;
            let read_dir = match fs::read_dir(&dir) {
                Ok(v) => v,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            };

            for child in read_dir {
                let child = match child {
                    Ok(v) => v,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };
                let file_type = match child.file_type() {
                    Ok(v) => v,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };

                // Do not follow symlinks/reparse-point-like links during recursive scan.
                if file_type.is_symlink() {
                    skipped += 1;
                    continue;
                }

                if file_type.is_dir() {
                    stack.push(child.path());
                    continue;
                }

                if file_type.is_file() {
                    if index.entries.len() >= MAX_INDEX_ENTRIES {
                        return Err(io::Error::new(io::ErrorKind::InvalidData, "SKB v0.3 supports at most u32::MAX files per index"));
                    }
                    let path = child.path();
                    let name = child.file_name().to_string_lossy().into_owned();
                    let parent = path.parent().unwrap_or(Path::new("")).to_string_lossy().into_owned();
                    let parent_id = intern_directory(
                        &parent,
                        &mut dir_ids,
                        &mut index.dir_pool,
                        &mut index.dir_offsets,
                    )?;
                    let name_offset = push_pool_string(&mut index.name_pool, &name)?;
                    let file_id = index.entries.len() as u32;
                    index.entries.push(CompactEntry { name_offset, parent_id });
                    pairs.push((filename_hash(&name), file_id));
                }
            }
        }

        index.install_sorted_lookup(pairs);
        let report = ScanReport {
            files_indexed: index.entries.len(),
            directories_seen: dirs,
            skipped_entries: skipped,
            elapsed: start.elapsed(),
        };
        Ok((index, report))
    }

    /// Allocation-free filename -> first file-id lookup, including filename hashing.
    #[inline]
    pub fn lookup_first_id(&self, filename: &str) -> Option<u32> {
        self.lookup_first_id_with_hash(filename, filename_hash(filename))
    }

    /// Allocation-free filename -> first file-id lookup when the caller already
    /// computed the 64-bit filename hash. This is SKB v0.3.2's L1 raw fast path.
    #[inline]
    pub fn lookup_first_id_with_hash(&self, filename: &str, hash: u64) -> Option<u32> {
        let (start, end) = self.hash_range(hash);
        for &file_id in &self.lookup_ids[start..end] {
            if filename_eq(self.entry_name(file_id), filename) {
                return Some(file_id);
            }
        }
        None
    }

    pub fn exact_candidates(&self, filename: &str) -> Vec<u32> {
        self.exact_candidates_with_hash(filename, filename_hash(filename))
    }

    pub fn exact_candidates_with_hash(&self, filename: &str, hash: u64) -> Vec<u32> {
        let (start, end) = self.hash_range(hash);
        self.lookup_ids[start..end]
            .iter()
            .copied()
            .filter(|file_id| filename_eq(self.entry_name(*file_id), filename))
            .collect()
    }

    /// Visit exact-name candidates without allocating an intermediate Vec.
    /// Returns the number of IDs passed to the visitor.
    #[inline]
    pub fn visit_exact_candidates_with_hash<F>(
        &self,
        filename: &str,
        hash: u64,
        limit: usize,
        mut visitor: F,
    ) -> usize
    where
        F: FnMut(u32),
    {
        let (start, end) = self.hash_range(hash);
        let mut visited = 0usize;
        let limit = limit.max(1);
        for &file_id in &self.lookup_ids[start..end] {
            if filename_eq(self.entry_name(file_id), filename) {
                visitor(file_id);
                visited += 1;
                if visited >= limit {
                    break;
                }
            }
        }
        visited
    }

    #[inline]
    fn hash_range(&self, hash: u64) -> (usize, usize) {
        let prefix = hash_prefix(hash, self.prefix_bits);
        let bucket_start = self.prefix_starts[prefix] as usize;
        let bucket_end = self.prefix_starts[prefix + 1] as usize;
        let bucket_hashes = &self.lookup_hashes[bucket_start..bucket_end];

        let local_start = bucket_hashes.partition_point(|h| *h < hash);
        if local_start == bucket_hashes.len() || bucket_hashes[local_start] != hash {
            return (bucket_start + local_start, bucket_start + local_start);
        }
        let local_end = local_start + bucket_hashes[local_start..].partition_point(|h| *h <= hash);
        (bucket_start + local_start, bucket_start + local_end)
    }

    fn install_sorted_lookup(&mut self, mut pairs: Vec<(u64, u32)>) {
        pairs.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        self.lookup_hashes = Vec::with_capacity(pairs.len());
        self.lookup_ids = Vec::with_capacity(pairs.len());
        for (hash, file_id) in pairs {
            self.lookup_hashes.push(hash);
            self.lookup_ids.push(file_id);
        }
        self.rebuild_prefix_table();
    }

    fn rebuild_prefix_table(&mut self) {
        let len = self.lookup_hashes.len();
        assert!(len <= u32::MAX as usize, "lookup index exceeds u32 prefix offsets");
        self.prefix_bits = choose_prefix_bits(len);
        let table_len = (1usize << self.prefix_bits) + 1;
        self.prefix_starts = vec![0u32; table_len];

        let mut next_prefix = 0usize;
        for (position, hash) in self.lookup_hashes.iter().copied().enumerate() {
            let prefix = hash_prefix(hash, self.prefix_bits);
            while next_prefix <= prefix {
                self.prefix_starts[next_prefix] = position as u32;
                next_prefix += 1;
            }
        }
        while next_prefix < table_len {
            self.prefix_starts[next_prefix] = len as u32;
            next_prefix += 1;
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = File::create(path)?;
        let mut w = BufWriter::new(file);
        w.write_all(MAGIC)?;
        write_u32(&mut w, VERSION)?;
        write_string_u32(&mut w, &self.root)?;
        write_u64(&mut w, self.entries.len() as u64)?;
        write_u32(&mut w, self.dir_offsets.len() as u32)?;
        write_u64(&mut w, self.name_pool.len() as u64)?;
        write_u64(&mut w, self.dir_pool.len() as u64)?;

        w.write_all(&self.name_pool)?;
        w.write_all(&self.dir_pool)?;
        for offset in &self.dir_offsets {
            write_u32(&mut w, *offset)?;
        }
        for entry in &self.entries {
            write_u32(&mut w, entry.name_offset)?;
            write_u32(&mut w, entry.parent_id)?;
        }
        for hash in &self.lookup_hashes {
            write_u64(&mut w, *hash)?;
        }
        for file_id in &self.lookup_ids {
            write_u32(&mut w, *file_id)?;
        }
        w.flush()
    }

    pub fn load(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut r = BufReader::new(file);
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not an SKB v0.3 compact index; run `skb scan <root>` to rebuild it",
            ));
        }
        let version = read_u32(&mut r)?;
        if version != VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("unsupported index version {version}")));
        }

        let root = read_string_u32(&mut r)?;
        let count = usize_from_u64(read_u64(&mut r)?, "entry count")?;
        if count > MAX_INDEX_ENTRIES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "index entry count exceeds v0.3 limit"));
        }
        let dir_count = read_u32(&mut r)? as usize;
        let name_pool_len = usize_from_u64(read_u64(&mut r)?, "name pool length")?;
        let dir_pool_len = usize_from_u64(read_u64(&mut r)?, "directory pool length")?;
        if name_pool_len > MAX_POOL_BYTES || dir_pool_len > MAX_POOL_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "index string pool exceeds 4 GiB"));
        }

        let mut name_pool = vec![0u8; name_pool_len];
        let mut dir_pool = vec![0u8; dir_pool_len];
        r.read_exact(&mut name_pool)?;
        r.read_exact(&mut dir_pool)?;
        validate_pool(&name_pool)?;
        validate_pool(&dir_pool)?;

        let mut dir_offsets = Vec::with_capacity(dir_count);
        for _ in 0..dir_count {
            dir_offsets.push(read_u32(&mut r)?);
        }
        for offset in &dir_offsets {
            validate_offset(&dir_pool, *offset, "directory")?;
        }

        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let name_offset = read_u32(&mut r)?;
            let parent_id = read_u32(&mut r)?;
            validate_offset(&name_pool, name_offset, "filename")?;
            if parent_id as usize >= dir_count {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid parent directory id"));
            }
            entries.push(CompactEntry { name_offset, parent_id });
        }

        let mut lookup_hashes = Vec::with_capacity(count);
        for _ in 0..count {
            lookup_hashes.push(read_u64(&mut r)?);
        }
        if lookup_hashes.windows(2).any(|w| w[0] > w[1]) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "lookup hashes are not sorted"));
        }

        let mut lookup_ids = Vec::with_capacity(count);
        for _ in 0..count {
            let file_id = read_u32(&mut r)?;
            if file_id as usize >= count {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid lookup file id"));
            }
            lookup_ids.push(file_id);
        }

        let mut index = Self {
            root,
            entries,
            name_pool,
            dir_pool,
            dir_offsets,
            lookup_hashes,
            lookup_ids,
            prefix_bits: PREFIX_BITS_SMALL,
            prefix_starts: Vec::new(),
        };
        // v0.3.2 keeps the v0.3 on-disk format. The small prefix directory is
        // rebuilt on load so old compact indexes remain readable.
        index.rebuild_prefix_table();
        Ok(index)
    }
}


#[inline]
fn hash_prefix(hash: u64, bits: u8) -> usize {
    (hash >> (64 - bits as u32)) as usize
}

#[inline]
fn choose_prefix_bits(entry_count: usize) -> u8 {
    if entry_count >= PREFIX_LARGE_THRESHOLD {
        PREFIX_BITS_LARGE
    } else if entry_count >= PREFIX_MEDIUM_THRESHOLD {
        PREFIX_BITS_MEDIUM
    } else {
        PREFIX_BITS_SMALL
    }
}

fn intern_directory(
    value: &str,
    ids: &mut HashMap<String, u32>,
    pool: &mut Vec<u8>,
    offsets: &mut Vec<u32>,
) -> io::Result<u32> {
    if let Some(id) = ids.get(value) {
        return Ok(*id);
    }
    if offsets.len() >= u32::MAX as usize {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "too many directories"));
    }
    let id = offsets.len() as u32;
    let offset = push_pool_string(pool, value)?;
    offsets.push(offset);
    ids.insert(value.to_owned(), id);
    Ok(id)
}

fn push_pool_string(pool: &mut Vec<u8>, value: &str) -> io::Result<u32> {
    let start = pool.len();
    let needed = value.len().saturating_add(1);
    if start > MAX_POOL_BYTES || needed > MAX_POOL_BYTES.saturating_sub(start) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "SKB v0.3 string pool exceeds 4 GiB"));
    }
    let offset = start as u32;
    pool.extend_from_slice(value.as_bytes());
    pool.push(0);
    Ok(offset)
}

fn pool_str(pool: &[u8], offset: u32) -> &str {
    let start = offset as usize;
    let tail = &pool[start..];
    let len = tail.iter().position(|b| *b == 0).unwrap_or(tail.len());
    // Pools are validated on load and created from Rust UTF-8 strings at build time.
    std::str::from_utf8(&tail[..len]).expect("validated UTF-8 pool")
}

fn validate_pool(pool: &[u8]) -> io::Result<()> {
    if !pool.is_empty() && *pool.last().unwrap() != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "unterminated string pool"));
    }
    let mut start = 0usize;
    while start < pool.len() {
        let rel_end = pool[start..]
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unterminated string in pool"))?;
        std::str::from_utf8(&pool[start..start + rel_end])
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 in string pool"))?;
        start += rel_end + 1;
    }
    Ok(())
}

fn validate_offset(pool: &[u8], offset: u32, label: &str) -> io::Result<()> {
    let offset = offset as usize;
    if offset >= pool.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("invalid {label} pool offset")));
    }
    if offset > 0 && pool[offset - 1] != 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{label} offset is not at a string boundary")));
    }
    Ok(())
}

fn usize_from_u64(value: u64, label: &str) -> io::Result<usize> {
    usize::try_from(value)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{label} does not fit usize")))
}

fn write_u32<W: Write>(w: &mut W, value: u32) -> io::Result<()> {
    w.write_all(&value.to_le_bytes())
}
fn write_u64<W: Write>(w: &mut W, value: u64) -> io::Result<()> {
    w.write_all(&value.to_le_bytes())
}
fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}
fn read_u64<R: Read>(r: &mut R) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}
fn write_string_u32<W: Write>(w: &mut W, value: &str) -> io::Result<()> {
    let bytes = value.as_bytes();
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "string too large"))?;
    write_u32(w, len)?;
    w.write_all(bytes)
}
fn read_string_u32<R: Read>(r: &mut R) -> io::Result<String> {
    let len = read_u32(r)? as usize;
    if len > 16 * 1024 * 1024 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "root string too large"));
    }
    let mut bytes = vec![0u8; len];
    r.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid UTF-8 in root"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_index_finds_exact_name_case_insensitively() {
        let index = FileIndex::synthetic(1_000);
        let ids = index.exact_candidates("FILE_00000042.DAT");
        assert_eq!(ids.len(), 1);
        assert_eq!(index.entry_name(ids[0]), "file_00000042.dat");
    }

    #[test]
    fn compact_fixed_metadata_is_twenty_bytes_or_less() {
        assert!(FileIndex::fixed_metadata_bytes_per_file() <= 20);
    }

    #[test]
    fn synthetic_path_reconstructs_from_parent_and_name() {
        let index = FileIndex::synthetic(1_001);
        let path = index.entry_path(1_000);
        assert!(path.ends_with("bucket_000001\\file_00001000.dat") || path.ends_with("bucket_000001/file_00001000.dat"));
    }

    #[test]
    fn synthetic_payload_stays_under_sixty_four_bytes_per_file() {
        let index = FileIndex::synthetic(100_000);
        let bytes_per_file = index.payload_bytes() as f64 / index.entry_count() as f64;
        assert!(bytes_per_file < 64.0, "compact payload was {bytes_per_file:.1} B/file");
    }

    #[test]
    fn prefix_table_bounds_exact_lookup_bucket() {
        let index = FileIndex::synthetic(100_000);
        assert_eq!(index.prefix_starts.len(), (1usize << index.prefix_bits) + 1);
        assert_eq!(index.prefix_starts[0], 0);
        assert_eq!(*index.prefix_starts.last().unwrap() as usize, index.entry_count());
        assert!(index.prefix_starts.windows(2).all(|w| w[0] <= w[1]));
        let ids = index.exact_candidates("FILE_00054321.DAT");
        assert_eq!(ids, vec![54_321]);
    }

    #[test]
    fn million_entry_payload_stays_under_forty_bytes_per_file() {
        let index = FileIndex::synthetic(1_000_000);
        assert_eq!(index.prefix_bits(), 16);
        let bytes_per_file = index.payload_bytes() as f64 / index.entry_count() as f64;
        assert!(bytes_per_file < 40.0, "v0.3.2 payload was {bytes_per_file:.1} B/file");
    }

    #[test]
    fn hundred_thousand_entry_payload_stays_under_forty_bytes_per_file() {
        let index = FileIndex::synthetic(100_000);
        assert_eq!(index.prefix_bits(), 12);
        let bytes_per_file = index.payload_bytes() as f64 / index.entry_count() as f64;
        assert!(bytes_per_file < 40.0, "v0.3.2 payload was {bytes_per_file:.1} B/file");
    }

    #[test]
    fn raw_first_id_lookup_is_case_insensitive() {
        let index = FileIndex::synthetic(10_000);
        let name = "FILE_00004321.DAT";
        let hash = filename_hash(name);
        assert_eq!(index.lookup_first_id(name), Some(4_321));
        assert_eq!(index.lookup_first_id_with_hash(name, hash), Some(4_321));
        assert_eq!(index.lookup_first_id("missing.dat"), None);
    }

    #[test]
    fn compact_index_round_trips() {
        let index = FileIndex::synthetic(2_000);
        let path = std::env::temp_dir().join(format!("skb-v03-roundtrip-{}.skb", std::process::id()));
        index.save(&path).unwrap();
        let loaded = FileIndex::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(loaded.entry_count(), 2_000);
        let ids = loaded.exact_candidates("FILE_00001000.DAT");
        assert_eq!(ids, vec![1_000]);
    }
}
