use crate::backup::BackupManager;
use crate::database::ScanDatabase;
use crate::duplicates::{DuplicateFinder, DuplicateGroup};
use crate::scanner::{FileInfo, ScanConfig, ScanProgress, Scanner};
use crate::state::{get_default_state_file, ScanState};
use crate::suggestions::SuggestionEngine;
use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Scanning,
    ReviewingDuplicates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Duplicates,
    Statistics,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageLocation {
    PerDirectory(PathBuf),  // .dupscanner directory in scan root
    InMemory,                // Fallback when write permissions fail
    #[allow(dead_code)]
    Global(PathBuf),         // Global directory (legacy, reserved for future use)
}

pub struct App {
    pub config: ScanConfig,
    pub scanner: Option<Scanner>,
    pub finder: DuplicateFinder,
    pub backup_manager: BackupManager,
    pub database: Option<ScanDatabase>,
    pub scan_id: Option<i64>,
    pub state: AppState,
    pub current_group_index: usize,
    pub scanned_count: usize,
    pub total_size: u64,
    pub paused: bool,
    pub scan_complete: bool,
    pub selected_file_index: usize,
    pub marked_for_deletion: Vec<bool>,
    pub marked_for_deletion_all_groups: HashMap<usize, Vec<bool>>, // Store markings across all groups
    pub show_help: bool,
    pub status_message: Option<String>,
    pub view_mode: ViewMode,
    // Streaming mode
    pub file_receiver: Option<Receiver<Vec<FileInfo>>>,
    pub progress_receiver: Option<Receiver<ScanProgress>>,
    pub streaming_mode: bool,
    // Storage location
    pub storage_location: StorageLocation,
}

fn get_dupscanner_dir(scan_root: &std::path::Path) -> Option<PathBuf> {
    let dupscanner_dir = scan_root.join(".dupscanner");

    // Try to create the directory
    if std::fs::create_dir_all(&dupscanner_dir).is_ok() {
        // Verify we can write to it
        let test_file = dupscanner_dir.join(".write_test");
        if std::fs::write(&test_file, b"test").is_ok() {
            let _ = std::fs::remove_file(&test_file);
            return Some(dupscanner_dir);
        }
    }

    None
}

impl App {
    pub fn new(config: ScanConfig) -> Self {
        let scanner = Some(Scanner::new(config.clone()));

        // Try to get per-directory .dupscanner location
        let dupscanner_dir = get_dupscanner_dir(&config.root_path);

        let (backup_manager, database, scan_id, storage_location) = if let Some(ref dir) = dupscanner_dir {
            // Per-directory storage
            let mut backup_manager = BackupManager::new_with_dir(dir.clone()).unwrap_or_default();
            let _ = backup_manager.load_records();

            let db_path = dir.join("scans.db");
            let database = ScanDatabase::open_or_in_memory(&db_path);
            let scan_id = database.start_scan(&config.root_path).ok();

            let storage_location = if database.is_in_memory() {
                StorageLocation::InMemory
            } else {
                StorageLocation::PerDirectory(dir.clone())
            };

            (backup_manager, Some(database), scan_id, storage_location)
        } else {
            // Fallback to in-memory only (no backup support without write permissions)
            let backup_manager = BackupManager::new().unwrap_or_default();
            let database = ScanDatabase::open_in_memory().ok();
            let scan_id = database.as_ref().and_then(|db| db.start_scan(&config.root_path).ok());

            (backup_manager, database, scan_id, StorageLocation::InMemory)
        };

        App {
            config,
            scanner,
            finder: DuplicateFinder::new(),
            backup_manager,
            database,
            scan_id,
            state: AppState::Scanning,
            current_group_index: 0,
            scanned_count: 0,
            total_size: 0,
            paused: false,
            scan_complete: false,
            selected_file_index: 0,
            marked_for_deletion: Vec::new(),
            marked_for_deletion_all_groups: HashMap::new(),
            show_help: false,
            status_message: None,
            view_mode: ViewMode::Duplicates,
            file_receiver: None,
            progress_receiver: None,
            streaming_mode: false,
            storage_location,
        }
    }

    pub fn from_state_file(state_file: PathBuf) -> Result<Self> {
        let scan_state = ScanState::load(&state_file)?;

        let mut app = App::new(scan_state.config.clone());
        app.scanned_count = scan_state.scanned_count;
        app.total_size = scan_state.total_size;

        if scan_state.completed {
            // Restore duplicate groups
            for group in scan_state.duplicate_groups {
                app.finder.groups_mut().push(group);
            }
            app.state = AppState::ReviewingDuplicates;
            app.scan_complete = true;
        } else {
            // Resume scanning - we'll need to re-scan
            app.state = AppState::Scanning;
        }

        Ok(app)
    }

    pub fn save_state(&self) -> Result<()> {
        // Save to JSON state file if requested
        if self.config.save_state {
            let state_file = get_default_state_file(&self.config.root_path)?;

            let mut scan_state = ScanState::new(self.config.clone());
            scan_state.scanned_count = self.scanned_count;
            scan_state.total_size = self.total_size;
            scan_state.completed = self.scan_complete;
            scan_state.update_from_finder(&self.finder);

            scan_state.save(&state_file)?;
        }

        // Save to database if scan is complete
        if self.scan_complete {
            if let (Some(ref db), Some(scan_id)) = (&self.database, self.scan_id) {
                // Save all duplicate groups
                for group in self.finder.groups() {
                    let _ = db.save_duplicate_group(scan_id, group);
                }

                // Complete the scan
                let _ = db.complete_scan(scan_id, self.scanned_count, self.finder.groups().len());
            }
        }

        Ok(())
    }

    pub fn current_group(&self) -> Option<&DuplicateGroup> {
        self.finder.groups().get(self.current_group_index)
    }

    pub fn current_group_mut(&mut self) -> Option<&mut DuplicateGroup> {
        self.finder.groups_mut().get_mut(self.current_group_index)
    }

    pub fn next_group(&mut self) {
        if self.current_group_index < self.finder.groups().len().saturating_sub(1) {
            // Save current group's marking state
            self.save_current_group_markings();

            self.current_group_index += 1;
            self.selected_file_index = 0;
            self.update_marked_for_deletion();
        }
    }

    pub fn previous_group(&mut self) {
        if self.current_group_index > 0 {
            // Save current group's marking state
            self.save_current_group_markings();

            self.current_group_index -= 1;
            self.selected_file_index = 0;
            self.update_marked_for_deletion();
        }
    }

    fn save_current_group_markings(&mut self) {
        // Save the current group's markings to the HashMap
        if !self.marked_for_deletion.is_empty() {
            self.marked_for_deletion_all_groups.insert(
                self.current_group_index,
                self.marked_for_deletion.clone()
            );
        }
    }

    pub fn select_next_file(&mut self) {
        if let Some(group) = self.current_group() {
            if self.selected_file_index < group.files.len().saturating_sub(1) {
                self.selected_file_index += 1;
            }
        }
    }

    pub fn select_previous_file(&mut self) {
        if self.selected_file_index > 0 {
            self.selected_file_index -= 1;
        }
    }

    pub fn toggle_mark_for_deletion(&mut self) {
        if self.selected_file_index < self.marked_for_deletion.len() {
            self.marked_for_deletion[self.selected_file_index] =
                !self.marked_for_deletion[self.selected_file_index];
        }
    }

    pub fn mark_all_suggested(&mut self) {
        if let Some(group) = self.current_group() {
            let suggestions = SuggestionEngine::suggest_deletions(&group.files);

            // Get the best file to keep
            let keeper_index = SuggestionEngine::get_best_keeper(&group.files);

            // Get keeper name for status message
            let keeper_name = keeper_index
                .and_then(|i| group.files.get(i))
                .and_then(|f| f.path.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();

            let file_count = group.files.len();

            self.marked_for_deletion = vec![false; file_count];

            let mut marked_count = 0;
            for suggestion in &suggestions {
                // Mark for deletion, but never mark the keeper
                if suggestion.file_index < self.marked_for_deletion.len() {
                    if Some(suggestion.file_index) != keeper_index {
                        self.marked_for_deletion[suggestion.file_index] = true;
                        marked_count += 1;
                    }
                }
            }

            // Save markings to HashMap so they persist
            if marked_count > 0 {
                self.marked_for_deletion_all_groups.insert(
                    self.current_group_index,
                    self.marked_for_deletion.clone()
                );
            }

            // Set status message with details
            if marked_count > 0 {
                // Find the highest score to show confidence
                let max_score = suggestions.iter().map(|s| s.score).max().unwrap_or(0);
                let confidence = if max_score >= 100 {
                    "high confidence"
                } else if max_score >= 80 {
                    "good confidence"
                } else if max_score >= 60 {
                    "medium confidence"
                } else {
                    "low confidence - review carefully!"
                };

                self.set_status_message(format!(
                    "Auto-marked {} of {} file(s) ({}, score: {}) - keeping: {}",
                    marked_count,
                    file_count,
                    confidence,
                    max_score,
                    keeper_name
                ));
            } else {
                // Explain WHY there are no suggestions
                let reasons = if file_count < 2 {
                    "Only one file in group"
                } else {
                    "All files look equally good (use 'o' to mark all except oldest, or Space to mark manually)"
                };
                self.set_status_message(format!("No auto-suggestions - {}", reasons));
            }
        }
    }

    pub fn mark_all_suggested_all_groups(&mut self) {
        // Mark all suggested files across ALL groups
        let mut total_marked = 0;

        // Process each group
        let num_groups = self.finder.groups().len();
        for group_idx in 0..num_groups {
            if let Some(group) = self.finder.groups().get(group_idx) {
                let suggestions = SuggestionEngine::suggest_deletions(&group.files);
                let keeper_index = SuggestionEngine::get_best_keeper(&group.files);

                // Create marking vector for this group
                let mut markings = vec![false; group.files.len()];

                for suggestion in suggestions {
                    // Mark suggested files, but never mark the keeper
                    if Some(suggestion.file_index) != keeper_index && suggestion.file_index < markings.len() {
                        markings[suggestion.file_index] = true;
                        total_marked += 1;
                    }
                }

                // Store the markings for this group
                self.marked_for_deletion_all_groups.insert(group_idx, markings);
            }
        }

        // Update the current group's display to show the markings
        self.update_marked_for_deletion();

        // Set status message
        self.set_status_message(format!(
            "Auto-marked {} suggested file(s) across {} group(s)",
            total_marked,
            num_groups
        ));
    }

    pub fn mark_all_except_oldest(&mut self) {
        if let Some(group) = self.current_group() {
            if let Some(keeper_index) = SuggestionEngine::get_best_keeper(&group.files) {
                // Get keeper name before modifying state
                let keeper_name = group.files.get(keeper_index)
                    .and_then(|f| f.path.file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("?")
                    .to_string();

                let file_count = group.files.len();

                self.marked_for_deletion = vec![true; file_count];
                if keeper_index < self.marked_for_deletion.len() {
                    self.marked_for_deletion[keeper_index] = false;
                }

                // Save markings to HashMap so they persist
                self.marked_for_deletion_all_groups.insert(
                    self.current_group_index,
                    self.marked_for_deletion.clone()
                );

                // Set status message
                self.set_status_message(format!(
                    "Marked all except oldest - keeping: {}",
                    keeper_name
                ));
            }
        }
    }

    pub fn mark_all_except_oldest_all_groups(&mut self) {
        // Mark all except oldest across ALL groups
        let mut total_marked = 0;

        // Process each group
        let num_groups = self.finder.groups().len();
        for group_idx in 0..num_groups {
            if let Some(group) = self.finder.groups().get(group_idx) {
                if let Some(keeper_index) = SuggestionEngine::get_best_keeper(&group.files) {
                    // Create marking vector for this group
                    let mut markings = vec![true; group.files.len()];
                    if keeper_index < markings.len() {
                        markings[keeper_index] = false;
                    }

                    // Count marked files (all except the keeper)
                    total_marked += markings.iter().filter(|&&m| m).count();

                    // Store the markings for this group
                    self.marked_for_deletion_all_groups.insert(group_idx, markings);
                }
            }
        }

        // Update the current group's display to show the markings
        self.update_marked_for_deletion();

        // Set status message
        self.set_status_message(format!(
            "Marked all except oldest across {} group(s) - {} file(s) marked",
            num_groups,
            total_marked
        ));
    }

    pub fn delete_marked_files(&mut self) -> Result<usize> {
        // Save current group's markings before we start deleting
        self.save_current_group_markings();

        let mut deleted_count = 0;
        let mut failed_count = 0;

        if let Some(group) = self.current_group() {
            let mut files_to_delete = Vec::new();

            for (i, &marked) in self.marked_for_deletion.iter().enumerate() {
                if marked && i < group.files.len() {
                    files_to_delete.push(group.files[i].clone());
                }
            }

            // Track which files were successfully deleted
            let mut successfully_deleted = Vec::new();

            // Delete files
            for file_info in files_to_delete.iter() {
                match self.backup_manager.delete_with_backup(&file_info.path) {
                    Ok(_) => {
                        deleted_count += 1;
                        successfully_deleted.push(file_info.path.clone());
                    }
                    Err(e) => {
                        failed_count += 1;
                        eprintln!("Error deleting {}: {}", file_info.path.display(), e);
                    }
                }
            }

            // Remove ONLY successfully deleted files from the group
            if let Some(group) = self.current_group_mut() {
                group.files.retain(|f| {
                    !successfully_deleted.iter().any(|d| d == &f.path)
                });
            }

            // Clean up empty groups
            self.finder.remove_empty_groups();

            // Clear the marking state for this group since files were deleted
            self.marked_for_deletion_all_groups.remove(&self.current_group_index);

            // Reset selection
            self.selected_file_index = 0;
            self.update_marked_for_deletion();

            // Save state after deletion
            let _ = self.save_state();

            // Set status message with results
            if failed_count > 0 {
                self.set_status_message(format!(
                    "Deleted {} file(s), {} failed (check terminal for errors)",
                    deleted_count,
                    failed_count
                ));
            }
        }

        Ok(deleted_count)
    }

    pub fn delete_marked_files_all_groups(&mut self) -> Result<usize> {
        // Save current group's markings first
        self.save_current_group_markings();

        let mut total_deleted = 0;
        let mut total_failed = 0;

        // Process each group that has marked files
        let num_groups = self.finder.groups().len();
        for group_idx in 0..num_groups {
            // Get the markings for this group (if any)
            let markings = if group_idx == self.current_group_index {
                self.marked_for_deletion.clone()
            } else {
                self.marked_for_deletion_all_groups
                    .get(&group_idx)
                    .cloned()
                    .unwrap_or_default()
            };

            if markings.is_empty() || !markings.iter().any(|&m| m) {
                continue; // Skip groups with no markings
            }

            if let Some(group) = self.finder.groups().get(group_idx).cloned() {
                let mut files_to_delete = Vec::new();

                for (i, &marked) in markings.iter().enumerate() {
                    if marked && i < group.files.len() {
                        files_to_delete.push(group.files[i].clone());
                    }
                }

                // Track which files were successfully deleted
                let mut successfully_deleted = Vec::new();

                // Delete files
                for file_info in files_to_delete.iter() {
                    match self.backup_manager.delete_with_backup(&file_info.path) {
                        Ok(_) => {
                            total_deleted += 1;
                            successfully_deleted.push(file_info.path.clone());
                        }
                        Err(e) => {
                            total_failed += 1;
                            eprintln!("Error deleting {}: {}", file_info.path.display(), e);
                        }
                    }
                }

                // Remove ONLY successfully deleted files from the group
                if let Some(group) = self.finder.groups_mut().get_mut(group_idx) {
                    group.files.retain(|f| {
                        !successfully_deleted.iter().any(|d| d == &f.path)
                    });
                }
            }
        }

        // Clean up empty groups
        self.finder.remove_empty_groups();

        // Clear all marking state
        self.marked_for_deletion_all_groups.clear();

        // Reset to first group and clear markings
        self.current_group_index = 0;
        self.selected_file_index = 0;
        self.update_marked_for_deletion();

        // Save state after deletion
        let _ = self.save_state();

        // Set status message with results
        if total_failed > 0 {
            self.set_status_message(format!(
                "Deleted {} file(s) across all groups, {} failed (check terminal for errors)",
                total_deleted,
                total_failed
            ));
        }

        Ok(total_deleted)
    }

    pub fn update_marked_for_deletion(&mut self) {
        if let Some(group) = self.current_group() {
            // Check if we have saved markings for this group
            if let Some(saved_markings) = self.marked_for_deletion_all_groups.get(&self.current_group_index) {
                self.marked_for_deletion = saved_markings.clone();
            } else {
                self.marked_for_deletion = vec![false; group.files.len()];
            }
        } else {
            self.marked_for_deletion.clear();
        }
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub fn set_status_message(&mut self, message: String) {
        self.status_message = Some(message);
    }

    pub fn clear_status_message(&mut self) {
        self.status_message = None;
    }

    pub fn cycle_view(&mut self) {
        self.view_mode = match self.view_mode {
            ViewMode::Duplicates => ViewMode::Statistics,
            ViewMode::Statistics => ViewMode::Help,
            ViewMode::Help => ViewMode::Duplicates,
        };
    }

    /// Start streaming scan - files will be processed incrementally as they're found
    pub fn start_streaming_scan(&mut self) {
        if let Some(scanner) = &self.scanner {
            let (file_rx, progress_rx) = scanner.scan_streaming();
            self.file_receiver = Some(file_rx);
            self.progress_receiver = Some(progress_rx);
            self.streaming_mode = true;
            self.state = AppState::ReviewingDuplicates; // Allow immediate interaction
        }
    }

    /// Process incoming files from the stream - returns true if scan is still active
    pub fn process_incoming_files(&mut self) -> bool {
        let mut scan_active = true;
        let mut files_processed = false;

        // Process all available batches (non-blocking)
        if let Some(ref file_rx) = self.file_receiver {
            loop {
                match file_rx.try_recv() {
                    Ok(batch) => {
                        // Process all files in this batch
                        for file in batch {
                            let _ = self.finder.process_file(file);
                        }
                        files_processed = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        break; // No more batches available right now
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        scan_active = false;
                        self.scan_complete = true;
                        break; // Scanner finished
                    }
                }
            }
        }

        // Update markings after processing files (outside the borrow)
        // Sort groups periodically for display (but not on every file)
        if files_processed {
            // Sort every 100 files or when scan completes
            if self.scanned_count % 100 == 0 || !scan_active {
                self.finder.ensure_sorted();
            }
            self.update_marked_for_deletion();
        }

        // Process progress updates
        if let Some(ref progress_rx) = self.progress_receiver {
            loop {
                match progress_rx.try_recv() {
                    Ok(progress) => {
                        self.scanned_count = progress.scanned_count;
                        self.total_size = progress.total_size;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        break;
                    }
                }
            }
        }

        scan_active
    }
}
