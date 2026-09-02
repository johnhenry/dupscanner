//! Heuristics for deciding which copy in a duplicate group to keep.
//!
//! Every file gets a *deletion score*: positive signals (temp directory,
//! "copy" in the name, ...) make it a better candidate for removal, and a
//! preferred location lowers the score. The keeper is the file with the
//! lowest score; ties go to the shallower path, then the older file, then
//! the shorter path, so the choice is deterministic.

use crate::scanner::FileInfo;
use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SuggestionReason {
    InTempDirectory,
    HasCopyInName,
    InDownloadsDirectory,
    InBackupDirectory,
    DeeperPath,
    LongerFilename,
    /// Negative weight: the file lives somewhere people keep originals.
    PreferredLocation,
}

impl SuggestionReason {
    pub fn description(&self) -> &'static str {
        match self {
            SuggestionReason::InTempDirectory => "in a temp directory",
            SuggestionReason::HasCopyInName => "name looks like a copy",
            SuggestionReason::InDownloadsDirectory => "in Downloads",
            SuggestionReason::InBackupDirectory => "in a backup folder",
            SuggestionReason::DeeperPath => "deeper in the tree",
            SuggestionReason::LongerFilename => "longer filename",
            SuggestionReason::PreferredLocation => "in a preferred location",
        }
    }

    pub fn weight(&self) -> i32 {
        match self {
            SuggestionReason::InTempDirectory => 100,
            SuggestionReason::HasCopyInName => 80,
            SuggestionReason::InDownloadsDirectory => 60,
            SuggestionReason::InBackupDirectory => 50,
            SuggestionReason::DeeperPath => 20,
            SuggestionReason::LongerFilename => 10,
            SuggestionReason::PreferredLocation => -40,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct FileSuggestion {
    pub file_index: usize,
    pub reasons: Vec<SuggestionReason>,
    pub score: i32,
}

impl FileSuggestion {
    pub fn new(file_index: usize, reasons: Vec<SuggestionReason>) -> Self {
        let score = reasons.iter().map(|r| r.weight()).sum();
        FileSuggestion {
            file_index,
            reasons,
            score,
        }
    }

    pub fn explain(&self) -> String {
        self.reasons
            .iter()
            .map(|r| r.description())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Complete analysis of one duplicate group.
#[derive(Debug, Clone, Serialize)]
pub struct GroupAnalysis {
    /// Index of the file to keep.
    pub keeper: Option<usize>,
    /// One entry per file, in file order.
    pub files: Vec<FileSuggestion>,
}

impl GroupAnalysis {
    /// Files worth deleting: positive score and not the keeper.
    pub fn suggested_deletions(&self) -> impl Iterator<Item = &FileSuggestion> {
        self.files
            .iter()
            .filter(move |s| Some(s.file_index) != self.keeper && s.score > 0)
    }

    /// Every file except the keeper.
    pub fn all_but_keeper(&self) -> impl Iterator<Item = &FileSuggestion> {
        self.files.iter().filter(move |s| Some(s.file_index) != self.keeper)
    }

    pub fn confidence(&self) -> &'static str {
        let max = self
            .suggested_deletions()
            .map(|s| s.score)
            .max()
            .unwrap_or(0);
        if max >= 100 {
            "high confidence"
        } else if max >= 80 {
            "good confidence"
        } else if max >= 50 {
            "medium confidence"
        } else {
            "low confidence, review carefully"
        }
    }
}

pub struct SuggestionEngine;

impl SuggestionEngine {
    pub fn analyze(files: &[FileInfo]) -> GroupAnalysis {
        if files.is_empty() {
            return GroupAnalysis {
                keeper: None,
                files: Vec::new(),
            };
        }

        let min_depth = files.iter().map(|f| f.depth).min().unwrap_or(0);
        let max_depth = files.iter().map(|f| f.depth).max().unwrap_or(0);
        let name_lens: Vec<usize> = files
            .iter()
            .map(|f| f.path.file_name().map(|n| n.to_string_lossy().chars().count()).unwrap_or(0))
            .collect();
        let min_len = name_lens.iter().copied().min().unwrap_or(0);
        let max_len = name_lens.iter().copied().max().unwrap_or(0);
        let group_stems: HashSet<String> = files.iter().filter_map(|f| stem_lower(&f.path)).collect();

        let mut analysed = Vec::with_capacity(files.len());
        for (i, file) in files.iter().enumerate() {
            let mut reasons = Vec::new();

            if is_in_temp_directory(&file.path) {
                reasons.push(SuggestionReason::InTempDirectory);
            }
            if filename_looks_like_copy(&file.path, &group_stems) {
                reasons.push(SuggestionReason::HasCopyInName);
            }
            if is_in_downloads_directory(&file.path) {
                reasons.push(SuggestionReason::InDownloadsDirectory);
            }
            if is_in_backup_directory(&file.path) {
                reasons.push(SuggestionReason::InBackupDirectory);
            }
            if max_depth > min_depth && file.depth == max_depth {
                reasons.push(SuggestionReason::DeeperPath);
            }
            if max_len > min_len + 5 && name_lens[i] == max_len {
                reasons.push(SuggestionReason::LongerFilename);
            }
            if is_in_preferred_location(&file.path) {
                reasons.push(SuggestionReason::PreferredLocation);
            }

            analysed.push(FileSuggestion::new(i, reasons));
        }

        let keeper = (0..files.len()).min_by(|&a, &b| {
            analysed[a]
                .score
                .cmp(&analysed[b].score)
                .then_with(|| files[a].depth.cmp(&files[b].depth))
                .then_with(|| files[a].modified.cmp(&files[b].modified))
                .then_with(|| {
                    files[a]
                        .path
                        .as_os_str()
                        .len()
                        .cmp(&files[b].path.as_os_str().len())
                })
                .then_with(|| files[a].path.cmp(&files[b].path))
        });

        GroupAnalysis {
            keeper,
            files: analysed,
        }
    }

    /// Files suggested for deletion, highest score first.
    pub fn suggest_deletions(files: &[FileInfo]) -> Vec<FileSuggestion> {
        let analysis = Self::analyze(files);
        let mut out: Vec<FileSuggestion> = analysis.suggested_deletions().cloned().collect();
        out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.file_index.cmp(&b.file_index)));
        out
    }

    pub fn get_best_keeper(files: &[FileInfo]) -> Option<usize> {
        Self::analyze(files).keeper
    }
}

fn components_lower(path: &Path) -> Vec<String> {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

/// Directory components only (everything but the file name).
fn dir_components_lower(path: &Path) -> Vec<String> {
    let mut parts = components_lower(path);
    parts.pop();
    parts
}

fn is_in_temp_directory(path: &Path) -> bool {
    dir_components_lower(path)
        .iter()
        .any(|c| matches!(c.as_str(), "tmp" | "temp" | ".tmp" | "cache" | ".cache"))
}

fn is_in_downloads_directory(path: &Path) -> bool {
    dir_components_lower(path)
        .iter()
        .any(|c| c == "downloads" || c == "download")
}

fn is_in_backup_directory(path: &Path) -> bool {
    dir_components_lower(path).iter().any(|c| {
        c == "backup"
            || c == "backups"
            || c == "old"
            || c == "archive"
            || c.starts_with("backup ")
            || c.ends_with(" backup")
            || c.ends_with("_backup")
            || c.ends_with("-backup")
    })
}

fn is_in_preferred_location(path: &Path) -> bool {
    dir_components_lower(path).iter().any(|c| {
        matches!(
            c.as_str(),
            "documents" | "pictures" | "photos" | "music" | "videos" | "movies" | "projects" | "src" | "desktop"
        )
    })
}

/// Lower-cased file stem (name without its last extension).
fn stem_lower(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    Some(match name.rfind('.') {
        Some(pos) if pos > 0 => name[..pos].to_string(),
        _ => name,
    })
}

/// Only the file name is inspected. Directory names are covered by the
/// backup-directory rule so a folder called "Copy Editing" does not taint
/// every file under it.
///
/// Unambiguous markers ("copy", "duplicate", "backup", "(1)") always count.
/// A trailing number such as "report 2" or "report_2" only counts when the
/// base name ("report") is also in the group, so sequential names like
/// "chapter_2" are not mistaken for copies.
fn filename_looks_like_copy(path: &Path, group_stems: &HashSet<String>) -> bool {
    let Some(stem) = stem_lower(path) else { return false };
    let stem = stem.as_str();

    if stem.starts_with("copy of ") || stem.starts_with("kopie von ") {
        return true;
    }
    if stem.contains("copy") || stem.contains("duplicate") || stem.contains("backup") {
        return true;
    }
    // "report (1)", "report(2)"
    if stem.ends_with(')') {
        if let Some(open) = stem.rfind('(') {
            let inner = &stem[open + 1..stem.len() - 1];
            if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    // "report 2", "report-2", "report_2" when "report" is also present.
    let digits = stem.trim_end_matches(|c: char| c.is_ascii_digit());
    if digits.len() < stem.len() && stem.len() - digits.len() <= 3 {
        let base = digits.trim_end_matches([' ', '-', '_']);
        if base.len() < digits.len() && !base.is_empty() && group_stems.contains(base) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn f(path: &str, age_secs: u64) -> FileInfo {
        FileInfo {
            path: PathBuf::from(path),
            size: 100,
            quick_hash: None,
            hash: Some("abc".into()),
            modified: UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs),
            depth: PathBuf::from(path).components().count(),
        }
    }

    #[test]
    fn temp_copy_is_suggested_and_home_file_kept() {
        let files = vec![f("/home/user/file.txt", 10), f("/tmp/file.txt", 0)];
        let analysis = SuggestionEngine::analyze(&files);
        assert_eq!(analysis.keeper, Some(0));
        let suggested: Vec<usize> = analysis.suggested_deletions().map(|s| s.file_index).collect();
        assert_eq!(suggested, vec![1]);
    }

    #[test]
    fn copy_patterns_in_filename() {
        let with_base: HashSet<String> = ["report".to_string()].into();
        let empty: HashSet<String> = HashSet::new();
        for name in [
            "report copy.pdf",
            "report (1).pdf",
            "report(2).pdf",
            "Copy of report.pdf",
            "report_backup.pdf",
            "report_duplicate.pdf",
        ] {
            assert!(filename_looks_like_copy(Path::new(name), &empty), "{name}");
        }
        // Trailing numbers only count next to their base name.
        for name in ["report 2.pdf", "report-2.pdf", "report_2.pdf", "report 12.pdf"] {
            assert!(filename_looks_like_copy(Path::new(name), &with_base), "{name}");
            assert!(!filename_looks_like_copy(Path::new(name), &empty), "{name}");
        }
        for name in ["report.pdf", "img_2024.jpg", "chapter 12.md", "file_2.txt"] {
            assert!(!filename_looks_like_copy(Path::new(name), &with_base), "{name}");
        }
    }

    #[test]
    fn directory_named_copy_does_not_taint_files() {
        let files = vec![
            f("/home/user/Copy Editing/notes.txt", 10),
            f("/home/user/Downloads/notes.txt", 0),
        ];
        let analysis = SuggestionEngine::analyze(&files);
        assert_eq!(analysis.keeper, Some(0));
        assert!(!analysis.files[0].reasons.contains(&SuggestionReason::HasCopyInName));
    }

    #[test]
    fn preferred_location_wins_over_neutral_path() {
        let files = vec![f("/data/misc/photo.jpg", 0), f("/data/Pictures/photo.jpg", 100)];
        let analysis = SuggestionEngine::analyze(&files);
        assert_eq!(analysis.keeper, Some(1));
    }

    #[test]
    fn ties_prefer_shallower_then_older() {
        let files = vec![f("/a/b/c/x.txt", 5), f("/a/x.txt", 0), f("/a/y.txt", 50)];
        let analysis = SuggestionEngine::analyze(&files);
        // /a/x.txt and /a/y.txt tie on score and depth; y is older.
        assert_eq!(analysis.keeper, Some(2));
    }

    #[test]
    fn identical_neutral_files_still_pick_exactly_one_keeper() {
        let files = vec![f("/a/one.txt", 0), f("/a/two.txt", 0)];
        let analysis = SuggestionEngine::analyze(&files);
        assert!(analysis.keeper.is_some());
        assert_eq!(analysis.suggested_deletions().count(), 0);
        assert_eq!(analysis.all_but_keeper().count(), 1);
    }
}
