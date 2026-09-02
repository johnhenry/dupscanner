use crate::scanner::FileInfo;
use anyhow::Result;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub hash: String,
    pub files: Vec<FileInfo>,
    pub total_size: u64,
    pub wasted_space: u64,
}

impl DuplicateGroup {
    pub fn new(hash: String, mut files: Vec<FileInfo>) -> Self {
        // Deterministic order so the UI and the keeper choice are stable.
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let total_size = files.iter().map(|f| f.size).sum();
        let wasted_space = files
            .first()
            .map(|f| f.size * (files.len() as u64).saturating_sub(1))
            .unwrap_or(0);

        DuplicateGroup {
            hash,
            files,
            total_size,
            wasted_space,
        }
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    pub fn file_size(&self) -> u64 {
        self.files.first().map(|f| f.size).unwrap_or(0)
    }

    /// A group with fewer than two files is no longer a duplicate group.
    pub fn is_empty(&self) -> bool {
        self.files.len() <= 1
    }

    /// Drop the given paths from the group and recompute derived sizes.
    pub fn remove_paths(&mut self, paths: &HashSet<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.files.retain(|f| !paths.contains(&f.path));
        self.total_size = self.files.iter().map(|f| f.size).sum();
        self.wasted_space = self
            .files
            .first()
            .map(|f| f.size * (self.files.len() as u64).saturating_sub(1))
            .unwrap_or(0);
    }
}

/// Incremental duplicate detector.
///
/// Files arrive in batches. A file is only hashed once another file of the
/// same size has been seen (quick hash of the first 64 KiB), and only fully
/// hashed once another file shares its quick hash. Hashing inside a batch
/// runs in parallel with rayon.
pub struct DuplicateFinder {
    groups: Vec<DuplicateGroup>,
    /// hash -> index into `groups`
    group_index: HashMap<String, usize>,
    /// Every file seen so far, keyed by size. Hashes are filled in lazily.
    size_groups: HashMap<u64, Vec<FileInfo>>,
    total_duplicates: usize,
    total_wasted_space: u64,
    needs_sort: bool,
}

impl DuplicateFinder {
    pub fn new() -> Self {
        DuplicateFinder {
            groups: Vec::new(),
            group_index: HashMap::new(),
            size_groups: HashMap::new(),
            total_duplicates: 0,
            total_wasted_space: 0,
            needs_sort: false,
        }
    }

    /// Process a single file. Prefer `process_batch` for throughput.
    pub fn process_file(&mut self, file: FileInfo) -> Result<()> {
        self.process_batch(vec![file]);
        Ok(())
    }

    /// Process a batch of newly discovered files.
    pub fn process_batch(&mut self, files: Vec<FileInfo>) {
        if files.is_empty() {
            return;
        }

        // 1. Register every file under its size and remember which sizes now
        //    hold more than one file (those are the only candidates).
        let mut candidate_sizes: HashSet<u64> = HashSet::new();
        for file in files {
            let size = file.size;
            let bucket = self.size_groups.entry(size).or_default();
            if bucket.iter().any(|f| f.path == file.path) {
                continue; // same path delivered twice
            }
            bucket.push(file);
            if bucket.len() >= 2 {
                candidate_sizes.insert(size);
            }
        }
        if candidate_sizes.is_empty() {
            return;
        }

        // Take the candidate buckets out of the map so they can be hashed in
        // parallel without holding a borrow on `self`.
        let mut buckets: Vec<Vec<FileInfo>> = candidate_sizes
            .iter()
            .filter_map(|s| self.size_groups.remove(s))
            .collect();

        // 2. Quick-hash every file in a candidate bucket that lacks one.
        buckets.par_iter_mut().for_each(|bucket| {
            bucket
                .par_iter_mut()
                .filter(|f| f.quick_hash.is_none())
                .for_each(|f| {
                    let _ = f.compute_quick_hash();
                });
        });

        // 3. Fully hash any file whose quick hash is shared within its bucket
        //    and that still lacks a full hash.
        buckets.par_iter_mut().for_each(|bucket| {
            let mut counts: HashMap<String, usize> = HashMap::new();
            for f in bucket.iter() {
                if let Some(q) = &f.quick_hash {
                    *counts.entry(q.clone()).or_insert(0) += 1;
                }
            }
            bucket
                .par_iter_mut()
                .filter(|f| {
                    f.hash.is_none()
                        && f.quick_hash
                            .as_ref()
                            .map(|q| counts.get(q).copied().unwrap_or(0) >= 2)
                            .unwrap_or(false)
                })
                .for_each(|f| {
                    let _ = f.compute_hash();
                });
        });

        // 4. Rebuild the affected duplicate groups from the buckets.
        let mut touched: Vec<(String, Vec<FileInfo>)> = Vec::new();
        for bucket in &buckets {
            let mut by_hash: HashMap<&str, Vec<&FileInfo>> = HashMap::new();
            for f in bucket {
                if let Some(h) = f.hash.as_deref() {
                    by_hash.entry(h).or_default().push(f);
                }
            }
            for (hash, members) in by_hash {
                if members.len() >= 2 {
                    touched.push((hash.to_string(), members.into_iter().cloned().collect()));
                }
            }
        }

        // Put the buckets back.
        for bucket in buckets {
            if let Some(first) = bucket.first() {
                self.size_groups.insert(first.size, bucket);
            }
        }

        for (hash, members) in touched {
            self.upsert_group(hash, members);
        }
    }

    fn upsert_group(&mut self, hash: String, files: Vec<FileInfo>) {
        match self.group_index.get(&hash) {
            Some(&idx) => {
                if self.groups[idx].files.len() != files.len() {
                    self.groups[idx] = DuplicateGroup::new(hash, files);
                    self.needs_sort = true;
                }
            }
            None => {
                self.group_index.insert(hash.clone(), self.groups.len());
                self.groups.push(DuplicateGroup::new(hash, files));
                self.needs_sort = true;
            }
        }
    }

    pub fn groups(&self) -> &[DuplicateGroup] {
        &self.groups
    }

    pub fn groups_mut(&mut self) -> &mut Vec<DuplicateGroup> {
        &mut self.groups
    }

    /// Sort groups by wasted space (largest first) and refresh totals.
    /// Cheap to call repeatedly; does nothing when already sorted.
    pub fn ensure_sorted(&mut self) {
        if self.needs_sort {
            self.groups.sort_by(|a, b| {
                b.wasted_space
                    .cmp(&a.wasted_space)
                    .then_with(|| a.hash.cmp(&b.hash))
            });
            self.rebuild_index();
            self.needs_sort = false;
        }
        self.recalculate_stats();
    }

    pub fn total_duplicates(&self) -> usize {
        self.total_duplicates
    }

    pub fn total_wasted_space(&self) -> u64 {
        self.total_wasted_space
    }

    /// Number of distinct files seen so far (all sizes).
    pub fn files_seen(&self) -> usize {
        self.size_groups.values().map(|v| v.len()).sum()
    }

    /// Forget files that were deleted or renamed away, in every structure.
    pub fn remove_paths(&mut self, paths: &HashSet<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        for bucket in self.size_groups.values_mut() {
            bucket.retain(|f| !paths.contains(&f.path));
        }
        self.size_groups.retain(|_, v| !v.is_empty());
        for group in &mut self.groups {
            group.remove_paths(paths);
        }
        self.remove_empty_groups();
    }

    pub fn remove_empty_groups(&mut self) {
        self.groups.retain(|g| !g.is_empty());
        self.rebuild_index();
        self.recalculate_stats();
    }

    /// Replace all groups (used when loading a finished scan from storage).
    pub fn replace_groups(&mut self, groups: Vec<DuplicateGroup>) {
        self.groups = groups.into_iter().filter(|g| !g.is_empty()).collect();
        self.size_groups.clear();
        for g in &self.groups {
            for f in &g.files {
                self.size_groups.entry(f.size).or_default().push(f.clone());
            }
        }
        self.needs_sort = true;
        self.ensure_sorted();
    }

    pub fn find_group_containing(&self, path: &Path) -> Option<&DuplicateGroup> {
        self.groups.iter().find(|g| g.files.iter().any(|f| f.path == path))
    }

    fn rebuild_index(&mut self) {
        self.group_index.clear();
        for (i, g) in self.groups.iter().enumerate() {
            self.group_index.insert(g.hash.clone(), i);
        }
    }

    fn recalculate_stats(&mut self) {
        self.total_duplicates = self.groups.iter().map(|g| g.file_count()).sum();
        self.total_wasted_space = self.groups.iter().map(|g| g.wasted_space).sum();
    }
}

impl Default for DuplicateFinder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn info(path: &str, size: u64, hash: &str) -> FileInfo {
        FileInfo {
            path: PathBuf::from(path),
            size,
            quick_hash: Some(format!("q{hash}")),
            hash: Some(hash.to_string()),
            modified: SystemTime::now(),
            depth: 2,
        }
    }

    fn scan_dir(dir: &Path) -> DuplicateFinder {
        let mut finder = DuplicateFinder::new();
        let mut files: Vec<FileInfo> = walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| FileInfo::from_path(e.path()).unwrap())
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        finder.process_batch(files);
        finder.ensure_sorted();
        finder
    }

    #[test]
    fn group_arithmetic() {
        let group = DuplicateGroup::new(
            "abc".into(),
            vec![info("/t/a", 100, "abc"), info("/t/b", 100, "abc")],
        );
        assert_eq!(group.file_count(), 2);
        assert_eq!(group.total_size, 200);
        assert_eq!(group.wasted_space, 100);
    }

    #[test]
    fn detects_a_pair_of_identical_files() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), b"same content").unwrap();
        fs::write(dir.path().join("b.txt"), b"same content").unwrap();

        let finder = scan_dir(dir.path());
        assert_eq!(finder.groups().len(), 1);
        assert_eq!(finder.groups()[0].file_count(), 2);
        assert_eq!(finder.groups()[0].wasted_space, "same content".len() as u64);
    }

    #[test]
    fn every_member_of_a_triple_is_reported() {
        let dir = TempDir::new().unwrap();
        for name in ["x.txt", "y.txt", "z.txt"] {
            fs::write(dir.path().join(name), b"other content").unwrap();
        }
        let finder = scan_dir(dir.path());
        assert_eq!(finder.groups().len(), 1);
        assert_eq!(finder.groups()[0].file_count(), 3);
    }

    #[test]
    fn same_size_different_content_is_not_a_duplicate() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), b"aaaaaaaa").unwrap();
        fs::write(dir.path().join("b.txt"), b"bbbbbbbb").unwrap();
        let finder = scan_dir(dir.path());
        assert!(finder.groups().is_empty());
    }

    #[test]
    fn same_prefix_different_tail_is_not_a_duplicate() {
        let dir = TempDir::new().unwrap();
        let mut data = vec![1u8; 100 * 1024];
        fs::write(dir.path().join("a.bin"), &data).unwrap();
        data[90 * 1024] = 9;
        fs::write(dir.path().join("b.bin"), &data).unwrap();
        let finder = scan_dir(dir.path());
        assert!(finder.groups().is_empty());
    }

    #[test]
    fn files_arriving_in_separate_batches_are_grouped() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), b"batch content").unwrap();
        fs::write(dir.path().join("b.txt"), b"batch content").unwrap();
        fs::write(dir.path().join("c.txt"), b"batch content").unwrap();

        let mut finder = DuplicateFinder::new();
        for name in ["a.txt", "b.txt", "c.txt"] {
            finder.process_file(FileInfo::from_path(&dir.path().join(name)).unwrap()).unwrap();
        }
        finder.ensure_sorted();
        assert_eq!(finder.groups().len(), 1);
        assert_eq!(finder.groups()[0].file_count(), 3);
        assert_eq!(finder.total_duplicates(), 3);
    }

    #[test]
    fn removing_paths_shrinks_and_drops_groups() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), b"same content").unwrap();
        fs::write(dir.path().join("b.txt"), b"same content").unwrap();
        let mut finder = scan_dir(dir.path());

        let mut gone = HashSet::new();
        gone.insert(dir.path().join("b.txt"));
        finder.remove_paths(&gone);
        assert!(finder.groups().is_empty());
        assert_eq!(finder.total_wasted_space(), 0);
    }
}
