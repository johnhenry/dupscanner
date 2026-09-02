use crate::scanner::FileInfo;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionReason {
    DeeperPath,
    Newer,
    InTempDirectory,
    InDownloadsDirectory,
    HasCopyInName,
    LongerFilename,
}

impl SuggestionReason {
    #[allow(dead_code)]
    pub fn description(&self) -> &str {
        match self {
            SuggestionReason::DeeperPath => "Deeper in directory tree",
            SuggestionReason::Newer => "Newer file (less likely to be referenced)",
            SuggestionReason::InTempDirectory => "In temporary directory",
            SuggestionReason::InDownloadsDirectory => "In downloads directory",
            SuggestionReason::HasCopyInName => "Filename contains 'copy'",
            SuggestionReason::LongerFilename => "Longer filename (possibly a copy)",
        }
    }

    pub fn priority(&self) -> u32 {
        match self {
            SuggestionReason::InTempDirectory => 100,
            SuggestionReason::HasCopyInName => 80,
            SuggestionReason::InDownloadsDirectory => 60,
            SuggestionReason::DeeperPath => 40,
            SuggestionReason::LongerFilename => 30,
            SuggestionReason::Newer => 20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileSuggestion {
    pub file_index: usize,
    #[allow(dead_code)]
    pub reasons: Vec<SuggestionReason>,
    pub score: u32,
}

impl FileSuggestion {
    pub fn new(file_index: usize, reasons: Vec<SuggestionReason>) -> Self {
        let score = reasons.iter().map(|r| r.priority()).sum();
        FileSuggestion {
            file_index,
            reasons,
            score,
        }
    }
}

pub struct SuggestionEngine;

impl SuggestionEngine {
    pub fn suggest_deletions(files: &[FileInfo]) -> Vec<FileSuggestion> {
        if files.len() < 2 {
            return Vec::new();
        }

        let mut suggestions = Vec::new();

        for (i, file) in files.iter().enumerate() {
            let mut reasons = Vec::new();

            // Check depth
            let max_depth = files.iter().map(|f| f.depth).max().unwrap_or(0);
            if file.depth == max_depth && file.depth > files.iter().map(|f| f.depth).min().unwrap_or(0) {
                reasons.push(SuggestionReason::DeeperPath);
            }

            // Check if in temp directory
            if is_in_temp_directory(&file.path) {
                reasons.push(SuggestionReason::InTempDirectory);
            }

            // Check if in downloads directory
            if is_in_downloads_directory(&file.path) {
                reasons.push(SuggestionReason::InDownloadsDirectory);
            }

            // Check if filename contains "copy"
            if filename_contains_copy(&file.path) {
                reasons.push(SuggestionReason::HasCopyInName);
            }

            // Check if newer than others
            if let Some(oldest) = files.iter().map(|f| f.modified).min() {
                if file.modified > oldest {
                    reasons.push(SuggestionReason::Newer);
                }
            }

            // Check filename length
            if let Some(filename) = file.path.file_name() {
                let max_len = files
                    .iter()
                    .filter_map(|f| f.path.file_name())
                    .map(|n| n.len())
                    .max()
                    .unwrap_or(0);

                if filename.len() == max_len && filename.len() > 0 {
                    let min_len = files
                        .iter()
                        .filter_map(|f| f.path.file_name())
                        .map(|n| n.len())
                        .min()
                        .unwrap_or(0);

                    if max_len > min_len + 5 {
                        reasons.push(SuggestionReason::LongerFilename);
                    }
                }
            }

            if !reasons.is_empty() {
                suggestions.push(FileSuggestion::new(i, reasons));
            }
        }

        // Sort by score (descending)
        suggestions.sort_by(|a, b| b.score.cmp(&a.score));

        suggestions
    }

    pub fn get_best_keeper(files: &[FileInfo]) -> Option<usize> {
        if files.is_empty() {
            return None;
        }

        // Find the file that is least likely to be deleted
        let suggestions = Self::suggest_deletions(files);

        // Create a set of suggested deletion indices
        let suggested_for_deletion: std::collections::HashSet<usize> =
            suggestions.iter().map(|s| s.file_index).collect();

        // Find files not suggested for deletion
        let keepers: Vec<usize> = (0..files.len())
            .filter(|i| !suggested_for_deletion.contains(i))
            .collect();

        if !keepers.is_empty() {
            // Prefer the oldest file among keepers
            let keeper_index = keepers.iter()
                .min_by(|&&a, &&b| files[a].modified.cmp(&files[b].modified))
                .copied()?;
            Some(keeper_index)
        } else {
            // If all files are suggested for deletion, keep the one with lowest score
            let keeper_index = (0..files.len())
                .min_by_key(|&i| {
                    suggestions.iter()
                        .find(|s| s.file_index == i)
                        .map(|s| s.score)
                        .unwrap_or(0)
                })?;
            Some(keeper_index)
        }
    }

    #[allow(dead_code)]
    pub fn sort_by_preference(files: &mut [FileInfo]) {
        files.sort_by(|a, b| {
            // Prefer files that are NOT in temp/downloads
            let a_bad = is_in_temp_directory(&a.path) || is_in_downloads_directory(&a.path);
            let b_bad = is_in_temp_directory(&b.path) || is_in_downloads_directory(&b.path);

            if a_bad != b_bad {
                return a_bad.cmp(&b_bad);
            }

            // Prefer files without "copy" in name
            let a_copy = filename_contains_copy(&a.path);
            let b_copy = filename_contains_copy(&b.path);

            if a_copy != b_copy {
                return a_copy.cmp(&b_copy);
            }

            // Prefer shallower paths
            if a.depth != b.depth {
                return a.depth.cmp(&b.depth);
            }

            // Prefer older files
            a.modified.cmp(&b.modified)
        });
    }
}

fn is_in_temp_directory(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().contains("/tmp/")
        || path.to_string_lossy().to_lowercase().contains("\\temp\\")
        || path.to_string_lossy().to_lowercase().contains("/temp/")
}

fn is_in_downloads_directory(path: &Path) -> bool {
    path.to_string_lossy().to_lowercase().contains("/downloads/")
        || path.to_string_lossy().to_lowercase().contains("\\downloads\\")
}

fn filename_contains_copy(path: &Path) -> bool {
    // Check the filename
    if let Some(filename) = path.file_name() {
        let name_lower = filename.to_string_lossy().to_lowercase();
        if name_lower.contains("copy") || name_lower.contains("duplicate") || name_lower.contains("(1)") {
            return true;
        }
    }

    // Also check the full path for copy indicators in directory names
    let path_lower = path.to_string_lossy().to_lowercase();
    path_lower.contains("copy")
        || path_lower.contains("duplicate")
        || path_lower.contains("(1)")
        || path_lower.contains(" 2/")  // macOS/Windows copy: "folder 2"
        || path_lower.contains(" 3/")
        || path_lower.contains(" 2\\")  // Windows path
        || path_lower.contains(" 3\\")
        || path_lower.ends_with(" 2")
        || path_lower.ends_with(" 3")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::SystemTime;

    #[test]
    fn test_suggest_deletions() {
        let files = vec![
            FileInfo {
                path: PathBuf::from("/home/user/file.txt"),
                size: 100,
                quick_hash: Some("quick123".to_string()),
                hash: Some("abc".to_string()),
                modified: SystemTime::now(),
                depth: 3,
            },
            FileInfo {
                path: PathBuf::from("/tmp/file.txt"),
                size: 100,
                quick_hash: Some("quick123".to_string()),
                hash: Some("abc".to_string()),
                modified: SystemTime::now(),
                depth: 2,
            },
        ];

        let suggestions = SuggestionEngine::suggest_deletions(&files);
        assert!(!suggestions.is_empty());

        // The file in /tmp should be suggested for deletion
        let tmp_suggestion = suggestions.iter().find(|s| s.file_index == 1);
        assert!(tmp_suggestion.is_some());
    }

    #[test]
    fn test_get_best_keeper() {
        let files = vec![
            FileInfo {
                path: PathBuf::from("/home/user/file.txt"),
                size: 100,
                quick_hash: Some("quick123".to_string()),
                hash: Some("abc".to_string()),
                modified: SystemTime::now(),
                depth: 3,
            },
            FileInfo {
                path: PathBuf::from("/tmp/file_copy.txt"),
                size: 100,
                quick_hash: Some("quick123".to_string()),
                hash: Some("abc".to_string()),
                modified: SystemTime::now(),
                depth: 2,
            },
        ];

        let keeper = SuggestionEngine::get_best_keeper(&files);
        assert_eq!(keeper, Some(0)); // Should keep the first file
    }
}
