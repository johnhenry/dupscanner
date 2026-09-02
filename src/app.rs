//! Interactive review state shared by the TUI.
//!
//! `App` drives a `ScanSession`, keeps the latest duplicate groups, tracks
//! which files the user has marked (by path, so marks survive re-sorting),
//! and funnels every removal through `deletion::plan_deletions` and a
//! `Deleter`, with an explicit confirmation step.

use crate::database::ScanDatabase;
use crate::deletion::{plan_deletions, Deleter, DeletionPlan, DeletionReport};
use crate::duplicates::DuplicateGroup;
use crate::engine::{EngineEvent, RemovedPaths, ScanSession};
use crate::scanner::{ScanConfig, ScanProgress};
use crate::suggestions::{GroupAnalysis, SuggestionEngine};
use anyhow::Result;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Duplicates,
    Statistics,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    CurrentGroup,
    AllGroups,
}

#[derive(Debug, Clone)]
pub struct PendingConfirm {
    pub plan: DeletionPlan,
    pub scope: Scope,
}

pub struct App {
    pub root_path: PathBuf,
    pub config: Option<ScanConfig>,
    session: Option<ScanSession>,
    removed: Option<RemovedPaths>,

    pub groups: Vec<DuplicateGroup>,
    pub progress: ScanProgress,
    pub scan_complete: bool,
    pub elapsed: Option<Duration>,
    scan_started: Instant,

    pub current_group_index: usize,
    pub selected_file_index: usize,
    pub marked: HashSet<PathBuf>,

    pub deleter: Deleter,
    database: Option<ScanDatabase>,
    pub scan_id: Option<i64>,

    pub view_mode: ViewMode,
    pub status_message: Option<String>,
    pub pending_confirm: Option<PendingConfirm>,
    pub total_deleted: usize,
    pub total_freed: u64,
    pub last_report: Option<DeletionReport>,
}

impl App {
    /// Start a fresh scan.
    pub fn new_scan(config: ScanConfig, deleter: Deleter, database: Option<ScanDatabase>) -> Self {
        let session = ScanSession::start(config.clone());
        let removed = session.removed_paths();
        App {
            root_path: config.root_path.clone(),
            config: Some(config),
            session: Some(session),
            removed: Some(removed),
            groups: Vec::new(),
            progress: ScanProgress::default(),
            scan_complete: false,
            elapsed: None,
            scan_started: Instant::now(),
            current_group_index: 0,
            selected_file_index: 0,
            marked: HashSet::new(),
            deleter,
            database,
            scan_id: None,
            view_mode: ViewMode::Duplicates,
            status_message: None,
            pending_confirm: None,
            total_deleted: 0,
            total_freed: 0,
            last_report: None,
        }
    }

    /// Review a previously recorded scan.
    pub fn from_recorded(
        root_path: PathBuf,
        groups: Vec<DuplicateGroup>,
        files_scanned: usize,
        deleter: Deleter,
        database: Option<ScanDatabase>,
        scan_id: Option<i64>,
    ) -> Self {
        let mut groups = groups;
        groups.retain(|g| !g.is_empty());
        groups.sort_by(|a, b| b.wasted_space.cmp(&a.wasted_space));
        App {
            root_path,
            config: None,
            session: None,
            removed: None,
            groups,
            progress: ScanProgress {
                scanned_count: files_scanned,
                ..ScanProgress::default()
            },
            scan_complete: true,
            elapsed: None,
            scan_started: Instant::now(),
            current_group_index: 0,
            selected_file_index: 0,
            marked: HashSet::new(),
            deleter,
            database,
            scan_id,
            view_mode: ViewMode::Duplicates,
            status_message: None,
            pending_confirm: None,
            total_deleted: 0,
            total_freed: 0,
            last_report: None,
        }
    }

    pub fn is_scanning(&self) -> bool {
        !self.scan_complete
    }

    pub fn scan_elapsed(&self) -> Duration {
        self.elapsed.unwrap_or_else(|| self.scan_started.elapsed())
    }

    /// Pull pending engine events. Cheap; call once per frame.
    pub fn tick(&mut self) {
        let events: Vec<EngineEvent> = {
            let Some(session) = &self.session else { return };
            let mut v = Vec::new();
            while let Some(ev) = session.try_next() {
                let done = matches!(ev, EngineEvent::Complete { .. });
                v.push(ev);
                if done || v.len() >= 32 {
                    break;
                }
            }
            v
        };
        for ev in events {
            match ev {
                EngineEvent::Progress(p) => self.progress = p,
                EngineEvent::Groups(groups) => {
                    self.groups = groups;
                    self.clamp_selection();
                }
                EngineEvent::Complete {
                    finder,
                    progress,
                    elapsed,
                } => {
                    self.groups = finder.groups().to_vec();
                    self.progress = progress;
                    self.elapsed = Some(elapsed);
                    self.scan_complete = true;
                    self.clamp_selection();
                    self.record_completed_scan();
                    self.session = None;
                    return;
                }
            }
        }
    }

    fn record_completed_scan(&mut self) {
        if let Some(db) = &mut self.database {
            match db.record_completed_scan(&self.root_path, self.progress.scanned_count, &self.groups) {
                Ok(id) => self.scan_id = Some(id),
                Err(e) => self.status_message = Some(format!("Could not save scan to database: {e}")),
            }
        }
    }

    fn persist_groups(&mut self) {
        if let (Some(db), Some(id)) = (&mut self.database, self.scan_id) {
            let _ = db.save_groups(id, &self.groups);
            let _ = db.complete_scan(id, self.progress.scanned_count, self.groups.len());
        }
    }

    pub fn db_path(&self) -> Option<&std::path::Path> {
        self.database.as_ref().and_then(|d| d.db_path())
    }

    // ----- navigation -------------------------------------------------

    pub fn current_group(&self) -> Option<&DuplicateGroup> {
        self.groups.get(self.current_group_index)
    }

    pub fn current_analysis(&self) -> Option<GroupAnalysis> {
        self.current_group().map(|g| SuggestionEngine::analyze(&g.files))
    }

    fn clamp_selection(&mut self) {
        if self.groups.is_empty() {
            self.current_group_index = 0;
            self.selected_file_index = 0;
            return;
        }
        if self.current_group_index >= self.groups.len() {
            self.current_group_index = self.groups.len() - 1;
        }
        let n = self.groups[self.current_group_index].files.len();
        if self.selected_file_index >= n {
            self.selected_file_index = n.saturating_sub(1);
        }
    }

    pub fn next_group(&mut self) {
        if self.current_group_index + 1 < self.groups.len() {
            self.current_group_index += 1;
            self.selected_file_index = 0;
        }
    }

    pub fn previous_group(&mut self) {
        if self.current_group_index > 0 {
            self.current_group_index -= 1;
            self.selected_file_index = 0;
        }
    }

    pub fn first_group(&mut self) {
        self.current_group_index = 0;
        self.selected_file_index = 0;
    }

    pub fn last_group(&mut self) {
        self.current_group_index = self.groups.len().saturating_sub(1);
        self.selected_file_index = 0;
    }

    pub fn select_next_file(&mut self) {
        if let Some(group) = self.current_group() {
            if self.selected_file_index + 1 < group.files.len() {
                self.selected_file_index += 1;
            }
        }
    }

    pub fn select_previous_file(&mut self) {
        self.selected_file_index = self.selected_file_index.saturating_sub(1);
    }

    // ----- marking ----------------------------------------------------

    pub fn is_marked(&self, path: &std::path::Path) -> bool {
        self.marked.contains(path)
    }

    pub fn toggle_mark(&mut self) {
        let Some(group) = self.current_group() else { return };
        let Some(file) = group.files.get(self.selected_file_index) else { return };
        let path = file.path.clone();
        if !self.marked.remove(&path) {
            self.marked.insert(path);
        }
    }

    fn scope_indices(&self, scope: Scope) -> Vec<usize> {
        match scope {
            Scope::CurrentGroup => self.current_group().map(|_| vec![self.current_group_index]).unwrap_or_default(),
            Scope::AllGroups => (0..self.groups.len()).collect(),
        }
    }

    /// Mark files the heuristics flag as copies (never the keeper).
    pub fn mark_suggested(&mut self, scope: Scope) {
        let mut marked = 0usize;
        let mut groups_touched = 0usize;
        for gi in self.scope_indices(scope) {
            let group = &self.groups[gi];
            let analysis = SuggestionEngine::analyze(&group.files);
            let mut any = false;
            for s in analysis.suggested_deletions() {
                if let Some(f) = group.files.get(s.file_index) {
                    if self.marked.insert(f.path.clone()) {
                        marked += 1;
                    }
                    any = true;
                }
            }
            if any {
                groups_touched += 1;
            }
        }
        self.status_message = Some(match scope {
            Scope::CurrentGroup => {
                if marked == 0 {
                    "No file in this group looks like a copy. Use 'o' to mark all but the keeper, or Space.".to_string()
                } else {
                    let conf = self.current_analysis().map(|a| a.confidence()).unwrap_or("");
                    format!("Marked {marked} suggested file(s) ({conf})")
                }
            }
            Scope::AllGroups => format!("Marked {marked} suggested file(s) in {groups_touched} group(s)"),
        });
    }

    /// Mark every file except the keeper.
    pub fn mark_all_but_keeper(&mut self, scope: Scope) {
        let mut marked = 0usize;
        for gi in self.scope_indices(scope) {
            let group = &self.groups[gi];
            let analysis = SuggestionEngine::analyze(&group.files);
            for s in analysis.all_but_keeper() {
                if let Some(f) = group.files.get(s.file_index) {
                    if self.marked.insert(f.path.clone()) {
                        marked += 1;
                    }
                }
            }
        }
        let keeper = self
            .current_analysis()
            .and_then(|a| a.keeper)
            .and_then(|k| self.current_group().and_then(|g| g.files.get(k)))
            .and_then(|f| f.path.file_name().map(|n| n.to_string_lossy().to_string()))
            .unwrap_or_default();
        self.status_message = Some(match scope {
            Scope::CurrentGroup => format!("Marked {marked} file(s), keeping {keeper}"),
            Scope::AllGroups => format!("Marked {marked} file(s) across all groups"),
        });
    }

    pub fn clear_marks(&mut self, scope: Scope) {
        match scope {
            Scope::CurrentGroup => {
                if let Some(group) = self.current_group() {
                    let paths: Vec<PathBuf> = group.files.iter().map(|f| f.path.clone()).collect();
                    for p in paths {
                        self.marked.remove(&p);
                    }
                }
                self.status_message = Some("Cleared marks in this group".into());
            }
            Scope::AllGroups => {
                self.marked.clear();
                self.status_message = Some("Cleared all marks".into());
            }
        }
    }

    pub fn marked_in_group(&self, group: &DuplicateGroup) -> usize {
        group.files.iter().filter(|f| self.marked.contains(&f.path)).count()
    }

    pub fn marked_bytes(&self) -> u64 {
        self.groups
            .iter()
            .flat_map(|g| g.files.iter())
            .filter(|f| self.marked.contains(&f.path))
            .map(|f| f.size)
            .sum()
    }

    // ----- deletion ---------------------------------------------------

    /// Validate the marked files in `scope` and ask for confirmation.
    pub fn request_delete(&mut self, scope: Scope) {
        let wanted: HashSet<PathBuf> = match scope {
            Scope::CurrentGroup => self
                .current_group()
                .map(|g| {
                    g.files
                        .iter()
                        .filter(|f| self.marked.contains(&f.path))
                        .map(|f| f.path.clone())
                        .collect()
                })
                .unwrap_or_default(),
            Scope::AllGroups => self.marked.clone(),
        };
        if wanted.is_empty() {
            self.status_message = Some("Nothing marked. Space marks a file, 'a' marks suggested copies.".into());
            return;
        }

        let groups: Vec<DuplicateGroup> = match scope {
            Scope::CurrentGroup => self.current_group().cloned().into_iter().collect(),
            Scope::AllGroups => self.groups.clone(),
        };
        match plan_deletions(&groups, &wanted) {
            Ok(plan) => self.pending_confirm = Some(PendingConfirm { plan, scope }),
            Err(e) => self.status_message = Some(format!("Not deleting: {e}")),
        }
    }

    pub fn cancel_delete(&mut self) {
        self.pending_confirm = None;
        self.status_message = Some("Deletion cancelled".into());
    }

    pub fn confirm_delete(&mut self) -> Result<()> {
        let Some(pending) = self.pending_confirm.take() else { return Ok(()) };
        let report = self.deleter.delete_planned(&pending.plan);
        let deleted = report.deleted_paths();

        for group in &mut self.groups {
            group.remove_paths(&deleted);
        }
        self.groups.retain(|g| !g.is_empty());
        for p in &deleted {
            self.marked.remove(p);
        }
        if let Some(removed) = &self.removed {
            removed.add_all(deleted.iter().cloned());
        }
        self.clamp_selection();

        self.total_deleted += report.deleted_count();
        self.total_freed += report.bytes_freed();
        let method = self.deleter.method().label();
        self.status_message = Some(if report.failed_count() > 0 {
            format!(
                "Deleted {} file(s) via {method}, {} failed: {}",
                report.deleted_count(),
                report.failed_count(),
                report
                    .failures()
                    .next()
                    .map(|(_, e)| e.to_string())
                    .unwrap_or_default()
            )
        } else {
            format!(
                "Deleted {} file(s) via {method}, freed {}",
                report.deleted_count(),
                humansize::format_size(report.bytes_freed(), humansize::BINARY)
            )
        });
        self.last_report = Some(report);
        if self.scan_complete {
            self.persist_groups();
        }
        Ok(())
    }

    // ----- misc -------------------------------------------------------

    pub fn cycle_view(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Duplicates => ViewMode::Statistics,
            ViewMode::Statistics => ViewMode::Help,
            ViewMode::Help => ViewMode::Duplicates,
        };
    }

    pub fn total_wasted(&self) -> u64 {
        self.groups.iter().map(|g| g.wasted_space).sum()
    }

    pub fn total_duplicate_files(&self) -> usize {
        self.groups.iter().map(|g| g.file_count()).sum()
    }

    pub fn clear_status(&mut self) {
        self.status_message = None;
    }
}
