use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::SystemTime;
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanConfig {
    pub root_path: PathBuf,
    pub min_size: u64,
    pub max_size: Option<u64>,
    pub save_state: bool,
    pub exclude_patterns: Vec<String>,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

fn default_batch_size() -> usize {
    1000
}

/// Get default exclusion patterns for common directories that should be skipped
pub fn get_default_exclusions() -> Vec<String> {
    vec![
        // Version control
        "*/.git/*".to_string(),
        "*/.svn/*".to_string(),
        "*/.hg/*".to_string(),

        // Package managers and dependencies
        "*/node_modules/*".to_string(),
        "*/bower_components/*".to_string(),
        "*/.npm/*".to_string(),
        "*/.yarn/*".to_string(),

        // Build artifacts
        "*/target/*".to_string(),      // Rust
        "*/dist/*".to_string(),         // General
        "*/build/*".to_string(),        // General
        "*/.next/*".to_string(),        // Next.js
        "*/.nuxt/*".to_string(),        // Nuxt.js

        // IDE and editor files
        "*/.vscode/*".to_string(),
        "*/.idea/*".to_string(),

        // OS files
        "*/.DS_Store".to_string(),
        "*/Thumbs.db".to_string(),
        "*/desktop.ini".to_string(),

        // Cache directories
        "*/.cache/*".to_string(),
        "*/__pycache__/*".to_string(),
        "*/.pytest_cache/*".to_string(),
    ]
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
}

impl Scanner {
    pub fn new(config: ScanConfig) -> Self {
        Scanner {
            config,
        }
    }

    /// Scan in streaming mode - returns a channel that yields batches of files
    pub fn scan_streaming(&self) -> (Receiver<Vec<FileInfo>>, Receiver<ScanProgress>) {
        let (batch_tx, batch_rx) = mpsc::channel();
        let (progress_tx, progress_rx) = mpsc::channel();

        let config = self.config.clone();

        thread::spawn(move || {
            let mut scanned_count = 0;
            let mut total_size = 0;
            let mut batch = Vec::with_capacity(config.batch_size);

            let walker = WalkDir::new(&config.root_path)
                .follow_links(false)
                .into_iter();

            for entry in walker.filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }

                let path = entry.path();

                // Check if path should be excluded
                if should_exclude(path, &config.exclude_patterns) {
                    continue;
                }

                if let Ok(file_info) = FileInfo::from_path(path) {
                    // Filter by size
                    if file_info.size < config.min_size {
                        continue;
                    }

                    if let Some(max_size) = config.max_size {
                        if file_info.size > max_size {
                            continue;
                        }
                    }

                    // Don't compute hashes yet - defer until we know there are duplicates
                    scanned_count += 1;
                    total_size += file_info.size;

                    batch.push(file_info);

                    // Send batch when it reaches the configured size
                    if batch.len() >= config.batch_size {
                        // Send progress update
                        let _ = progress_tx.send(ScanProgress {
                            scanned_count,
                            total_size,
                        });

                        // Send batch to processor (replace with new empty batch)
                        let full_batch = std::mem::replace(&mut batch, Vec::with_capacity(config.batch_size));
                        if batch_tx.send(full_batch).is_err() {
                            break; // Receiver dropped, stop scanning
                        }
                    }
                }
            }

            // Send any remaining files in the last batch
            if !batch.is_empty() {
                let _ = progress_tx.send(ScanProgress {
                    scanned_count,
                    total_size,
                });
                let _ = batch_tx.send(batch);
            }

            // Channels will be dropped here, signaling completion
        });

        (batch_rx, progress_rx)
    }
}

#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub scanned_count: usize,
    pub total_size: u64,
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

fn should_exclude(path: &Path, patterns: &[String]) -> bool {
    use glob::Pattern;

    let path_str = path.to_string_lossy();

    for pattern_str in patterns {
        if let Ok(pattern) = Pattern::new(pattern_str) {
            if pattern.matches(&path_str) {
                return true;
            }

            // Also check against just the filename
            if let Some(filename) = path.file_name() {
                if pattern.matches(&filename.to_string_lossy()) {
                    return true;
                }
            }
        }
    }

    false
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

    #[test]
    fn test_exclusion_patterns() {
        // Test basic filename pattern
        assert!(should_exclude(
            Path::new("/tmp/test.tmp"),
            &["*.tmp".to_string()]
        ));
        assert!(!should_exclude(
            Path::new("/tmp/test.txt"),
            &["*.tmp".to_string()]
        ));

        // Test path pattern
        assert!(should_exclude(
            Path::new("/tmp/.git/config"),
            &["*/.git/*".to_string()]
        ));

        // Test multiple patterns
        assert!(should_exclude(
            Path::new("/tmp/test.log"),
            &["*.tmp".to_string(), "*.log".to_string()]
        ));

        // Test directory pattern
        assert!(should_exclude(
            Path::new("/tmp/node_modules/package.json"),
            &["*/node_modules/*".to_string()]
        ));
    }

    #[test]
    fn test_scanner_with_exclusions() {
        use std::io::Write;

        let temp_dir = TempDir::new().unwrap();

        // Create a subdirectory to avoid hidden directory issues
        let test_dir = temp_dir.path().join("testdir");
        fs::create_dir(&test_dir).unwrap();

        // Create test files with content
        let mut f1 = File::create(test_dir.join("include.txt")).unwrap();
        f1.write_all(b"test content").unwrap();

        let mut f2 = File::create(test_dir.join("exclude.tmp")).unwrap();
        f2.write_all(b"excluded content").unwrap();

        let mut f3 = File::create(test_dir.join("also_include.dat")).unwrap();
        f3.write_all(b"more content").unwrap();

        let config = ScanConfig {
            root_path: test_dir,
            min_size: 1,
            max_size: None,
            save_state: false,
            exclude_patterns: vec!["*.tmp".to_string()],
            batch_size: 1000,
        };

        let mut scanner = Scanner::new(config);
        let _result = scanner.scan(|_, _| {}).unwrap();

        // Should have scanned 2 files (excluded the .tmp file)
        assert_eq!(scanner.scanned_count(), 2);
    }
}
