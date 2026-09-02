//! Backup store used by the `--delete-method backup` path and the
//! `dupscanner restore` command.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupRecord {
    pub original_path: PathBuf,
    pub backup_path: PathBuf,
    pub size: u64,
    pub timestamp: chrono::DateTime<Utc>,
}

pub struct BackupManager {
    backup_dir: PathBuf,
    records: Vec<BackupRecord>,
}

/// Default backup directory: `<local data dir>/dupscanner/backups`.
pub fn default_backup_dir() -> Result<PathBuf> {
    Ok(crate::paths::data_dir()?.join("backups"))
}

impl BackupManager {
    /// Open (creating if needed) the default backup store and load its index.
    pub fn open_default() -> Result<Self> {
        Self::open(default_backup_dir()?)
    }

    /// Open (creating if needed) a backup store at `backup_dir`.
    pub fn open(backup_dir: PathBuf) -> Result<Self> {
        fs::create_dir_all(&backup_dir)
            .with_context(|| format!("Failed to create backup directory {}", backup_dir.display()))?;
        let mut manager = BackupManager {
            backup_dir,
            records: Vec::new(),
        };
        manager.load_records()?;
        Ok(manager)
    }

    pub fn backup_dir(&self) -> &Path {
        &self.backup_dir
    }

    /// Copy `file_path` into the store and record it. Returns the backup path.
    pub fn backup_file(&mut self, file_path: &Path) -> Result<PathBuf> {
        let metadata = fs::metadata(file_path)
            .with_context(|| format!("File does not exist: {}", file_path.display()))?;
        if !metadata.is_file() {
            bail!("{} is not a regular file", file_path.display());
        }
        let size = metadata.len();
        let timestamp = Utc::now();

        let backup_path = self.unique_backup_path(file_path, &timestamp);

        let copied = fs::copy(file_path, &backup_path)
            .with_context(|| format!("Failed to copy {} to backup", file_path.display()))?;
        if copied != size {
            let _ = fs::remove_file(&backup_path);
            bail!(
                "Backup of {} is incomplete ({} of {} bytes)",
                file_path.display(),
                copied,
                size
            );
        }

        self.records.push(BackupRecord {
            original_path: file_path.to_path_buf(),
            backup_path: backup_path.clone(),
            size,
            timestamp,
        });
        self.save_records()?;

        Ok(backup_path)
    }

    /// Back up, then remove the original.
    pub fn delete_with_backup(&mut self, file_path: &Path) -> Result<PathBuf> {
        let backup_path = self.backup_file(file_path)?;
        fs::remove_file(file_path).context("Failed to delete file after backup")?;
        Ok(backup_path)
    }

    /// Restore a backup to its original location (or `restore_path`).
    /// Refuses to overwrite an existing file.
    pub fn restore_file(&mut self, backup_path: &Path, restore_path: Option<&Path>) -> Result<PathBuf> {
        let idx = self
            .records
            .iter()
            .position(|r| r.backup_path == backup_path)
            .context("Backup record not found")?;
        let target = restore_path
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.records[idx].original_path.clone());

        if target.exists() {
            bail!("Target file already exists: {}", target.display());
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(backup_path, &target).context("Failed to restore file")?;
        fs::remove_file(backup_path).ok();
        self.records.remove(idx);
        self.save_records()?;
        Ok(target)
    }

    pub fn list_backups(&self) -> &[BackupRecord] {
        &self.records
    }

    pub fn find_by_original(&self, original: &Path) -> Vec<&BackupRecord> {
        self.records
            .iter()
            .filter(|r| r.original_path == original)
            .collect()
    }

    /// Remove backups older than `days`. Returns how many were removed.
    pub fn clean_old_backups(&mut self, days: i64) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        let mut removed = 0usize;
        self.records.retain(|record| {
            if record.timestamp < cutoff {
                match fs::remove_file(&record.backup_path) {
                    Ok(()) => removed += 1,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => removed += 1,
                    Err(e) => eprintln!("Failed to remove old backup {}: {e}", record.backup_path.display()),
                }
                false
            } else {
                true
            }
        });
        self.save_records()?;
        Ok(removed)
    }

    pub fn total_backup_size(&self) -> u64 {
        self.records.iter().map(|r| r.size).sum()
    }

    fn unique_backup_path(&self, file_path: &Path, timestamp: &chrono::DateTime<Utc>) -> PathBuf {
        let filename = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let name = truncate_chars(&filename, 120);

        // Short digest of the original path plus time makes names unique
        // even for same-named files deleted in the same second.
        let mut hasher = Sha256::new();
        hasher.update(file_path.to_string_lossy().as_bytes());
        hasher.update(timestamp.timestamp_nanos_opt().unwrap_or(0).to_le_bytes());
        let digest = format!("{:x}", hasher.finalize());

        let mut candidate = self.backup_dir.join(format!(
            "{}_{}_{}.bak",
            timestamp.format("%Y%m%d_%H%M%S"),
            &digest[..8],
            name
        ));
        let mut n = 1;
        while candidate.exists() {
            candidate = self.backup_dir.join(format!(
                "{}_{}_{}_{}.bak",
                timestamp.format("%Y%m%d_%H%M%S"),
                &digest[..8],
                n,
                name
            ));
            n += 1;
        }
        candidate
    }

    fn records_file(&self) -> PathBuf {
        self.backup_dir.join("records.json")
    }

    /// Write the index atomically (temp file + rename).
    fn save_records(&self) -> Result<()> {
        let final_path = self.records_file();
        let tmp_path = self.backup_dir.join(".records.json.tmp");
        let json = serde_json::to_string_pretty(&self.records)?;
        fs::write(&tmp_path, json).context("Failed to write backup index")?;
        fs::rename(&tmp_path, &final_path).context("Failed to replace backup index")?;
        Ok(())
    }

    fn load_records(&mut self) -> Result<()> {
        let records_file = self.records_file();
        if records_file.exists() {
            let json = fs::read_to_string(&records_file)?;
            self.records = serde_json::from_str(&json)
                .with_context(|| format!("Backup index {} is corrupt", records_file.display()))?;
        }
        Ok(())
    }
}

/// Truncate to at most `max` characters on a char boundary, keeping the
/// extension when there is one.
pub fn truncate_chars(name: &str, max: usize) -> String {
    if name.chars().count() <= max {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(pos) if pos > 0 && name.len() - pos <= 16 => (&name[..pos], &name[pos..]),
        _ => (name, ""),
    };
    let keep = max.saturating_sub(ext.chars().count() + 1).max(1);
    let mut out: String = stem.chars().take(keep).collect();
    out.push('…');
    out.push_str(ext);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn store() -> (TempDir, BackupManager) {
        let dir = TempDir::new().unwrap();
        let mgr = BackupManager::open(dir.path().join("backups")).unwrap();
        (dir, mgr)
    }

    #[test]
    fn backup_and_delete_records_file() {
        let (dir, mut mgr) = store();
        let test_file = dir.path().join("test.txt");
        fs::File::create(&test_file).unwrap().write_all(b"test content").unwrap();

        let backup_path = mgr.delete_with_backup(&test_file).unwrap();
        assert!(!test_file.exists());
        assert!(backup_path.exists());
        assert_eq!(mgr.list_backups().len(), 1);

        // Index survives a reopen.
        let reopened = BackupManager::open(dir.path().join("backups")).unwrap();
        assert_eq!(reopened.list_backups().len(), 1);
    }

    #[test]
    fn same_name_same_second_do_not_collide() {
        let (dir, mut mgr) = store();
        fs::create_dir_all(dir.path().join("a")).unwrap();
        fs::create_dir_all(dir.path().join("b")).unwrap();
        fs::write(dir.path().join("a/f.txt"), b"one").unwrap();
        fs::write(dir.path().join("b/f.txt"), b"two").unwrap();

        let p1 = mgr.backup_file(&dir.path().join("a/f.txt")).unwrap();
        let p2 = mgr.backup_file(&dir.path().join("b/f.txt")).unwrap();
        assert_ne!(p1, p2);
        assert_eq!(fs::read(&p1).unwrap(), b"one");
        assert_eq!(fs::read(&p2).unwrap(), b"two");
    }

    #[test]
    fn restore_puts_file_back() {
        let (dir, mut mgr) = store();
        let original = dir.path().join("doc.txt");
        fs::write(&original, b"restore me").unwrap();
        let backup = mgr.delete_with_backup(&original).unwrap();
        assert!(!original.exists());

        let restored = mgr.restore_file(&backup, None).unwrap();
        assert_eq!(restored, original);
        assert_eq!(fs::read(&original).unwrap(), b"restore me");
        assert!(mgr.list_backups().is_empty());
    }

    #[test]
    fn truncation_is_char_safe() {
        let name = "é".repeat(300) + ".txt";
        let t = truncate_chars(&name, 50);
        assert!(t.chars().count() <= 51);
        assert!(t.ends_with(".txt"));
    }
}
