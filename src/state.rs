use crate::hash::normalize_filename;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageRecord {
    pub access_count: u64,
    pub last_access_ms: u64,
}

/// Adaptive state is keyed by normalized filename, not by file contents or path.
/// This matches SKB's filename-only goal: learn which *names* the user repeatedly asks for.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageState {
    pub records: HashMap<String, UsageRecord>,
}

impl UsageState {
    pub fn load(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes = fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("invalid state file: {e}")))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(self)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))?;
        fs::write(&tmp, bytes)?;
        match fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(_) => {
                if path.exists() {
                    let _ = fs::remove_file(path);
                }
                fs::rename(tmp, path)
            }
        }
    }

    pub fn record_filename(&mut self, filename: &str) -> &UsageRecord {
        let key = normalize_filename(filename);
        let now = now_ms();
        let record = self.records.entry(key).or_default();
        record.access_count = record.access_count.saturating_add(1);
        record.last_access_ms = now;
        record
    }

    pub fn get_for_filename(&self, filename: &str) -> UsageRecord {
        self.records
            .get(&normalize_filename(filename))
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_normalized(&self, normalized_filename: &str) -> UsageRecord {
        self.records.get(normalized_filename).cloned().unwrap_or_default()
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
