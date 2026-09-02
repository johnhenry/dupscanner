use crate::scanner::FileInfo;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub hash: String,
    pub files: Vec<FileInfo>,
    pub total_size: u64,
    pub wasted_space: u64,
}

impl DuplicateGroup {
    pub fn new(hash: String, files: Vec<FileInfo>) -> Self {
        let total_size = files.iter().map(|f| f.size).sum();
        let wasted_space = if files.is_empty() {
            0
        } else {
            files[0].size * (files.len() as u64 - 1)
        };

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

    pub fn is_empty(&self) -> bool {
        self.files.len() <= 1
    }
}

pub struct DuplicateFinder {
    groups: Vec<DuplicateGroup>,
    total_duplicates: usize,
    total_wasted_space: u64,
    // For incremental processing
    size_groups: HashMap<u64, Vec<FileInfo>>,
    hash_groups: HashMap<String, Vec<FileInfo>>,
    needs_sort: bool,
    needs_stats_update: bool,
}

impl DuplicateFinder {
    pub fn new() -> Self {
        DuplicateFinder {
            groups: Vec::new(),
            total_duplicates: 0,
            total_wasted_space: 0,
            size_groups: HashMap::new(),
            hash_groups: HashMap::new(),
            needs_sort: false,
            needs_stats_update: false,
        }
    }

    /// Process a single file incrementally - checks for duplicates as files arrive
    pub fn process_file(&mut self, mut file: FileInfo) -> Result<()> {
        let size = file.size;

        // Add to size group
        let size_group = self.size_groups.entry(size).or_insert_with(Vec::new);

        // Check if we have a size duplicate
        let has_size_duplicate = !size_group.is_empty();

        size_group.push(file.clone());

        if !has_size_duplicate {
            return Ok(()); // First file of this size, no duplicates yet
        }

        // We have a size duplicate! Now compute quick_hash for this file AND
        // any other files in the group that don't have it yet
        if file.quick_hash.is_none() {
            let _ = file.compute_quick_hash();
        }

        // Compute quick_hash for other files in this size group if not already done
        let size_group = self.size_groups.get_mut(&size).unwrap();
        for other_file in size_group.iter_mut() {
            if other_file.quick_hash.is_none() && other_file.path != file.path {
                let _ = other_file.compute_quick_hash();
            }
        }

        // Now check for quick_hash matches
        let file_qh = file.quick_hash.as_ref();
        let has_quick_hash_match = if let Some(qh) = file_qh {
            size_group.iter().any(|f| {
                f.path != file.path && f.quick_hash.as_ref() == Some(qh)
            })
        } else {
            false
        };

        if !has_quick_hash_match {
            return Ok(()); // No quick_hash matches, not duplicates
        }

        // We have a quick_hash match! Compute full hash
        if file.hash.is_none() {
            let _ = file.get_or_compute_hash();
        }

        if let Some(ref hash) = file.hash {
            // Add to hash groups and check if we need to update
            let hash_clone = hash.clone();
            let should_update = {
                let hash_group = self.hash_groups.entry(hash_clone.clone()).or_insert_with(Vec::new);
                hash_group.push(file.clone());
                hash_group.len() >= 2
            };

            // Update the duplicate group (but defer sorting) - done outside the borrow
            if should_update {
                let files = self.hash_groups.get(&hash_clone).unwrap().clone();
                self.update_duplicate_group_deferred(hash_clone, files);
            }
        }

        Ok(())
    }

    fn update_duplicate_group_deferred(&mut self, hash: String, files: Vec<FileInfo>) {
        // Find existing group or create new one
        if let Some(group) = self.groups.iter_mut().find(|g| g.hash == hash) {
            // Update existing group
            *group = DuplicateGroup::new(hash, files);
        } else {
            // Create new group
            let group = DuplicateGroup::new(hash, files);
            self.groups.push(group);
        }

        // Mark that we need to sort later (don't sort on every update)
        self.needs_sort = true;
        self.needs_stats_update = true;
    }

    pub fn groups(&self) -> &[DuplicateGroup] {
        &self.groups
    }

    /// Call this before displaying groups to ensure they're sorted
    pub fn ensure_sorted(&mut self) {
        if self.needs_sort {
            self.groups.sort_by(|a, b| b.wasted_space.cmp(&a.wasted_space));
            self.needs_sort = false;
        }
        if self.needs_stats_update {
            self.recalculate_stats();
            self.needs_stats_update = false;
        }
    }

    pub fn groups_mut(&mut self) -> &mut Vec<DuplicateGroup> {
        &mut self.groups
    }

    #[allow(dead_code)]
    pub fn total_duplicates(&self) -> usize {
        self.total_duplicates
    }

    #[allow(dead_code)]
    pub fn total_wasted_space(&self) -> u64 {
        self.total_wasted_space
    }

    pub fn remove_empty_groups(&mut self) {
        self.groups.retain(|g| !g.is_empty());
        self.recalculate_stats();
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
    use std::path::PathBuf;
    use std::time::SystemTime;

    #[test]
    fn test_duplicate_group_creation() {
        let files = vec![
            FileInfo {
                path: PathBuf::from("/test/file1.txt"),
                size: 100,
                quick_hash: Some("quick123".to_string()),
                hash: Some("abc123".to_string()),
                modified: SystemTime::now(),
                depth: 2,
            },
            FileInfo {
                path: PathBuf::from("/test/file2.txt"),
                size: 100,
                quick_hash: Some("quick123".to_string()),
                hash: Some("abc123".to_string()),
                modified: SystemTime::now(),
                depth: 2,
            },
        ];

        let group = DuplicateGroup::new("abc123".to_string(), files);
        assert_eq!(group.file_count(), 2);
        assert_eq!(group.total_size, 200);
        assert_eq!(group.wasted_space, 100);
    }

    #[test]
    fn test_quick_hash_filtering() {
        let files = vec![
            FileInfo {
                path: PathBuf::from("/test/file1.txt"),
                size: 100,
                quick_hash: Some("quick123".to_string()),
                hash: Some("abc123".to_string()),
                modified: SystemTime::now(),
                depth: 2,
            },
            FileInfo {
                path: PathBuf::from("/test/file2.txt"),
                size: 100,
                quick_hash: Some("quick456".to_string()),
                hash: Some("def456".to_string()),
                modified: SystemTime::now(),
                depth: 2,
            },
        ];

        let group = DuplicateGroup::new("abc123".to_string(), files);
        assert_eq!(group.file_count(), 2);
    }
}
