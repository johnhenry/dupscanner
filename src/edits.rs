//! Edits a front end makes to files while a scan is still running, and the
//! rename operation both front ends share.
//!
//! The engine keeps sending group snapshots that predate the user's
//! deletions and renames. `ScanEdits` replays those edits onto every
//! snapshot so a deleted file never reappears and a renamed file keeps its
//! new name.

use crate::duplicates::DuplicateGroup;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct ScanEdits {
    deleted: HashSet<PathBuf>,
    /// original path as the engine knows it -> current path
    renamed: HashMap<PathBuf, PathBuf>,
}

impl ScanEdits {
    pub fn is_empty(&self) -> bool {
        self.deleted.is_empty() && self.renamed.is_empty()
    }

    pub fn record_deleted<I: IntoIterator<Item = PathBuf>>(&mut self, paths: I) {
        self.deleted.extend(paths);
    }

    /// Record `old -> new`, chaining through earlier renames so the key is
    /// always the path the engine originally reported.
    pub fn record_rename(&mut self, old: &Path, new: &Path) {
        let key = self
            .renamed
            .iter()
            .find(|(_, v)| v.as_path() == old)
            .map(|(k, _)| k.clone())
            .unwrap_or_else(|| old.to_path_buf());
        self.renamed.insert(key, new.to_path_buf());
    }

    /// Apply the recorded edits to a snapshot from the engine.
    pub fn apply(&self, groups: &mut Vec<DuplicateGroup>) {
        if self.is_empty() {
            groups.retain(|g| !g.is_empty());
            return;
        }
        for g in groups.iter_mut() {
            if !self.renamed.is_empty() {
                for f in &mut g.files {
                    if let Some(new) = self.renamed.get(&f.path) {
                        f.path = new.clone();
                    }
                }
            }
            if !self.deleted.is_empty() {
                g.remove_paths(&self.deleted);
            }
        }
        groups.retain(|g| !g.is_empty());
    }

    pub fn clear(&mut self) {
        self.deleted.clear();
        self.renamed.clear();
    }
}

#[derive(Debug)]
pub enum RenameError {
    InvalidName(String),
    NotInGroups,
    SameName,
    TargetExists(PathBuf),
    Io(String),
}

impl std::fmt::Display for RenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenameError::InvalidName(m) => write!(f, "{m}"),
            RenameError::NotInGroups => write!(f, "File is not part of any duplicate group"),
            RenameError::SameName => write!(f, "The new name is the same as the current name"),
            RenameError::TargetExists(p) => write!(f, "{} already exists", p.display()),
            RenameError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for RenameError {}

/// A new file name must be exactly one normal path component.
pub fn validate_new_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("New name must not be empty".into());
    }
    if name != name.trim() {
        return Err("New name must not start or end with whitespace".into());
    }
    if name.contains('\0') {
        return Err("New name must not contain NUL".into());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("New name must not contain path separators".into());
    }
    if name == "." || name == ".." {
        return Err("New name must be a file name, not . or ..".into());
    }
    if name.len() > 255 {
        return Err("New name is too long (255 bytes max)".into());
    }
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(()),
        _ => Err("New name must be a single file name".into()),
    }
}

/// Rename `old` (which must belong to one of `groups`) to `new_name` in the
/// same directory, then update the group entry. Returns the new path and the
/// index of the group that changed.
pub fn rename_in_groups(
    groups: &mut [DuplicateGroup],
    old: &Path,
    new_name: &str,
) -> Result<(PathBuf, usize), RenameError> {
    validate_new_name(new_name).map_err(RenameError::InvalidName)?;
    let gi = groups
        .iter()
        .position(|g| g.files.iter().any(|f| f.path == old))
        .ok_or(RenameError::NotInGroups)?;
    let parent = old
        .parent()
        .ok_or_else(|| RenameError::Io("Cannot rename a path without a parent directory".into()))?;
    let new = parent.join(new_name);
    if new == old {
        return Err(RenameError::SameName);
    }
    if fs::symlink_metadata(&new).is_ok() {
        return Err(RenameError::TargetExists(new));
    }
    fs::rename(old, &new).map_err(|e| RenameError::Io(format!("Failed to rename {}: {e}", old.display())))?;
    if let Some(f) = groups[gi].files.iter_mut().find(|f| f.path == old) {
        f.path = new.clone();
    }
    Ok((new, gi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::FileInfo;
    use std::time::SystemTime;

    fn info(path: &Path) -> FileInfo {
        FileInfo {
            path: path.to_path_buf(),
            size: 4,
            quick_hash: None,
            hash: Some("h".into()),
            modified: SystemTime::UNIX_EPOCH,
            depth: 2,
        }
    }

    #[test]
    fn new_name_validation() {
        assert!(validate_new_name("report.pdf").is_ok());
        assert!(validate_new_name("with spaces.txt").is_ok());
        assert!(validate_new_name("v1..2.txt").is_ok());
        assert!(validate_new_name(".hidden").is_ok());
        for bad in ["", " x", "x ", "a/b", "a\\b", "..", ".", "nul\0", "/abs"] {
            assert!(validate_new_name(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(validate_new_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn edits_replay_onto_snapshots() {
        let a = PathBuf::from("/r/a.txt");
        let b = PathBuf::from("/r/b.txt");
        let c = PathBuf::from("/r/c.txt");
        let mut edits = ScanEdits::default();
        edits.record_deleted([c.clone()]);
        edits.record_rename(&a, Path::new("/r/a2.txt"));
        edits.record_rename(Path::new("/r/a2.txt"), Path::new("/r/a3.txt"));

        let mut snapshot = vec![DuplicateGroup::new(
            "h".into(),
            vec![info(&a), info(&b), info(&c)],
        )];
        edits.apply(&mut snapshot);
        let paths: Vec<String> = snapshot[0].files.iter().map(|f| f.path.display().to_string()).collect();
        assert_eq!(paths, vec!["/r/a3.txt", "/r/b.txt"]);

        // Deleting down to one member drops the group.
        edits.record_deleted([b.clone()]);
        let mut snapshot = vec![DuplicateGroup::new("h".into(), vec![info(&a), info(&b), info(&c)])];
        edits.apply(&mut snapshot);
        assert!(snapshot.is_empty());
    }

    #[test]
    fn rename_updates_disk_and_group() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, b"same").unwrap();
        fs::write(&b, b"same").unwrap();
        let mut groups = vec![DuplicateGroup::new("h".into(), vec![info(&a), info(&b)])];

        let (new, gi) = rename_in_groups(&mut groups, &a, "renamed.txt").unwrap();
        assert_eq!(gi, 0);
        assert!(new.exists());
        assert!(!a.exists());
        assert!(groups[0].files.iter().any(|f| f.path == new));

        assert!(matches!(
            rename_in_groups(&mut groups, &new, "b.txt"),
            Err(RenameError::TargetExists(_))
        ));
        assert!(matches!(
            rename_in_groups(&mut groups, &new, "renamed.txt"),
            Err(RenameError::SameName)
        ));
        assert!(matches!(
            rename_in_groups(&mut groups, Path::new("/nope"), "x.txt"),
            Err(RenameError::NotInGroups)
        ));
        assert!(matches!(
            rename_in_groups(&mut groups, &new, "a/b"),
            Err(RenameError::InvalidName(_))
        ));
    }
}
