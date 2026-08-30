/// Small, dependency-free 64-bit FNV-1a hash.
///
/// This is NOT a security hash. SKB uses it only to narrow candidates and then
/// verifies the real filename, so collisions cannot create false matches.
#[inline]
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET;
    for &byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

pub fn normalize_filename(name: &str) -> String {
    name.to_lowercase()
}

/// Case-insensitive filename hash.
///
/// v0.3.2 adds a zero-allocation ASCII fast path because source-code/project
/// filenames are overwhelmingly ASCII. Unicode keeps the previous streaming
/// lowercase behavior for correctness.
#[inline]
pub fn filename_hash(name: &str) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET;
    if name.is_ascii() {
        for &byte in name.as_bytes() {
            hash ^= byte.to_ascii_lowercase() as u64;
            hash = hash.wrapping_mul(PRIME);
        }
        return hash;
    }

    let mut utf8 = [0u8; 4];
    for ch in name.chars().flat_map(|c| c.to_lowercase()) {
        let encoded = ch.encode_utf8(&mut utf8);
        for &byte in encoded.as_bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

/// Exact verification after hash narrowing, without heap allocation.
/// ASCII uses the standard byte-oriented fast path; Unicode falls back to the
/// same lowercase-character semantics used by `filename_hash`.
#[inline]
pub fn filename_eq(a: &str, b: &str) -> bool {
    if a.is_ascii() && b.is_ascii() {
        return a.eq_ignore_ascii_case(b);
    }
    a.chars()
        .flat_map(|c| c.to_lowercase())
        .eq(b.chars().flat_map(|c| c.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_hash_is_case_insensitive() {
        assert_eq!(filename_hash("Player.cs"), filename_hash("player.CS"));
    }

    #[test]
    fn streaming_hash_matches_normalized_hash() {
        for value in ["README.md", "PlayerController.CS", "ÄBC.txt", "Straße.TXT"] {
            assert_eq!(filename_hash(value), fnv1a64(normalize_filename(value).as_bytes()));
        }
    }

    #[test]
    fn filename_eq_is_case_insensitive() {
        assert!(filename_eq("Player.CS", "player.cs"));
        assert!(filename_eq("README.MD", "readme.md"));
        assert!(!filename_eq("Player.cs", "Player2.cs"));
    }
}
