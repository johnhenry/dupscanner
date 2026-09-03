//! Interactive review state shared by the TUI.
//!
//! `App` drives a `ScanSession`, keeps the latest duplicate groups, tracks
//! which files the user has marked (by path, so marks survive re-sorting),
//! filters the visible groups with the shared `GroupFilter`, applies the
//! shared auto-select rules, replays mid-scan edits onto engine snapshots,
//! and funnels every removal through `deletion::plan_deletions` and a
//! `Deleter`, with an explicit confirmation step.

use crate::database::ScanDatabase;
use crate::deletion::{plan_deletions, Deleter, DeletionPlan, DeletionReport};
use crate::duplicates::DuplicateGroup;
use crate::edits::{self, ScanEdits};
use crate::engine::{EngineEvent, RemovedPaths, ScanSession};
use crate::filters::GroupFilter;
use crate::naming;
use crate::preview::Previewer;
use crate::scanner::{ScanConfig, ScanProgress};
use crate::selection::{self, SelectMode};
use crate::suggestions::{GroupAnalysis, SuggestionEngine};
use anyhow::Result;
use humansize::{format_size, BINARY};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Duplicates,
    Statistics,
    Help,
}

/// Which groups an action applies to. `AllGroups` means every group that
/// passes the current filter, matching the web UI's "all matching groups".
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

/// A text field the footer is currently editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// Path substring filter.
    Filter(String),
    /// New name for the selected file.
    Rename { path: PathBuf, text: String },
}

pub struct App {
    pub root_path: PathBuf,
    pub config: Option<ScanConfig>,
    session: Option<ScanSession>,
    removed: Option<RemovedPaths>,
    edits: ScanEdits,

    pub groups: Vec<DuplicateGroup>,
    /// Indices into `groups` that pass `filter`, in display order.
    pub visible: Vec<usize>,
    pub filter: GroupFilter,
    pub progress: ScanProgress,
    pub scan_complete: bool,
    pub elapsed: Option<Duration>,
    scan_started: Instant,

    /// Position within `visible`.
    pub current_group_index: usize,
    pub selected_file_index: usize,
    pub marked: HashSet<PathBuf>,

    pub deleter: Deleter,
    database: Option<ScanDatabase>,
    pub scan_id: Option<i64>,

    pub view_mode: ViewMode,
    pub status_message: Option<String>,
    pub pending_confirm: Option<PendingConfirm>,
    pub input: Option<Input>,
    pub show_mark_menu: bool,
    /// Image preview pane beside the file list (toggle with `v`).
    pub show_preview: bool,
    pub previewer: Previewer,
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
            edits: ScanEdits::default(),
            groups: Vec::new(),
            visible: Vec::new(),
            filter: GroupFilter::default(),
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
            input: None,
            show_mark_menu: false,
            show_preview: true,
            previewer: Previewer::new(),
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
        let mut app = App {
            root_path,
            config: None,
            session: None,
            removed: None,
            edits: ScanEdits::default(),
            groups,
            visible: Vec::new(),
            filter: GroupFilter::default(),
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
            input: None,
            show_mark_menu: false,
            show_preview: true,
            previewer: Previewer::new(),
            total_deleted: 0,
            total_freed: 0,
            last_report: None,
        };
        app.refresh_visible();
        app
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
                EngineEvent::Groups(groups) => self.adopt_snapshot(groups),
                EngineEvent::Complete {
                    finder,
                    progress,
                    elapsed,
                } => {
                    self.adopt_snapshot(finder.groups().to_vec());
                    self.progress = progress;
                    self.elapsed = Some(elapsed);
                    self.scan_complete = true;
                    self.edits.clear();
                    self.record_completed_scan();
                    self.session = None;
                    self.removed = None;
                    return;
                }
            }
        }
    }

    /// Replace the groups with a snapshot from the engine, replaying the
    /// deletions and renames made through this UI while the scan ran.
    fn adopt_snapshot(&mut self, mut groups: Vec<DuplicateGroup>) {
        self.edits.apply(&mut groups);
        self.groups = groups;
        self.refresh_visible();
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

    pub fn db_path(&self) -> Option<&Path> {
        self.database.as_ref().and_then(|d| d.db_path())
    }

    // ----- filtering ----------------------------------------------------

    /// Recompute `visible` after the groups or the filter changed, keeping
    /// the selection on the same group when it is still visible.
    pub fn refresh_visible(&mut self) {
        let previously_selected = self.visible.get(self.current_group_index).copied();
        self.groups.retain(|g| !g.is_empty());
        self.visible = self.filter.apply(&self.groups);
        if let Some(gi) = previously_selected {
            if let Some(pos) = self.visible.iter().position(|&v| v == gi) {
                self.current_group_index = pos;
            }
        }
        self.clamp_selection();
    }

    pub fn set_path_filter(&mut self, text: String) {
        self.filter.path = text;
        self.refresh_visible();
        self.status_message = Some(format!("Filter: {}", self.filter.describe()));
    }

    pub fn cycle_size_filter(&mut self) {
        self.filter.size = self.filter.size.next();
        self.refresh_visible();
        self.status_message = Some(format!("Filter: {}", self.filter.describe()));
    }

    pub fn cycle_kind_filter(&mut self) {
        self.filter.kind = self.filter.kind.next();
        self.refresh_visible();
        self.status_message = Some(format!("Filter: {}", self.filter.describe()));
    }

    pub fn clear_filter(&mut self) {
        self.filter = GroupFilter::default();
        self.refresh_visible();
        self.status_message = Some("Filter cleared".into());
    }

    // ----- navigation -------------------------------------------------

    pub fn current_group_id(&self) -> Option<usize> {
        self.visible.get(self.current_group_index).copied()
    }

    pub fn current_group(&self) -> Option<&DuplicateGroup> {
        self.current_group_id().map(|gi| &self.groups[gi])
    }

    pub fn current_analysis(&self) -> Option<GroupAnalysis> {
        self.current_group().map(|g| SuggestionEngine::analyze(&g.files))
    }

    pub fn selected_file(&self) -> Option<&crate::scanner::FileInfo> {
        self.current_group().and_then(|g| g.files.get(self.selected_file_index))
    }

    fn clamp_selection(&mut self) {
        if self.visible.is_empty() {
            self.current_group_index = 0;
            self.selected_file_index = 0;
            return;
        }
        if self.current_group_index >= self.visible.len() {
            self.current_group_index = self.visible.len() - 1;
        }
        let n = self.groups[self.visible[self.current_group_index]].files.len();
        if self.selected_file_index >= n {
            self.selected_file_index = n.saturating_sub(1);
        }
    }

    pub fn next_group(&mut self) {
        if self.current_group_index + 1 < self.visible.len() {
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
        self.current_group_index = self.visible.len().saturating_sub(1);
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

    pub fn is_marked(&self, path: &Path) -> bool {
        self.marked.contains(path)
    }

    pub fn toggle_mark(&mut self) {
        let Some(path) = self.selected_file().map(|f| f.path.clone()) else { return };
        if !self.marked.remove(&path) {
            self.marked.insert(path);
        }
    }

    fn scope_group_ids(&self, scope: Scope) -> Vec<usize> {
        match scope {
            Scope::CurrentGroup => self.current_group_id().into_iter().collect(),
            Scope::AllGroups => self.visible.clone(),
        }
    }

    /// Apply one of the shared auto-select rules. The rule owns the marks
    /// of every group it touches (existing marks there are replaced).
    pub fn apply_select_mode(&mut self, mode: SelectMode, scope: Scope) {
        let ids = self.scope_group_ids(scope);
        let mut marked = 0usize;
        let mut touched = 0usize;
        for gi in &ids {
            let n = selection::apply_mode(&mut self.marked, &self.groups[*gi], mode);
            marked += n;
            if n > 0 {
                touched += 1;
            }
        }
        self.status_message = Some(match (mode, scope) {
            (SelectMode::Suggested, Scope::CurrentGroup) if marked == 0 => {
                "No file in this group looks like a copy. Use 'o' to mark all but the keeper, or Space.".to_string()
            }
            (SelectMode::Suggested, Scope::CurrentGroup) => {
                let conf = self.current_analysis().map(|a| a.confidence()).unwrap_or("");
                format!("Marked {marked} suggested file(s) ({conf})")
            }
            (SelectMode::AllButKeeper, Scope::CurrentGroup) => {
                let keeper = self
                    .current_analysis()
                    .and_then(|a| a.keeper)
                    .and_then(|k| self.current_group().and_then(|g| g.files.get(k)))
                    .and_then(|f| f.path.file_name().map(|n| n.to_string_lossy().to_string()))
                    .unwrap_or_default();
                format!("Marked {marked} file(s), keeping {keeper}")
            }
            (_, Scope::CurrentGroup) => format!("{}: marked {marked} file(s)", mode.label()),
            (_, Scope::AllGroups) => {
                let scope_text = if self.filter.is_active() { "matching" } else { "all" };
                format!("{}: marked {marked} file(s) in {touched} {scope_text} group(s)", mode.label())
            }
        });
    }

    pub fn mark_suggested(&mut self, scope: Scope) {
        self.apply_select_mode(SelectMode::Suggested, scope);
    }

    pub fn mark_all_but_keeper(&mut self, scope: Scope) {
        self.apply_select_mode(SelectMode::AllButKeeper, scope);
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

    // ----- rename / open ------------------------------------------------

    pub fn start_rename(&mut self) {
        let Some(file) = self.selected_file() else { return };
        let name = file.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        self.input = Some(Input::Rename {
            path: file.path.clone(),
            text: name,
        });
    }

    pub fn start_filter_input(&mut self) {
        self.input = Some(Input::Filter(self.filter.path.clone()));
    }

    pub fn cancel_input(&mut self) {
        self.input = None;
    }

    pub fn input_push(&mut self, c: char) {
        match &mut self.input {
            Some(Input::Filter(t)) | Some(Input::Rename { text: t, .. }) => t.push(c),
            None => {}
        }
    }

    pub fn input_pop(&mut self) {
        match &mut self.input {
            Some(Input::Filter(t)) | Some(Input::Rename { text: t, .. }) => {
                t.pop();
            }
            None => {}
        }
    }

    /// Enter pressed while editing.
    pub fn commit_input(&mut self) {
        match self.input.take() {
            Some(Input::Filter(text)) => self.set_path_filter(text),
            Some(Input::Rename { path, text }) => self.rename(&path, &text),
            None => {}
        }
    }

    fn rename(&mut self, old: &Path, new_name: &str) {
        match edits::rename_in_groups(&mut self.groups, old, new_name) {
            Ok((new, _)) => {
                self.previewer.forget(old);
                if self.marked.remove(old) {
                    self.marked.insert(new.clone());
                }
                if self.is_scanning() {
                    self.edits.record_rename(old, &new);
                }
                self.refresh_visible();
                if self.scan_complete {
                    self.persist_groups();
                }
                self.status_message = Some(format!("Renamed to {}", new.display()));
            }
            Err(e) => self.status_message = Some(format!("Not renamed: {e}")),
        }
    }

    /// Rename the keeper of the current group to the group's canonical name
    /// (the name with copy markers removed), when that name is free.
    pub fn rename_keeper_to_canonical(&mut self) {
        let Some(group) = self.current_group() else { return };
        let Some(keeper) = SuggestionEngine::analyze(&group.files).keeper else { return };
        match naming::suggested_rename(group, keeper) {
            Some((path, new_name)) => self.rename(&path, &new_name),
            None => {
                let msg = match naming::canonical_name(group) {
                    Some(c) if c.existing.is_some() => {
                        format!("A copy already has the original name {}; keep that one", c.name)
                    }
                    Some(c) => format!("{} is taken in the keeper's folder", c.name),
                    None => "No copy markers in this group's names, so there is no name to restore".to_string(),
                };
                self.status_message = Some(msg);
            }
        }
    }

    /// Open the selected file with the system's default application.
    pub fn open_selected(&mut self) {
        let Some(path) = self.selected_file().map(|f| f.path.clone()) else { return };
        match open::that_detached(&path) {
            Ok(()) => self.status_message = Some(format!("Opened {}", path.display())),
            Err(e) => self.status_message = Some(format!("Could not open {}: {e}", path.display())),
        }
    }

    // ----- deletion ---------------------------------------------------

    /// Validate the marked files in `scope` and ask for confirmation.
    pub fn request_delete(&mut self, scope: Scope) {
        let ids = self.scope_group_ids(scope);
        let groups: Vec<DuplicateGroup> = ids.iter().map(|&gi| self.groups[gi].clone()).collect();
        let wanted: HashSet<PathBuf> = groups
            .iter()
            .flat_map(|g| g.files.iter())
            .filter(|f| self.marked.contains(&f.path))
            .map(|f| f.path.clone())
            .collect();
        if wanted.is_empty() {
            self.status_message = Some("Nothing marked. Space marks a file, 'a' marks suggested copies.".into());
            return;
        }
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
        for p in &deleted {
            self.marked.remove(p);
            self.previewer.forget(p);
        }
        if self.is_scanning() {
            if let Some(removed) = &self.removed {
                removed.add_all(deleted.iter().cloned());
            }
            self.edits.record_deleted(deleted.iter().cloned());
        }
        self.refresh_visible();

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
                format_size(report.bytes_freed(), BINARY)
            )
        });
        if let Some(db) = &mut self.database {
            let _ = db.record_deletions(self.scan_id, &self.root_path, method, &report);
        }
        self.last_report = Some(report);
        if self.scan_complete {
            self.persist_groups();
        }
        Ok(())
    }

    // ----- misc -------------------------------------------------------

    pub fn toggle_preview(&mut self) {
        self.show_preview = !self.show_preview;
        self.status_message = Some(if self.show_preview {
            "Preview pane shown".into()
        } else {
            "Preview pane hidden".into()
        });
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deletion::DeleteMethod;
    use crate::scanner::FileInfo;
    use std::fs;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn info(path: &Path) -> FileInfo {
        FileInfo {
            path: path.to_path_buf(),
            size: 4,
            quick_hash: None,
            hash: Some("h".into()),
            modified: SystemTime::UNIX_EPOCH,
            depth: path.components().count(),
        }
    }

    fn app_with(dir: &Path, groups: Vec<DuplicateGroup>) -> App {
        App::from_recorded(
            dir.to_path_buf(),
            groups,
            10,
            Deleter::new(DeleteMethod::Permanent, None),
            None,
            None,
        )
    }

    #[test]
    fn filter_narrows_visible_groups_and_scope() {
        let dir = TempDir::new().unwrap();
        let g1 = DuplicateGroup::new("1".into(), vec![info(&dir.path().join("a.jpg")), info(&dir.path().join("b.jpg"))]);
        let g2 = DuplicateGroup::new("2".into(), vec![info(&dir.path().join("c.txt")), info(&dir.path().join("d.txt"))]);
        let mut app = app_with(dir.path(), vec![g1, g2]);
        assert_eq!(app.visible.len(), 2);

        app.set_path_filter(".txt".into());
        assert_eq!(app.visible.len(), 1);
        assert!(app.current_group().unwrap().files[0].path.ends_with("c.txt"));

        app.mark_all_but_keeper(Scope::AllGroups);
        assert_eq!(app.marked.len(), 1, "AllGroups scope respects the filter");

        app.clear_filter();
        assert_eq!(app.visible.len(), 2);
    }

    #[test]
    fn auto_select_owns_marks_and_delete_requires_confirmation() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, b"same").unwrap();
        fs::write(&b, b"same").unwrap();
        let mut app = app_with(dir.path(), vec![DuplicateGroup::new("h".into(), vec![info(&a), info(&b)])]);

        app.toggle_mark(); // marks a (index 0)
        assert!(app.is_marked(&a));
        app.mark_all_but_keeper(Scope::CurrentGroup);
        assert_eq!(app.marked.len(), 1);
        let keeper = SuggestionEngine::analyze(&app.groups[0].files).keeper.unwrap();
        assert!(!app.is_marked(&app.groups[0].files[keeper].path));

        app.request_delete(Scope::CurrentGroup);
        assert!(app.pending_confirm.is_some());
        assert!(a.exists() && b.exists(), "nothing deleted before confirmation");
        app.confirm_delete().unwrap();
        assert_eq!(app.total_deleted, 1);
        assert!(app.groups.is_empty());
    }

    #[test]
    fn rename_moves_mark_and_updates_group() {
        let dir = TempDir::new().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, b"same").unwrap();
        fs::write(&b, b"same").unwrap();
        let mut app = app_with(dir.path(), vec![DuplicateGroup::new("h".into(), vec![info(&a), info(&b)])]);
        app.toggle_mark();
        app.start_rename();
        assert!(matches!(app.input, Some(Input::Rename { .. })));
        for _ in 0..5 {
            app.input_pop();
        }
        for c in "z.txt".chars() {
            app.input_push(c);
        }
        app.commit_input();
        let z = dir.path().join("z.txt");
        assert!(z.exists());
        assert!(app.is_marked(&z));
        assert!(app.groups[0].files.iter().any(|f| f.path == z));
    }
}
