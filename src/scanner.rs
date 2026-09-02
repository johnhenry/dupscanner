use anyhow::{Context, Result};
use glob::Pattern;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::SystemTime;
use walkdir::{DirEntry, WalkDir};

/// Size of the prefix hashed for the "quick hash" tier.
pub const QUICK_HASH_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanConfig {
    pub root_path: PathBuf,
    pub min_size: u64,
    pub max_size: Option<u64>,
    pub exclude_patterns: Vec<String>,
    #[serde(default = "default_batch_size")]
    pub batch_size: usize,
}

fn default_batch_size() -> usize {
    1000
}

/// Default exclusion patterns for directories that almost never contain
/// duplicates a user wants to act on. Directory patterns are matched against
/// the directory *name* so the walker can prune whole subtrees.
pub fn get_default_exclusions() -> Vec<String> {
    [
        // Version control
        ".git", ".svn", ".hg",
        // Package managers and dependencies
        "node_modules", "bower_components", ".npm", ".yarn",
        // Build artifacts
        "target", "dist", "build", ".next", ".nuxt",
        // IDE and editor files
        ".vscode", ".idea",
        // Cache directories
        ".cache", "__pycache__", ".pytest_cache",
        // OS metadata files
        ".DS_Store", "Thumbs.db", "desktop.ini",
        // Data left behind by older dupscanner versions
        ".dupscanner",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Compiled exclusion patterns. A pattern matches a path if it matches the
/// full path string, the file/directory name, or (for `*/name/*` style
/// patterns from older configs) the bare `name` component.
#[derive(Debug, Clone)]
pub struct ExclusionMatcher {
    patterns: Vec<Pattern>,
}

impl ExclusionMatcher {
    pub fn new(patterns: &[String]) -> Self {
        let mut compiled = Vec::with_capacity(patterns.len());
        for raw in patterns {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(p) = Pattern::new(trimmed) {
                compiled.push(p);
            }
            // Accept legacy "*/name/*" and "*/name" forms as a bare name too.
            let mut stripped = trimmed;
            if let Some(s) = stripped.strip_prefix("*/") {
                stripped = s;
            }
            if let Some(s) = stripped.strip_suffix("/*") {
                stripped = s;
            }
            if stripped != trimmed && !stripped.is_empty() {
                if let Ok(p) = Pattern::new(stripped) {
                    compiled.push(p);
                }
            }
        }
        ExclusionMatcher { patterns: compiled }
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn matches(&self, path: &Path) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let path_str = path.to_string_lossy();
        let name = path.file_name().map(|n| n.to_string_lossy());
        self.patterns.iter().any(|p| {
            p.matches(&path_str)
                || name.as_deref().map(|n| p.matches(n)).unwrap_or(false)
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FileInfo {
    pub path: PathBuf,
    pub size: u64,
    /// SHA-256 of the first 64 KiB, computed lazily.
    pub quick_hash: Option<String>,
    /// SHA-256 of the whole file, computed lazily.
    pub hash: Option<String>,
    pub modified: SystemTime,
    pub depth: usize,
}

impl FileInfo {
    pub fn from_path(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        let size = metadata.len();
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
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
        // For files that fit inside the quick-hash window the two hashes are
        // identical, so reuse it instead of reading the file twice.
        if self.size <= QUICK_HASH_BYTES {
            if let Some(q) = &self.quick_hash {
                self.hash = Some(q.clone());
                return Ok(());
            }
        }
        let hash = compute_file_hash(&self.path)?;
        if self.size <= QUICK_HASH_BYTES {
            self.quick_hash = Some(hash.clone());
        }
        self.hash = Some(hash);
        Ok(())
    }

    pub fn get_or_compute_hash(&mut self) -> Result<&str> {
        if self.hash.is_none() {
            self.compute_hash()?;
        }
        Ok(self.hash.as_deref().unwrap_or_default())
    }

    pub fn compute_quick_hash(&mut self) -> Result<()> {
        let hash = compute_quick_hash(&self.path)?;
        if self.size <= QUICK_HASH_BYTES {
            self.hash = Some(hash.clone());
        }
        self.quick_hash = Some(hash);
        Ok(())
    }

    pub fn get_or_compute_quick_hash(&mut self) -> Result<&str> {
        if self.quick_hash.is_none() {
            self.compute_quick_hash()?;
        }
        Ok(self.quick_hash.as_deref().unwrap_or_default())
    }
}

pub struct Scanner {
    config: ScanConfig,
}

impl Scanner {
    pub fn new(config: ScanConfig) -> Self {
        Scanner { config }
    }

    pub fn config(&self) -> &ScanConfig {
        &self.config
    }

    /// Walk the tree on a background thread, sending batches of `FileInfo`
    /// and progress updates. The batch channel is bounded so a fast walker
    /// cannot run arbitrarily far ahead of a slow consumer.
    pub fn scan_streaming(&self) -> (Receiver<Vec<FileInfo>>, Receiver<ScanProgress>) {
        let (batch_tx, batch_rx) = mpsc::sync_channel(4);
        let (progress_tx, progress_rx) = mpsc::channel();

        let config = self.config.clone();
        let matcher = ExclusionMatcher::new(&config.exclude_patterns);

        thread::spawn(move || {
            let mut scanned_count = 0usize;
            let mut total_size = 0u64;
            let mut skipped = 0usize;
            let mut batch = Vec::with_capacity(config.batch_size);

            let root = config.root_path.clone();
            let walker = WalkDir::new(&root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry: &DirEntry| {
                    // Never prune the root itself, even if its name matches.
                    if entry.depth() == 0 {
                        return true;
                    }
                    !matcher.matches(entry.path())
                });

            for entry in walker {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };
                if !entry.file_type().is_file() {
                    continue;
                }

                let file_info = match FileInfo::from_path(entry.path()) {
                    Ok(f) => f,
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                };

                if file_info.size < config.min_size {
                    continue;
                }
                if let Some(max_size) = config.max_size {
                    if file_info.size > max_size {
                        continue;
                    }
                }

                scanned_count += 1;
                total_size += file_info.size;
                batch.push(file_info);

                if batch.len() >= config.batch_size {
                    let _ = progress_tx.send(ScanProgress {
                        scanned_count,
                        total_size,
                        skipped,
                    });
                    let full = std::mem::replace(&mut batch, Vec::with_capacity(config.batch_size));
                    if batch_tx.send(full).is_err() {
                        return; // Receiver dropped, stop scanning
                    }
                }
            }

            let _ = progress_tx.send(ScanProgress {
                scanned_count,
                total_size,
                skipped,
            });
            if !batch.is_empty() {
                let _ = batch_tx.send(batch);
            }
            // Channels drop here, signalling completion.
        });

        (batch_rx, progress_rx)
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ScanProgress {
    pub scanned_count: usize,
    pub total_size: u64,
    /// Entries that could not be read (permissions, vanished files, ...).
    pub skipped: usize,
}

pub fn compute_file_hash(path: &Path) -> Result<String> {
    let file = File::open(path).context("Failed to open file for hashing")?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// SHA-256 of the first `QUICK_HASH_BYTES` of the file.
pub fn compute_quick_hash(path: &Path) -> Result<String> {
    let file = File::open(path).context("Failed to open file for quick hashing")?;
    let mut reader = BufReader::new(file).take(QUICK_HASH_BYTES);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 16 * 1024];

    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }

    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write(path: &Path, bytes: &[u8]) {
        let mut f = File::create(path).unwrap();
        f.write_all(bytes).unwrap();
    }

    #[test]
    fn file_info_creation() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        write(&file_path, b"test content");

        let file_info = FileInfo::from_path(&file_path).unwrap();
        assert_eq!(file_info.size, 12);
        assert!(file_info.hash.is_none());
    }

    #[test]
    fn hash_is_stable() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        write(&file_path, b"test content");

        let mut file_info = FileInfo::from_path(&file_path).unwrap();
        file_info.compute_hash().unwrap();
        let first = file_info.hash.clone();
        file_info.hash = None;
        file_info.compute_hash().unwrap();
        assert_eq!(first, file_info.hash);
    }

    #[test]
    fn quick_hash_equals_full_hash_for_small_files_only() {
        let temp_dir = TempDir::new().unwrap();

        let small = temp_dir.path().join("small.txt");
        write(&small, b"small content");
        let mut small_info = FileInfo::from_path(&small).unwrap();
        small_info.compute_quick_hash().unwrap();
        // Small file: quick hash doubles as the full hash without a re-read.
        assert_eq!(small_info.quick_hash, small_info.hash);
        assert_eq!(small_info.hash.as_deref(), Some(compute_file_hash(&small).unwrap().as_str()));

        let large = temp_dir.path().join("large.bin");
        write(&large, &vec![7u8; 128 * 1024]);
        let mut large_info = FileInfo::from_path(&large).unwrap();
        large_info.compute_quick_hash().unwrap();
        assert!(large_info.hash.is_none());
        large_info.compute_hash().unwrap();
        assert_ne!(large_info.quick_hash, large_info.hash);
    }

    #[test]
    fn quick_hash_ignores_bytes_after_window() {
        let temp_dir = TempDir::new().unwrap();
        let a = temp_dir.path().join("a.bin");
        let b = temp_dir.path().join("b.bin");
        let mut data = vec![1u8; 100 * 1024];
        write(&a, &data);
        data[90 * 1024] = 2;
        write(&b, &data);
        assert_eq!(compute_quick_hash(&a).unwrap(), compute_quick_hash(&b).unwrap());
        assert_ne!(compute_file_hash(&a).unwrap(), compute_file_hash(&b).unwrap());
    }

    #[test]
    fn exclusion_matcher() {
        let m = ExclusionMatcher::new(&["*.tmp".to_string(), "node_modules".to_string(), "*/.git/*".to_string()]);
        assert!(m.matches(Path::new("/tmp/test.tmp")));
        assert!(!m.matches(Path::new("/tmp/test.txt")));
        assert!(m.matches(Path::new("/proj/node_modules")));
        assert!(m.matches(Path::new("/proj/.git")));
        assert!(m.matches(Path::new("/proj/.git/config")));
        assert!(!m.matches(Path::new("/proj/src/main.rs")));
    }

    #[test]
    fn scanner_prunes_excluded_directories_and_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path().join("root");
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        write(&root.join("src/keep.txt"), b"keep me");
        write(&root.join("src/skip.tmp"), b"skip me");
        write(&root.join("node_modules/pkg/index.js"), b"skip me too");

        let config = ScanConfig {
            root_path: root,
            min_size: 1,
            max_size: None,
            exclude_patterns: vec!["*.tmp".to_string(), "node_modules".to_string()],
            batch_size: 1000,
        };

        let scanner = Scanner::new(config);
        let (files_rx, progress_rx) = scanner.scan_streaming();
        let files: Vec<FileInfo> = files_rx.iter().flatten().collect();
        let last = progress_rx.iter().last().unwrap();

        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("keep.txt"));
        assert_eq!(last.scanned_count, 1);
    }
}
