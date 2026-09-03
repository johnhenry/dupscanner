//! Auto-select rules shared by the TUI and the web UI.
//!
//! Every mode picks files to mark for deletion inside one group and never
//! marks every member. Applying a mode to a group first clears that group's
//! existing marks, so a survivor can never stay marked from an earlier
//! manual click.

use crate::duplicates::DuplicateGroup;
use crate::suggestions::SuggestionEngine;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectMode {
    /// Files whose deletion score is positive, keeper excluded.
    Suggested,
    /// Everything except the keeper chosen by the heuristics.
    AllButKeeper,
    /// Everything except the oldest file.
    AllButOldest,
    /// Everything except the newest file.
    AllButNewest,
    /// Everything except the file with the shortest path.
    AllButShortestPath,
    /// Everything except the file with the longest path.
    AllButLongestPath,
}

impl SelectMode {
    pub const ALL: [SelectMode; 6] = [
        SelectMode::Suggested,
        SelectMode::AllButKeeper,
        SelectMode::AllButOldest,
        SelectMode::AllButNewest,
        SelectMode::AllButShortestPath,
        SelectMode::AllButLongestPath,
    ];

    pub fn parse(s: &str) -> Option<SelectMode> {
        match s.trim() {
            "suggested" => Some(SelectMode::Suggested),
            "allButKeeper" | "keeper" => Some(SelectMode::AllButKeeper),
            "allButOldest" | "oldest" => Some(SelectMode::AllButOldest),
            "allButNewest" | "newest" => Some(SelectMode::AllButNewest),
            "allButShortestPath" | "allButShortest" | "shortest" => Some(SelectMode::AllButShortestPath),
            "allButLongestPath" | "allButLongest" | "longest" => Some(SelectMode::AllButLongestPath),
            _ => None,
        }
    }

    pub fn key(&self) -> &'static str {
        match self {
            SelectMode::Suggested => "suggested",
            SelectMode::AllButKeeper => "allButKeeper",
            SelectMode::AllButOldest => "allButOldest",
            SelectMode::AllButNewest => "allButNewest",
            SelectMode::AllButShortestPath => "allButShortestPath",
            SelectMode::AllButLongestPath => "allButLongestPath",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SelectMode::Suggested => "Mark suggested copies",
            SelectMode::AllButKeeper => "Mark all but keeper",
            SelectMode::AllButOldest => "Mark all but oldest",
            SelectMode::AllButNewest => "Mark all but newest",
            SelectMode::AllButShortestPath => "Mark all but shortest path",
            SelectMode::AllButLongestPath => "Mark all but longest path",
        }
    }

    /// Single-letter shortcut used by the TUI's mark menu.
    pub fn shortcut(&self) -> char {
        match self {
            SelectMode::Suggested => 's',
            SelectMode::AllButKeeper => 'k',
            SelectMode::AllButOldest => 'o',
            SelectMode::AllButNewest => 'n',
            SelectMode::AllButShortestPath => 'h',
            SelectMode::AllButLongestPath => 'l',
        }
    }

    pub fn from_shortcut(c: char) -> Option<SelectMode> {
        Self::ALL.iter().copied().find(|m| m.shortcut() == c.to_ascii_lowercase())
    }
}

/// Indices of the files in `group` that `mode` would mark. Never all of them.
pub fn select_for_deletion(group: &DuplicateGroup, mode: SelectMode) -> Vec<usize> {
    let n = group.files.len();
    if n < 2 {
        return Vec::new();
    }
    let mut picked: Vec<usize> = match mode {
        SelectMode::Suggested => {
            let analysis = SuggestionEngine::analyze(&group.files);
            analysis.suggested_deletions().map(|s| s.file_index).collect()
        }
        SelectMode::AllButKeeper => {
            let analysis = SuggestionEngine::analyze(&group.files);
            analysis.all_but_keeper().map(|s| s.file_index).collect()
        }
        SelectMode::AllButOldest | SelectMode::AllButNewest | SelectMode::AllButShortestPath | SelectMode::AllButLongestPath => {
            let survivor = (0..n)
                .min_by(|&a, &b| {
                    let fa = &group.files[a];
                    let fb = &group.files[b];
                    let primary = match mode {
                        SelectMode::AllButOldest => fa.modified.cmp(&fb.modified),
                        SelectMode::AllButNewest => fb.modified.cmp(&fa.modified),
                        SelectMode::AllButShortestPath => {
                            fa.path.as_os_str().len().cmp(&fb.path.as_os_str().len())
                        }
                        SelectMode::AllButLongestPath => {
                            fb.path.as_os_str().len().cmp(&fa.path.as_os_str().len())
                        }
                        _ => std::cmp::Ordering::Equal,
                    };
                    primary.then_with(|| fa.path.cmp(&fb.path))
                })
                .unwrap_or(0);
            (0..n).filter(|&i| i != survivor).collect()
        }
    };
    picked.sort_unstable();
    picked.dedup();
    if picked.len() >= n {
        picked.truncate(n - 1);
    }
    picked
}

/// Apply `mode` to `group`: clear the group's existing marks, then mark the
/// selected files. Returns how many files are now marked in the group.
pub fn apply_mode(marks: &mut HashSet<PathBuf>, group: &DuplicateGroup, mode: SelectMode) -> usize {
    for f in &group.files {
        marks.remove(&f.path);
    }
    let picked = select_for_deletion(group, mode);
    for &i in &picked {
        marks.insert(group.files[i].path.clone());
    }
    picked.len()
}

/// Paths `mode` would mark across `groups` (no mutation).
pub fn paths_for_mode<'a, I>(groups: I, mode: SelectMode) -> Vec<PathBuf>
where
    I: IntoIterator<Item = &'a DuplicateGroup>,
{
    let mut out = Vec::new();
    for g in groups {
        for i in select_for_deletion(g, mode) {
            out.push(g.files[i].path.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::FileInfo;
    use std::time::{Duration, UNIX_EPOCH};

    fn info(path: &str, age: u64) -> FileInfo {
        FileInfo {
            path: PathBuf::from(path),
            size: 10,
            quick_hash: None,
            hash: Some("h".into()),
            modified: UNIX_EPOCH + Duration::from_secs(1_000_000 - age),
            depth: PathBuf::from(path).components().count(),
        }
    }

    fn group() -> DuplicateGroup {
        DuplicateGroup::new(
            "h".into(),
            vec![
                info("/home/u/Documents/report.pdf", 500),
                info("/home/u/Downloads/report.pdf", 10),
                info("/home/u/a/b/c/d/report copy.pdf", 0),
            ],
        )
    }

    fn names(g: &DuplicateGroup, idx: &[usize]) -> Vec<String> {
        idx.iter().map(|&i| g.files[i].path.display().to_string()).collect()
    }

    #[test]
    fn suggested_and_keeper_modes_follow_the_heuristics() {
        let g = group();
        let keep = SuggestionEngine::analyze(&g.files).keeper.unwrap();
        assert!(g.files[keep].path.ends_with("Documents/report.pdf"));
        let s = select_for_deletion(&g, SelectMode::Suggested);
        assert!(!s.contains(&keep));
        assert_eq!(s.len(), 2);
        let k = select_for_deletion(&g, SelectMode::AllButKeeper);
        assert_eq!(k.len(), 2);
        assert!(!k.contains(&keep));
    }

    #[test]
    fn survivor_modes_keep_exactly_one() {
        let g = group();
        for mode in [
            SelectMode::AllButOldest,
            SelectMode::AllButNewest,
            SelectMode::AllButShortestPath,
            SelectMode::AllButLongestPath,
        ] {
            let picked = select_for_deletion(&g, mode);
            assert_eq!(picked.len(), 2, "{mode:?}");
        }
        let oldest = select_for_deletion(&g, SelectMode::AllButOldest);
        assert!(!names(&g, &oldest).iter().any(|p| p.contains("Documents")));
        let longest = select_for_deletion(&g, SelectMode::AllButLongestPath);
        assert!(!names(&g, &longest).iter().any(|p| p.contains("copy")));
    }

    #[test]
    fn apply_mode_owns_the_groups_marks() {
        let g = group();
        let mut marks: HashSet<PathBuf> = HashSet::new();
        marks.insert(g.files[0].path.clone()); // manual mark on the eventual survivor
        let n = apply_mode(&mut marks, &g, SelectMode::AllButKeeper);
        assert_eq!(n, 2);
        assert_eq!(marks.len(), 2);
        assert!(!marks.contains(&g.files[0].path));
    }

    #[test]
    fn never_marks_every_copy() {
        let g = DuplicateGroup::new("h".into(), vec![info("/x/a", 0), info("/x/b", 0)]);
        for mode in SelectMode::ALL {
            assert!(select_for_deletion(&g, mode).len() < 2, "{mode:?}");
        }
    }

    #[test]
    fn parse_and_shortcuts_round_trip() {
        for mode in SelectMode::ALL {
            assert_eq!(SelectMode::parse(mode.key()), Some(mode));
            assert_eq!(SelectMode::from_shortcut(mode.shortcut()), Some(mode));
        }
    }
}
