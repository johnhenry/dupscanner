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

    pub fn remove_file(&mut self, index: usize) -> Option<FileInfo> {
        if index < self.files.len() {
            let file = self.files.remove(index);
            self.update_stats();
            Some(file)
        } else {
            None
        }
    }

    fn update_stats(&mut self) {
        self.total_size = self.files.iter().map(|f| f.size).sum();
        self.wasted_space = if self.files.is_empty() {
            0
        } else {
            self.files[0].size * (self.files.len() as u64 - 1)
        };
    }

    pub fn is_empty(&self) -> bool {
        self.files.len() <= 1
    }
}

pub struct DuplicateFinder {
    groups: Vec<DuplicateGroup>,
    total_duplicates: usize,
    total_wasted_space: u64,
}

impl DuplicateFinder {
    pub fn new() -> Self {
        DuplicateFinder {
            groups: Vec::new(),
            total_duplicates: 0,
            total_wasted_space: 0,
        }
    }

    pub fn find_duplicates(&mut self, size_groups: HashMap<u64, Vec<FileInfo>>) -> Result<()> {
        let mut hash_map: HashMap<String, Vec<FileInfo>> = HashMap::new();

        // Only process size groups with multiple files
        for (_, mut files) in size_groups {
            if files.len() < 2 {
                continue;
            }

            // Compute hashes for all files in this size group
            for file in &mut files {
                if let Ok(hash) = file.get_or_compute_hash() {
                    hash_map
                        .entry(hash.to_string())
                        .or_insert_with(Vec::new)
                        .push(file.clone());
                }
            }
        }

        // Create duplicate groups from hash map
        for (hash, files) in hash_map {
            if files.len() > 1 {
                let group = DuplicateGroup::new(hash, files);
                self.total_duplicates += group.file_count();
                self.total_wasted_space += group.wasted_space;
                self.groups.push(group);
            }
        }

        // Sort groups by wasted space (descending)
        self.groups.sort_by(|a, b| b.wasted_space.cmp(&a.wasted_space));

        Ok(())
    }

    pub fn add_files_from_size_group(&mut self, mut files: Vec<FileInfo>) -> Result<()> {
        if files.len() < 2 {
            return Ok(());
        }

        let mut hash_map: HashMap<String, Vec<FileInfo>> = HashMap::new();

        for file in &mut files {
            if let Ok(hash) = file.get_or_compute_hash() {
                hash_map
                    .entry(hash.to_string())
                    .or_insert_with(Vec::new)
                    .push(file.clone());
            }
        }

        for (hash, files) in hash_map {
            if files.len() > 1 {
                // Check if group already exists
                if let Some(existing_group) = self.groups.iter_mut().find(|g| g.hash == hash) {
                    // Merge files
                    for file in files {
                        if !existing_group.files.iter().any(|f| f.path == file.path) {
                            existing_group.files.push(file);
                        }
                    }
                    existing_group.update_stats();
                } else {
                    // Create new group
                    let group = DuplicateGroup::new(hash, files);
                    self.total_duplicates += group.file_count();
                    self.total_wasted_space += group.wasted_space;
                    self.groups.push(group);
                }
            }
        }

        // Re-sort groups
        self.groups.sort_by(|a, b| b.wasted_space.cmp(&a.wasted_space));

        Ok(())
    }

    pub fn groups(&self) -> &[DuplicateGroup] {
        &self.groups
    }

    pub fn groups_mut(&mut self) -> &mut Vec<DuplicateGroup> {
        &mut self.groups
    }

    pub fn total_duplicates(&self) -> usize {
        self.total_duplicates
    }

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
                hash: Some("abc123".to_string()),
                modified: SystemTime::now(),
                depth: 2,
            },
            FileInfo {
                path: PathBuf::from("/test/file2.txt"),
                size: 100,
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
    fn test_remove_file() {
        let files = vec![
            FileInfo {
                path: PathBuf::from("/test/file1.txt"),
                size: 100,
                hash: Some("abc123".to_string()),
                modified: SystemTime::now(),
                depth: 2,
            },
            FileInfo {
                path: PathBuf::from("/test/file2.txt"),
                size: 100,
                hash: Some("abc123".to_string()),
                modified: SystemTime::now(),
                depth: 2,
            },
        ];

        let mut group = DuplicateGroup::new("abc123".to_string(), files);
        assert_eq!(group.file_count(), 2);

        group.remove_file(0);
        assert_eq!(group.file_count(), 1);
        assert_eq!(group.wasted_space, 0);
    }
}
