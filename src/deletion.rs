//! One deletion path for every front end.
//!
//! Every removal goes through `Deleter`, which either moves the file to the
//! system trash (default) or copies it into the backup store and then
//! removes it. Before anything is removed, `plan_deletions` checks that no
//! duplicate group would lose its last copy.

use crate::backup::BackupManager;
use crate::duplicates::DuplicateGroup;
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMethod {
    /// Move to the OS trash / recycle bin (default).
    Trash,
    /// Copy into the dupscanner backup store, then remove.
    Backup,
    /// Remove without any safety net. Only for `--permanent`.
    Permanent,
}

impl DeleteMethod {
    pub fn label(&self) -> &'static str {
        match self {
            DeleteMethod::Trash => "trash",
            DeleteMethod::Backup => "backup",
            DeleteMethod::Permanent => "permanent",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            DeleteMethod::Trash => "Move to system trash (restore from the trash)",
            DeleteMethod::Backup => "Copy to dupscanner backups, then delete (restore with `dupscanner restore`)",
            DeleteMethod::Permanent => "Delete permanently (no undo)",
        }
    }
}

/// A single removal outcome.
#[derive(Debug, Clone)]
pub struct DeletionOutcome {
    pub path: PathBuf,
    pub size: u64,
    pub result: Result<(), String>,
}

#[derive(Debug, Default, Clone)]
pub struct DeletionReport {
    pub outcomes: Vec<DeletionOutcome>,
}

impl DeletionReport {
    pub fn deleted_paths(&self) -> HashSet<PathBuf> {
        self.outcomes
            .iter()
            .filter(|o| o.result.is_ok())
            .map(|o| o.path.clone())
            .collect()
    }

    pub fn deleted_count(&self) -> usize {
        self.outcomes.iter().filter(|o| o.result.is_ok()).count()
    }

    pub fn failed_count(&self) -> usize {
        self.outcomes.iter().filter(|o| o.result.is_err()).count()
    }

    pub fn bytes_freed(&self) -> u64 {
        self.outcomes
            .iter()
            .filter(|o| o.result.is_ok())
            .map(|o| o.size)
            .sum()
    }

    pub fn failures(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.outcomes.iter().filter_map(|o| match &o.result {
            Err(e) => Some((o.path.as_path(), e.as_str())),
            Ok(()) => None,
        })
    }
}

pub struct Deleter {
    method: DeleteMethod,
    backup: Option<BackupManager>,
}

impl Deleter {
    pub fn new(method: DeleteMethod, backup: Option<BackupManager>) -> Self {
        Deleter { method, backup }
    }

    /// Trash by default; falls back to the backup store if a backup manager
    /// is provided and the trash is unavailable.
    pub fn trash() -> Self {
        Deleter { method: DeleteMethod::Trash, backup: None }
    }

    pub fn method(&self) -> DeleteMethod {
        self.method
    }

    pub fn backup_manager(&self) -> Option<&BackupManager> {
        self.backup.as_ref()
    }

    /// Remove one file according to the configured method. The file must
    /// still match the recorded size, which guards against deleting a file
    /// that changed after it was hashed.
    pub fn delete_one(&mut self, path: &Path, expected_size: Option<u64>) -> Result<()> {
        let meta = fs::symlink_metadata(path)
            .with_context(|| format!("{} no longer exists", path.display()))?;
        if !meta.is_file() {
            bail!("{} is not a regular file", path.display());
        }
        if let Some(size) = expected_size {
            if meta.len() != size {
                bail!(
                    "{} changed size since it was scanned ({} -> {}); skipped",
                    path.display(),
                    size,
                    meta.len()
                );
            }
        }

        match self.method {
            DeleteMethod::Trash => {
                trash::delete(path).map_err(|e| anyhow::anyhow!("Failed to move to trash: {e}"))
            }
            DeleteMethod::Backup => {
                let backup = self
                    .backup
                    .as_mut()
                    .context("Backup deletion requested but no backup store is available")?;
                backup.delete_with_backup(path).map(|_| ())
            }
            DeleteMethod::Permanent => {
                fs::remove_file(path).with_context(|| format!("Failed to delete {}", path.display()))
            }
        }
    }

    /// Delete a set of planned files, collecting per-file outcomes.
    pub fn delete_planned(&mut self, plan: &DeletionPlan) -> DeletionReport {
        let mut report = DeletionReport::default();
        for item in &plan.items {
            let result = self
                .delete_one(&item.path, Some(item.size))
                .map_err(|e| e.to_string());
            report.outcomes.push(DeletionOutcome {
                path: item.path.clone(),
                size: item.size,
                result,
            });
        }
        report
    }
}

#[derive(Debug, Clone)]
pub struct PlannedDeletion {
    pub path: PathBuf,
    pub size: u64,
    pub group_hash: String,
}

#[derive(Debug, Clone, Default)]
pub struct DeletionPlan {
    pub items: Vec<PlannedDeletion>,
}

impl DeletionPlan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn bytes(&self) -> u64 {
        self.items.iter().map(|i| i.size).sum()
    }
}

/// Validate a set of paths the user wants to remove against the current
/// duplicate groups.
///
/// Rules:
/// * every path must belong to a known duplicate group (nothing outside the
///   scan results can be deleted through dupscanner);
/// * no group may lose all of its members, so at least one copy survives.
pub fn plan_deletions(groups: &[DuplicateGroup], wanted: &HashSet<PathBuf>) -> Result<DeletionPlan> {
    let mut plan = DeletionPlan::default();
    let mut seen: HashSet<&PathBuf> = HashSet::new();

    for group in groups {
        let marked: Vec<_> = group
            .files
            .iter()
            .filter(|f| wanted.contains(&f.path))
            .collect();
        if marked.is_empty() {
            continue;
        }
        if marked.len() >= group.files.len() {
            bail!(
                "Refusing to delete every copy of {} ({} files); leave at least one",
                group
                    .files
                    .first()
                    .and_then(|f| f.path.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| group.hash.chars().take(8).collect()),
                group.files.len()
            );
        }
        for f in marked {
            seen.insert(&f.path);
            plan.items.push(PlannedDeletion {
                path: f.path.clone(),
                size: f.size,
                group_hash: group.hash.clone(),
            });
        }
    }

    if let Some(unknown) = wanted.iter().find(|p| !seen.contains(p)) {
        bail!(
            "{} is not part of any duplicate group in this scan",
            unknown.display()
        );
    }

    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::FileInfo;
    use std::time::SystemTime;

    fn info(path: &str) -> FileInfo {
        FileInfo {
            path: PathBuf::from(path),
            size: 10,
            quick_hash: None,
            hash: Some("h".into()),
            modified: SystemTime::UNIX_EPOCH,
            depth: 2,
        }
    }

    fn group() -> DuplicateGroup {
        DuplicateGroup::new("h".into(), vec![info("/g/a"), info("/g/b"), info("/g/c")])
    }

    #[test]
    fn plan_allows_partial_deletion() {
        let wanted: HashSet<PathBuf> = [PathBuf::from("/g/a"), PathBuf::from("/g/b")].into();
        let plan = plan_deletions(&[group()], &wanted).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.bytes(), 20);
    }

    #[test]
    fn plan_refuses_to_delete_last_copy() {
        let wanted: HashSet<PathBuf> =
            [PathBuf::from("/g/a"), PathBuf::from("/g/b"), PathBuf::from("/g/c")].into();
        let err = plan_deletions(&[group()], &wanted).unwrap_err();
        assert!(err.to_string().contains("every copy"));
    }

    #[test]
    fn plan_refuses_unknown_paths() {
        let wanted: HashSet<PathBuf> = [PathBuf::from("/etc/passwd")].into();
        let err = plan_deletions(&[group()], &wanted).unwrap_err();
        assert!(err.to_string().contains("not part of any duplicate group"));
    }

    #[test]
    fn permanent_delete_checks_size() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("f.txt");
        fs::write(&p, b"12345").unwrap();
        let mut d = Deleter::new(DeleteMethod::Permanent, None);
        assert!(d.delete_one(&p, Some(4)).is_err());
        assert!(p.exists());
        d.delete_one(&p, Some(5)).unwrap();
        assert!(!p.exists());
    }
}
