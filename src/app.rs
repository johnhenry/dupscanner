use crate::backup::BackupManager;
use crate::duplicates::{DuplicateFinder, DuplicateGroup};
use crate::scanner::{ScanConfig, Scanner};
use crate::state::{get_default_state_file, ScanState};
use crate::suggestions::SuggestionEngine;
use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Scanning,
    FindingDuplicates,
    ReviewingDuplicates,
}

pub struct App {
    pub config: ScanConfig,
    pub scanner: Option<Scanner>,
    pub finder: DuplicateFinder,
    pub backup_manager: BackupManager,
    pub state: AppState,
    pub current_group_index: usize,
    pub scanned_count: usize,
    pub total_size: u64,
    pub paused: bool,
    pub scan_complete: bool,
    pub selected_file_index: usize,
    pub marked_for_deletion: Vec<bool>,
    pub show_help: bool,
    pub status_message: Option<String>,
}

impl App {
    pub fn new(config: ScanConfig) -> Self {
        let scanner = Some(Scanner::new(config.clone()));
        let mut backup_manager = BackupManager::new().unwrap_or_default();
        let _ = backup_manager.load_records();

        App {
            config,
            scanner,
            finder: DuplicateFinder::new(),
            backup_manager,
            state: AppState::Scanning,
            current_group_index: 0,
            scanned_count: 0,
            total_size: 0,
            paused: false,
            scan_complete: false,
            selected_file_index: 0,
            marked_for_deletion: Vec::new(),
            show_help: false,
            status_message: None,
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
        if !self.config.save_state {
            return Ok(());
        }

        let state_file = get_default_state_file(&self.config.root_path)?;

        let mut scan_state = ScanState::new(self.config.clone());
        scan_state.scanned_count = self.scanned_count;
        scan_state.total_size = self.total_size;
        scan_state.completed = self.scan_complete;
        scan_state.update_from_finder(&self.finder);

        scan_state.save(&state_file)?;

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
            self.current_group_index += 1;
            self.selected_file_index = 0;
            self.update_marked_for_deletion();
        }
    }

    pub fn previous_group(&mut self) {
        if self.current_group_index > 0 {
            self.current_group_index -= 1;
            self.selected_file_index = 0;
            self.update_marked_for_deletion();
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

            self.marked_for_deletion = vec![false; group.files.len()];

            for suggestion in suggestions {
                if suggestion.file_index < self.marked_for_deletion.len() {
                    self.marked_for_deletion[suggestion.file_index] = true;
                }
            }
        }
    }

    pub fn mark_all_except_oldest(&mut self) {
        if let Some(group) = self.current_group() {
            if let Some(keeper_index) = SuggestionEngine::get_best_keeper(&group.files) {
                self.marked_for_deletion = vec![true; group.files.len()];
                if keeper_index < self.marked_for_deletion.len() {
                    self.marked_for_deletion[keeper_index] = false;
                }
            }
        }
    }

    pub fn delete_marked_files(&mut self) -> Result<usize> {
        let mut deleted_count = 0;

        if let Some(group) = self.current_group() {
            let mut files_to_delete = Vec::new();

            for (i, &marked) in self.marked_for_deletion.iter().enumerate() {
                if marked && i < group.files.len() {
                    files_to_delete.push(group.files[i].clone());
                }
            }

            // Delete in reverse order to maintain indices
            for file_info in files_to_delete.iter().rev() {
                match self.backup_manager.delete_with_backup(&file_info.path) {
                    Ok(_) => {
                        deleted_count += 1;
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Error deleting {}: {}", file_info.path.display(), e));
                    }
                }
            }

            // Remove deleted files from the group
            if let Some(group) = self.current_group_mut() {
                group.files.retain(|f| {
                    !files_to_delete.iter().any(|d| d.path == f.path)
                });
            }

            // Clean up empty groups
            self.finder.remove_empty_groups();

            // Reset selection
            self.selected_file_index = 0;
            self.update_marked_for_deletion();

            // Save state after deletion
            let _ = self.save_state();
        }

        Ok(deleted_count)
    }

    pub fn update_marked_for_deletion(&mut self) {
        if let Some(group) = self.current_group() {
            self.marked_for_deletion = vec![false; group.files.len()];
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
}
