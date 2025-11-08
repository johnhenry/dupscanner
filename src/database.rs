use anyhow::Result;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use crate::scanner::FileInfo;
use crate::duplicates::DuplicateGroup;

#[allow(dead_code)]
pub struct ScanDatabase {
    conn: Connection,
}

impl ScanDatabase {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS scans (
                id INTEGER PRIMARY KEY,
                root_path TEXT NOT NULL,
                start_time INTEGER NOT NULL,
                end_time INTEGER,
                files_scanned INTEGER DEFAULT 0,
                groups_found INTEGER DEFAULT 0
            )",
            [],
        )?;

        conn.execute(
            "CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL,
                path TEXT NOT NULL,
                size INTEGER NOT NULL,
                quick_hash TEXT,
                full_hash TEXT,
                group_id TEXT,
                FOREIGN KEY(scan_id) REFERENCES scans(id)
            )",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_scan_id ON files(scan_id)",
            [],
        )?;

        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_group_id ON files(group_id)",
            [],
        )?;

        Ok(ScanDatabase { conn })
    }

    pub fn start_scan(&self, root_path: &Path) -> Result<i64> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO scans (root_path, start_time) VALUES (?1, ?2)",
            params![root_path.to_string_lossy().to_string(), now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn complete_scan(&self, scan_id: i64, files_scanned: usize, groups_found: usize) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "UPDATE scans SET end_time = ?1, files_scanned = ?2, groups_found = ?3 WHERE id = ?4",
            params![now, files_scanned as i64, groups_found as i64, scan_id],
        )?;
        Ok(())
    }

    pub fn save_duplicate_group(&self, scan_id: i64, group: &DuplicateGroup) -> Result<()> {
        for file in &group.files {
            self.conn.execute(
                "INSERT INTO files (scan_id, path, size, quick_hash, full_hash, group_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    scan_id,
                    file.path.to_string_lossy().to_string(),
                    file.size as i64,
                    file.quick_hash.as_ref().map(|s| s.as_str()),
                    file.hash.as_ref().map(|s| s.as_str()),
                    &group.hash,
                ],
            )?;
        }
        Ok(())
    }

    pub fn list_scans(&self) -> Result<Vec<ScanInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, root_path, start_time, end_time, files_scanned, groups_found
             FROM scans ORDER BY start_time DESC"
        )?;

        let scans = stmt.query_map([], |row| {
            Ok(ScanInfo {
                id: row.get(0)?,
                root_path: PathBuf::from(row.get::<_, String>(1)?),
                start_time: row.get(2)?,
                end_time: row.get(3)?,
                files_scanned: row.get(4)?,
                groups_found: row.get(5)?,
            })
        })?;

        let mut result = Vec::new();
        for scan in scans {
            result.push(scan?);
        }
        Ok(result)
    }

    pub fn load_duplicate_groups(&self, scan_id: i64) -> Result<Vec<DuplicateGroup>> {
        let mut stmt = self.conn.prepare(
            "SELECT group_id FROM files WHERE scan_id = ?1 AND group_id IS NOT NULL GROUP BY group_id"
        )?;

        let group_ids = stmt.query_map([scan_id], |row| row.get::<_, String>(0))?;

        let mut groups = Vec::new();
        for group_id in group_ids {
            let group_id = group_id?;
            groups.push(self.load_group(scan_id, &group_id)?);
        }

        Ok(groups)
    }

    fn load_group(&self, scan_id: i64, group_id: &str) -> Result<DuplicateGroup> {
        let mut stmt = self.conn.prepare(
            "SELECT path, size, quick_hash, full_hash FROM files
             WHERE scan_id = ?1 AND group_id = ?2"
        )?;

        let files = stmt.query_map(params![scan_id, group_id], |row| {
            Ok(FileInfo {
                path: PathBuf::from(row.get::<_, String>(0)?),
                size: row.get::<_, i64>(1)? as u64,
                quick_hash: row.get(2)?,
                hash: row.get(3)?,
                modified: std::time::SystemTime::now(), // Placeholder
                depth: 0, // Placeholder
            })
        })?;

        let mut file_vec = Vec::new();
        for file in files {
            file_vec.push(file?);
        }

        Ok(DuplicateGroup::new(group_id.to_string(), file_vec))
    }
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct ScanInfo {
    pub id: i64,
    pub root_path: PathBuf,
    pub start_time: i64,
    pub end_time: Option<i64>,
    pub files_scanned: i64,
    pub groups_found: i64,
}
