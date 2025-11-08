use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::{DirEntry, WalkDir};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanConfig {
    pub root_path: PathBuf,
    pub min_size: u64,
    pub max_size: Option<u64>,
    pub save_state: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FileInfo {
    pub path: PathBuf,
    pub size: u64,
    pub quick_hash: Option<String>,  // Hash of first 64KB for fast comparison
    pub hash: Option<String>,         // Full file hash (computed lazily)
    pub modified: SystemTime,
    pub depth: usize,
}

impl FileInfo {
    pub fn from_path(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        let size = metadata.len();
        let modified = metadata.modified()?;

        let depth = path.components().count();

        Ok(FileInfo {
            path: path.to_path_buf(),
            size,
            quick_hash: None,
            hash: None,
            modified,
            depth,
        })
    }

    pub fn compute_hash(&mut self) -> Result<()> {
        let hash = compute_file_hash(&self.path)?;
        self.hash = Some(hash);
        Ok(())
    }

    pub fn get_or_compute_hash(&mut self) -> Result<&str> {
        if self.hash.is_none() {
            self.compute_hash()?;
        }
        Ok(self.hash.as_ref().unwrap())
    }

    pub fn compute_quick_hash(&mut self) -> Result<()> {
        let hash = compute_quick_hash(&self.path)?;
        self.quick_hash = Some(hash);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_or_compute_quick_hash(&mut self) -> Result<&str> {
        if self.quick_hash.is_none() {
            self.compute_quick_hash()?;
        }
        Ok(self.quick_hash.as_ref().unwrap())
    }
}

pub struct Scanner {
    config: ScanConfig,
    size_groups: HashMap<u64, Vec<FileInfo>>,
    scanned_count: usize,
    total_size: u64,
}

impl Scanner {
    pub fn new(config: ScanConfig) -> Self {
        Scanner {
            config,
            size_groups: HashMap::new(),
            scanned_count: 0,
            total_size: 0,
        }
    }

    pub fn scan<F>(&mut self, mut progress_callback: F) -> Result<HashMap<u64, Vec<FileInfo>>>
    where
        F: FnMut(usize, &Path),
    {
        let walker = WalkDir::new(&self.config.root_path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !is_hidden(e));

        for entry in walker.filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();

            if let Ok(mut file_info) = FileInfo::from_path(path) {
                // Filter by size
                if file_info.size < self.config.min_size {
                    continue;
                }

                if let Some(max_size) = self.config.max_size {
                    if file_info.size > max_size {
                        continue;
                    }
                }

                // Compute quick hash during initial scan
                if file_info.compute_quick_hash().is_err() {
                    continue; // Skip files we can't hash
                }

                self.scanned_count += 1;
                self.total_size += file_info.size;

                // Group by size
                self.size_groups
                    .entry(file_info.size)
                    .or_insert_with(Vec::new)
                    .push(file_info);

                progress_callback(self.scanned_count, path);
            }
        }

        Ok(self.size_groups.clone())
    }

    #[allow(dead_code)]
    pub fn get_potential_duplicates(&self) -> Vec<&Vec<FileInfo>> {
        self.size_groups
            .values()
            .filter(|group| group.len() > 1)
            .collect()
    }

    #[allow(dead_code)]
    pub fn scanned_count(&self) -> usize {
        self.scanned_count
    }

    pub fn total_size(&self) -> u64 {
        self.total_size
    }
}

pub fn compute_file_hash(path: &Path) -> Result<String> {
    let file = File::open(path).context("Failed to open file for hashing")?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 8192];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Compute quick hash of first 64KB of file for fast comparison
pub fn compute_quick_hash(path: &Path) -> Result<String> {
    let file = File::open(path).context("Failed to open file for quick hashing")?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 65536]; // 64KB buffer

    // Read only first 64KB
    let bytes_read = reader.read(&mut buffer)?;
    hasher.update(&buffer[..bytes_read]);

    Ok(format!("{:x}", hasher.finalize()))
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_file_info_creation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"test content").unwrap();

        let file_info = FileInfo::from_path(&file_path).unwrap();
        assert_eq!(file_info.size, 12);
        assert!(file_info.hash.is_none());
    }

    #[test]
    fn test_hash_computation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"test content").unwrap();

        let mut file_info = FileInfo::from_path(&file_path).unwrap();
        file_info.compute_hash().unwrap();
        assert!(file_info.hash.is_some());

        // Hash should be consistent
        let hash1 = file_info.hash.clone();
        file_info.compute_hash().unwrap();
        assert_eq!(hash1, file_info.hash);
    }

    #[test]
    fn test_quick_hash_computation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let mut file = File::create(&file_path).unwrap();
        file.write_all(b"test content").unwrap();

        let mut file_info = FileInfo::from_path(&file_path).unwrap();
        assert!(file_info.quick_hash.is_none());

        file_info.compute_quick_hash().unwrap();
        assert!(file_info.quick_hash.is_some());

        // Quick hash should be consistent
        let quick_hash1 = file_info.quick_hash.clone();
        file_info.compute_quick_hash().unwrap();
        assert_eq!(quick_hash1, file_info.quick_hash);
    }

    #[test]
    fn test_quick_hash_vs_full_hash() {
        let temp_dir = TempDir::new().unwrap();

        // Create a small file (less than 64KB)
        let small_file = temp_dir.path().join("small.txt");
        let mut file = File::create(&small_file).unwrap();
        file.write_all(b"small content").unwrap();

        let mut small_info = FileInfo::from_path(&small_file).unwrap();
        small_info.compute_quick_hash().unwrap();
        small_info.compute_hash().unwrap();

        // For small files, quick hash and full hash should be the same
        assert_eq!(small_info.quick_hash, small_info.hash);

        // Create a large file (more than 64KB)
        let large_file = temp_dir.path().join("large.txt");
        let mut file = File::create(&large_file).unwrap();
        let large_content = vec![0u8; 128 * 1024]; // 128KB
        file.write_all(&large_content).unwrap();

        let mut large_info = FileInfo::from_path(&large_file).unwrap();
        large_info.compute_quick_hash().unwrap();
        large_info.compute_hash().unwrap();

        // For large files, quick hash and full hash should be different
        assert_ne!(large_info.quick_hash, large_info.hash);
    }
}
