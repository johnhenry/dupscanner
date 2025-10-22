use crate::duplicates::{DuplicateFinder, DuplicateGroup};
use crate::scanner::{FileInfo, ScanConfig};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanState {
    pub config: ScanConfig,
    pub size_groups: HashMap<u64, Vec<FileInfo>>,
    pub duplicate_groups: Vec<DuplicateGroup>,
    pub scanned_count: usize,
    pub total_size: u64,
    pub timestamp: DateTime<Utc>,
    pub completed: bool,
}

impl ScanState {
    pub fn new(config: ScanConfig) -> Self {
        ScanState {
            config,
            size_groups: HashMap::new(),
            duplicate_groups: Vec::new(),
            scanned_count: 0,
            total_size: 0,
            timestamp: Utc::now(),
            completed: false,
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json).context("Failed to write state file")?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let json = fs::read_to_string(path).context("Failed to read state file")?;
        let state: ScanState = serde_json::from_str(&json)?;
        Ok(state)
    }

    pub fn update_from_finder(&mut self, finder: &DuplicateFinder) {
        self.duplicate_groups = finder.groups().to_vec();
        self.timestamp = Utc::now();
    }
}

pub fn get_state_dir() -> Result<PathBuf> {
    let state_dir = dirs::data_local_dir()
        .context("Failed to get local data directory")?
        .join("dupscanner");

    if !state_dir.exists() {
        fs::create_dir_all(&state_dir)?;
    }

    Ok(state_dir)
}

pub fn get_default_state_file(scan_path: &Path) -> Result<PathBuf> {
    let state_dir = get_state_dir()?;
    let path_hash = format!("{:x}", md5::compute(scan_path.to_string_lossy().as_bytes()));
    Ok(state_dir.join(format!("scan_{}.json", path_hash)))
}

pub fn get_latest_state_file() -> Result<PathBuf> {
    let state_dir = get_state_dir()?;
    let mut entries: Vec<_> = fs::read_dir(&state_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .collect();

    entries.sort_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok()));

    entries
        .last()
        .map(|e| e.path())
        .context("No state files found")
}

#[derive(Debug, Clone)]
pub struct StateInfo {
    pub path: PathBuf,
    pub timestamp: String,
    pub files_scanned: usize,
}

pub fn list_state_files() -> Result<Vec<StateInfo>> {
    let state_dir = get_state_dir()?;

    if !state_dir.exists() {
        return Ok(Vec::new());
    }

    let mut states = Vec::new();

    for entry in fs::read_dir(state_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.extension().map(|ext| ext == "json").unwrap_or(false) {
            if let Ok(state) = ScanState::load(&path) {
                states.push(StateInfo {
                    path: state.config.root_path,
                    timestamp: state.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
                    files_scanned: state.scanned_count,
                });
            }
        }
    }

    states.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

    Ok(states)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_state_save_and_load() {
        let temp_dir = TempDir::new().unwrap();
        let state_file = temp_dir.path().join("state.json");

        let config = ScanConfig {
            root_path: PathBuf::from("/test"),
            min_size: 1,
            max_size: None,
            save_state: true,
        };

        let state = ScanState::new(config);
        state.save(&state_file).unwrap();

        let loaded_state = ScanState::load(&state_file).unwrap();
        assert_eq!(loaded_state.scanned_count, 0);
        assert_eq!(loaded_state.config.root_path, PathBuf::from("/test"));
    }
}
