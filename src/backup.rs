use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
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

impl BackupManager {
    pub fn new() -> Result<Self> {
        let backup_dir = dirs::data_local_dir()
            .context("Failed to get local data directory")?
            .join("dupscanner")
            .join("backups");

        if !backup_dir.exists() {
            fs::create_dir_all(&backup_dir)?;
        }

        Ok(BackupManager {
            backup_dir,
            records: Vec::new(),
        })
    }

    pub fn new_with_dir(custom_dir: PathBuf) -> Result<Self> {
        let backup_dir = custom_dir.join("backups");

        if !backup_dir.exists() {
            fs::create_dir_all(&backup_dir)?;
        }

        Ok(BackupManager {
            backup_dir,
            records: Vec::new(),
        })
    }

    pub fn backup_file(&mut self, file_path: &Path) -> Result<PathBuf> {
        if !file_path.exists() {
            anyhow::bail!("File does not exist: {}", file_path.display());
        }

        let metadata = fs::metadata(file_path)?;
        let size = metadata.len();

        // Create timestamped backup filename
        let timestamp = Utc::now();
        let filename = file_path
            .file_name()
            .context("Invalid filename")?
            .to_string_lossy();

        // Truncate filename if too long to prevent filesystem errors
        // Most filesystems limit filenames to 255 bytes
        // Reserve ~30 chars for timestamp and extension
        let max_filename_len = 200;
        let truncated_filename = if filename.len() > max_filename_len {
            // Keep the extension if present
            if let Some(ext_pos) = filename.rfind('.') {
                let name_part = &filename[..ext_pos];
                let ext_part = &filename[ext_pos..];
                let truncate_len = max_filename_len.saturating_sub(ext_part.len()).saturating_sub(3);
                format!("{}...{}", &name_part[..truncate_len.min(name_part.len())], ext_part)
            } else {
                format!("{}...", &filename[..max_filename_len - 3])
            }
        } else {
            filename.to_string()
        };

        let backup_filename = format!(
            "{}_{}.bak",
            timestamp.format("%Y%m%d_%H%M%S"),
            truncated_filename
        );

        let backup_path = self.backup_dir.join(backup_filename);

        // Copy file to backup location
        fs::copy(file_path, &backup_path)
            .context("Failed to create backup")?;

        // Record backup
        let record = BackupRecord {
            original_path: file_path.to_path_buf(),
            backup_path: backup_path.clone(),
            size,
            timestamp,
        };

        self.records.push(record);
        self.save_records()?;

        Ok(backup_path)
    }

    pub fn delete_with_backup(&mut self, file_path: &Path) -> Result<PathBuf> {
        let backup_path = self.backup_file(file_path)?;

        fs::remove_file(file_path)
            .context("Failed to delete file after backup")?;

        Ok(backup_path)
    }

    #[allow(dead_code)]
    pub fn restore_file(&self, backup_path: &Path, restore_path: Option<&Path>) -> Result<()> {
        let record = self
            .records
            .iter()
            .find(|r| r.backup_path == backup_path)
            .context("Backup record not found")?;

        let target_path = restore_path.unwrap_or(&record.original_path);

        // Check if target already exists
        if target_path.exists() {
            anyhow::bail!("Target file already exists: {}", target_path.display());
        }

        // Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }

        fs::copy(backup_path, target_path)
            .context("Failed to restore file")?;

        Ok(())
    }

    #[allow(dead_code)]
    pub fn list_backups(&self) -> &[BackupRecord] {
        &self.records
    }

    #[allow(dead_code)]
    pub fn clean_old_backups(&mut self, days: i64) -> Result<usize> {
        let cutoff = Utc::now() - chrono::Duration::days(days);
        let mut removed_count = 0;

        self.records.retain(|record| {
            if record.timestamp < cutoff {
                if let Err(e) = fs::remove_file(&record.backup_path) {
                    eprintln!("Failed to remove old backup: {}", e);
                } else {
                    removed_count += 1;
                }
                false
            } else {
                true
            }
        });

        self.save_records()?;
        Ok(removed_count)
    }

    #[allow(dead_code)]
    pub fn get_total_backup_size(&self) -> u64 {
        self.records.iter().map(|r| r.size).sum()
    }

    fn save_records(&self) -> Result<()> {
        let records_file = self.backup_dir.join("records.json");
        let json = serde_json::to_string_pretty(&self.records)?;
        fs::write(records_file, json)?;
        Ok(())
    }

    pub fn load_records(&mut self) -> Result<()> {
        let records_file = self.backup_dir.join("records.json");
        if records_file.exists() {
            let json = fs::read_to_string(records_file)?;
            self.records = serde_json::from_str(&json)?;
        }
        Ok(())
    }
}

impl Default for BackupManager {
    fn default() -> Self {
        Self::new().expect("Failed to create backup manager")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_backup_file() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");

        let mut file = fs::File::create(&test_file).unwrap();
        file.write_all(b"test content").unwrap();
        drop(file);

        let mut backup_mgr = BackupManager::new().unwrap();
        let backup_path = backup_mgr.backup_file(&test_file).unwrap();

        assert!(backup_path.exists());
        assert_eq!(backup_mgr.records.len(), 1);
    }

    #[test]
    fn test_delete_with_backup() {
        let temp_dir = TempDir::new().unwrap();
        let test_file = temp_dir.path().join("test.txt");

        let mut file = fs::File::create(&test_file).unwrap();
        file.write_all(b"test content").unwrap();
        drop(file);

        let mut backup_mgr = BackupManager::new().unwrap();
        let backup_path = backup_mgr.delete_with_backup(&test_file).unwrap();

        assert!(!test_file.exists());
        assert!(backup_path.exists());
    }
}
