//! SQLite persistence for finished scans.
//!
//! One database (see `paths::default_db_path`) holds every scan from every
//! mode. Only files that ended up in a duplicate group are stored.

use crate::deletion::DeletionReport;
use crate::duplicates::DuplicateGroup;
use crate::scanner::FileInfo;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct ScanDatabase {
    conn: Connection,
    db_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ScanInfo {
    pub id: i64,
    pub root_path: PathBuf,
    pub start_time: i64,
    pub end_time: Option<i64>,
    pub files_scanned: i64,
    pub groups_found: i64,
    /// Files removed during this scan, from the deletion journal.
    pub deleted_count: i64,
    pub deleted_bytes: i64,
}

impl ScanDatabase {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)
            .with_context(|| format!("Failed to open database {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "synchronous", "NORMAL").ok();
        Self::init_tables(&conn)?;
        Ok(ScanDatabase {
            conn,
            db_path: Some(path.to_path_buf()),
        })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::init_tables(&conn)?;
        Ok(ScanDatabase { conn, db_path: None })
    }

    pub fn db_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }

    fn init_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scans (
                id INTEGER PRIMARY KEY,
                root_path TEXT NOT NULL,
                start_time INTEGER NOT NULL,
                end_time INTEGER,
                files_scanned INTEGER DEFAULT 0,
                groups_found INTEGER DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL,
                path TEXT NOT NULL,
                size INTEGER NOT NULL,
                quick_hash TEXT,
                full_hash TEXT,
                group_id TEXT,
                modified INTEGER,
                depth INTEGER,
                FOREIGN KEY(scan_id) REFERENCES scans(id)
            );
            CREATE INDEX IF NOT EXISTS idx_scan_id ON files(scan_id);
            CREATE INDEX IF NOT EXISTS idx_group_id ON files(group_id);
            CREATE TABLE IF NOT EXISTS deletions (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER,
                root_path TEXT NOT NULL,
                path TEXT NOT NULL,
                size INTEGER NOT NULL,
                method TEXT NOT NULL,
                deleted_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_deletions_scan ON deletions(scan_id);",
        )?;

        // Older databases predate the modified/depth columns.
        for col in ["modified", "depth"] {
            let exists: bool = conn
                .prepare("SELECT 1 FROM pragma_table_info('files') WHERE name = ?1")?
                .exists([col])?;
            if !exists {
                conn.execute(&format!("ALTER TABLE files ADD COLUMN {col} INTEGER"), [])?;
            }
        }
        Ok(())
    }

    pub fn start_scan(&self, root_path: &Path) -> Result<i64> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "INSERT INTO scans (root_path, start_time) VALUES (?1, ?2)",
            params![root_path.to_string_lossy().to_string(), now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update counts for a scan that is still running or was interrupted,
    /// without marking it complete.
    pub fn update_progress(&self, scan_id: i64, files_scanned: usize, groups_found: usize) -> Result<()> {
        self.conn.execute(
            "UPDATE scans SET files_scanned = ?1, groups_found = ?2 WHERE id = ?3",
            params![files_scanned as i64, groups_found as i64, scan_id],
        )?;
        Ok(())
    }

    pub fn complete_scan(&self, scan_id: i64, files_scanned: usize, groups_found: usize) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        self.conn.execute(
            "UPDATE scans SET end_time = ?1, files_scanned = ?2, groups_found = ?3 WHERE id = ?4",
            params![now, files_scanned as i64, groups_found as i64, scan_id],
        )?;
        Ok(())
    }

    /// Replace the stored groups for a scan with `groups`, in one transaction.
    pub fn save_groups(&mut self, scan_id: i64, groups: &[DuplicateGroup]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM files WHERE scan_id = ?1", params![scan_id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO files (scan_id, path, size, quick_hash, full_hash, group_id, modified, depth)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            for group in groups {
                for file in &group.files {
                    stmt.execute(params![
                        scan_id,
                        file.path.to_string_lossy().to_string(),
                        file.size as i64,
                        file.quick_hash.as_deref(),
                        file.hash.as_deref(),
                        &group.hash,
                        system_time_to_secs(file.modified),
                        file.depth as i64,
                    ])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Record a finished scan (metadata plus groups) in one go.
    pub fn record_completed_scan(
        &mut self,
        root_path: &Path,
        files_scanned: usize,
        groups: &[DuplicateGroup],
    ) -> Result<i64> {
        let scan_id = self.start_scan(root_path)?;
        self.save_groups(scan_id, groups)?;
        self.complete_scan(scan_id, files_scanned, groups.len())?;
        Ok(scan_id)
    }

    /// Append every successful removal in `report` to the deletion journal.
    pub fn record_deletions(
        &mut self,
        scan_id: Option<i64>,
        root_path: &Path,
        method: &str,
        report: &DeletionReport,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO deletions (scan_id, root_path, path, size, method, deleted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for o in report.outcomes.iter().filter(|o| o.result.is_ok()) {
                stmt.execute(params![
                    scan_id,
                    root_path.to_string_lossy().to_string(),
                    o.path.to_string_lossy().to_string(),
                    o.size as i64,
                    method,
                    now,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Paths removed during `scan_id`, newest first.
    pub fn deletions_for_scan(&self, scan_id: i64) -> Result<Vec<(PathBuf, u64, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, size, method, deleted_at FROM deletions WHERE scan_id = ?1 ORDER BY id DESC",
        )?;
        let rows = stmt.query_map([scan_id], |row| {
            Ok((
                PathBuf::from(row.get::<_, String>(0)?),
                row.get::<_, i64>(1)?.max(0) as u64,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn get_scan_info(&self, scan_id: i64) -> Option<ScanInfo> {
        self.conn
            .query_row(
                &format!("{SCAN_INFO_SELECT} WHERE s.id = ?1"),
                params![scan_id],
                row_to_scan_info,
            )
            .ok()
    }

    pub fn list_scans(&self) -> Result<Vec<ScanInfo>> {
        let mut stmt = self
            .conn
            .prepare(&format!("{SCAN_INFO_SELECT} ORDER BY s.start_time DESC, s.id DESC"))?;
        let rows = stmt.query_map([], row_to_scan_info)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn delete_scan(&self, scan_id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM deletions WHERE scan_id = ?1", params![scan_id])?;
        self.conn.execute("DELETE FROM files WHERE scan_id = ?1", params![scan_id])?;
        self.conn.execute("DELETE FROM scans WHERE id = ?1", params![scan_id])?;
        Ok(())
    }

    pub fn load_duplicate_groups(&self, scan_id: i64) -> Result<Vec<DuplicateGroup>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, size, quick_hash, full_hash, group_id, modified, depth
             FROM files WHERE scan_id = ?1 AND group_id IS NOT NULL ORDER BY group_id, path",
        )?;
        let rows = stmt.query_map([scan_id], |row| {
            let group_id: String = row.get(4)?;
            let modified: Option<i64> = row.get(5)?;
            let depth: Option<i64> = row.get(6)?;
            Ok((
                group_id,
                FileInfo {
                    path: PathBuf::from(row.get::<_, String>(0)?),
                    size: row.get::<_, i64>(1)? as u64,
                    quick_hash: row.get(2)?,
                    hash: row.get(3)?,
                    modified: modified
                        .map(|s| UNIX_EPOCH + Duration::from_secs(s.max(0) as u64))
                        .unwrap_or(UNIX_EPOCH),
                    depth: depth.map(|d| d.max(0) as usize).unwrap_or(0),
                },
            ))
        })?;

        let mut groups: Vec<DuplicateGroup> = Vec::new();
        let mut current: Option<(String, Vec<FileInfo>)> = None;
        for r in rows {
            let (gid, file) = r?;
            match &mut current {
                Some((h, files)) if *h == gid => files.push(file),
                _ => {
                    if let Some((h, files)) = current.take() {
                        groups.push(DuplicateGroup::new(h, files));
                    }
                    current = Some((gid, vec![file]));
                }
            }
        }
        if let Some((h, files)) = current.take() {
            groups.push(DuplicateGroup::new(h, files));
        }
        groups.sort_by(|a, b| b.wasted_space.cmp(&a.wasted_space));
        Ok(groups)
    }
}

const SCAN_INFO_SELECT: &str = "SELECT s.id, s.root_path, s.start_time, s.end_time, s.files_scanned, s.groups_found,
       (SELECT COUNT(*) FROM deletions d WHERE d.scan_id = s.id),
       (SELECT COALESCE(SUM(size), 0) FROM deletions d WHERE d.scan_id = s.id)
     FROM scans s";

fn row_to_scan_info(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanInfo> {
    Ok(ScanInfo {
        id: row.get(0)?,
        root_path: PathBuf::from(row.get::<_, String>(1)?),
        start_time: row.get(2)?,
        end_time: row.get(3)?,
        files_scanned: row.get(4)?,
        groups_found: row.get(5)?,
        deleted_count: row.get(6)?,
        deleted_bytes: row.get(7)?,
    })
}

fn system_time_to_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(path: &str, size: u64) -> FileInfo {
        FileInfo {
            path: PathBuf::from(path),
            size,
            quick_hash: Some("q".into()),
            hash: Some("h".into()),
            modified: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            depth: 3,
        }
    }

    #[test]
    fn round_trip_groups() {
        let mut db = ScanDatabase::open_in_memory().unwrap();
        let groups = vec![DuplicateGroup::new(
            "h".into(),
            vec![info("/x/a", 10), info("/x/b", 10)],
        )];
        let id = db.record_completed_scan(Path::new("/x"), 5, &groups).unwrap();

        let info = db.get_scan_info(id).unwrap();
        assert_eq!(info.files_scanned, 5);
        assert_eq!(info.groups_found, 1);

        let loaded = db.load_duplicate_groups(id).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].file_count(), 2);
        assert_eq!(loaded[0].files[0].depth, 3);
        assert_eq!(
            loaded[0].files[0].modified,
            UNIX_EPOCH + Duration::from_secs(1_700_000_000)
        );
    }

    #[test]
    fn deletion_journal_round_trip() {
        use crate::deletion::DeletionOutcome;
        let mut db = ScanDatabase::open_in_memory().unwrap();
        let id = db.start_scan(Path::new("/x")).unwrap();
        let report = DeletionReport {
            outcomes: vec![
                DeletionOutcome { path: PathBuf::from("/x/a"), size: 5, result: Ok(()) },
                DeletionOutcome { path: PathBuf::from("/x/b"), size: 7, result: Err("nope".into()) },
            ],
        };
        db.record_deletions(Some(id), Path::new("/x"), "trash", &report).unwrap();
        let rows = db.deletions_for_scan(id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, PathBuf::from("/x/a"));
        let info = db.get_scan_info(id).unwrap();
        assert_eq!(info.deleted_count, 1);
        assert_eq!(info.deleted_bytes, 5);
    }

    #[test]
    fn save_groups_replaces_previous_rows() {
        let mut db = ScanDatabase::open_in_memory().unwrap();
        let id = db.start_scan(Path::new("/x")).unwrap();
        db.save_groups(id, &[DuplicateGroup::new("h".into(), vec![info("/x/a", 1), info("/x/b", 1)])])
            .unwrap();
        db.save_groups(id, &[]).unwrap();
        assert!(db.load_duplicate_groups(id).unwrap().is_empty());
    }
}
