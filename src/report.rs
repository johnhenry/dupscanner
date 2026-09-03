//! Machine-readable output shared by `scan --json`, `view --json` and the
//! web API.

use crate::duplicates::DuplicateGroup;
use crate::naming;
use crate::scanner::ScanProgress;
use crate::suggestions::{SuggestionEngine, SuggestionReason};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct FileReport {
    pub path: PathBuf,
    pub size: u64,
    pub modified: DateTime<Utc>,
    pub keep: bool,
    pub score: i32,
    pub reasons: Vec<SuggestionReason>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupReport {
    pub hash: String,
    pub file_size: u64,
    pub file_count: usize,
    pub wasted_space: u64,
    /// The group's name with copy markers removed, when any member has one.
    pub canonical_name: Option<String>,
    /// Rename that would give the keeper the canonical name, when no
    /// surviving copy carries it and the name is free.
    pub suggested_rename: Option<SuggestedRename>,
    pub files: Vec<FileReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SuggestedRename {
    pub path: PathBuf,
    pub new_name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub root: PathBuf,
    pub generated_at: DateTime<Utc>,
    pub files_scanned: usize,
    pub bytes_scanned: u64,
    pub unreadable: usize,
    pub elapsed_seconds: Option<f64>,
    pub group_count: usize,
    pub duplicate_files: usize,
    pub wasted_space: u64,
    pub groups: Vec<GroupReport>,
}

pub fn group_report(group: &DuplicateGroup) -> GroupReport {
    let analysis = SuggestionEngine::analyze(&group.files);
    let canonical_name = naming::canonical_name(group).map(|c| c.name);
    let suggested_rename = analysis
        .keeper
        .and_then(|k| naming::suggested_rename(group, k))
        .map(|(path, new_name)| SuggestedRename { path, new_name });
    GroupReport {
        hash: group.hash.clone(),
        file_size: group.file_size(),
        file_count: group.file_count(),
        wasted_space: group.wasted_space,
        canonical_name,
        suggested_rename,
        files: group
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| FileReport {
                path: f.path.clone(),
                size: f.size,
                modified: f.modified.into(),
                keep: analysis.keeper == Some(i),
                score: analysis.files.get(i).map(|s| s.score).unwrap_or(0),
                reasons: analysis.files.get(i).map(|s| s.reasons.clone()).unwrap_or_default(),
            })
            .collect(),
    }
}

pub fn scan_report(
    root: &Path,
    groups: &[DuplicateGroup],
    progress: &ScanProgress,
    elapsed: Option<Duration>,
) -> ScanReport {
    ScanReport {
        root: root.to_path_buf(),
        generated_at: Utc::now(),
        files_scanned: progress.scanned_count,
        bytes_scanned: progress.total_size,
        unreadable: progress.skipped,
        elapsed_seconds: elapsed.map(|d| d.as_secs_f64()),
        group_count: groups.len(),
        duplicate_files: groups.iter().map(|g| g.file_count()).sum(),
        wasted_space: groups.iter().map(|g| g.wasted_space).sum(),
        groups: groups.iter().map(group_report).collect(),
    }
}
