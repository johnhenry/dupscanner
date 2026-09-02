use crate::app::{App, AppState, ViewMode, StorageLocation};
use crate::suggestions::SuggestionEngine;
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::io;
use std::time::Duration;

pub async fn run(app: &mut App) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run the app
    let res = run_app(&mut terminal, app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(err) = res {
        println!("Error: {:?}", err);
    }

    Ok(())
}

async fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    // Start scanning if needed
    if app.state == AppState::Scanning && !app.scan_complete {
        // Start streaming scan instead of batch scan
        app.start_streaming_scan();
    }

    // Main event loop
    loop {
        // Process incoming files if in streaming mode
        if app.streaming_mode && !app.scan_complete {
            app.process_incoming_files();
        }

        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match app.state {
                    AppState::Scanning => {
                        match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Char('p') => {
                                app.paused = !app.paused;
                            }
                            KeyCode::Char('s') => {
                                app.save_state()?;
                                app.set_status_message("State saved!".to_string());
                            }
                            _ => {}
                        }
                    }
                    AppState::ReviewingDuplicates => {
                        // Tab key cycles through views regardless of current view
                        if let KeyCode::Tab = key.code {
                            app.cycle_view();
                        } else if app.show_help {
                            if let KeyCode::Char('?') | KeyCode::Esc = key.code {
                                app.toggle_help();
                            }
                        } else {
                            match key.code {
                                KeyCode::Char('q') => {
                                    app.save_state()?;
                                    return Ok(());
                                }
                                KeyCode::Char('?') => {
                                    app.toggle_help();
                                }
                                KeyCode::Char('j') | KeyCode::Down => {
                                    app.select_next_file();
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    app.select_previous_file();
                                }
                                KeyCode::Char('n') | KeyCode::Right => {
                                    app.next_group();
                                }
                                KeyCode::Char('p') | KeyCode::Left => {
                                    app.previous_group();
                                }
                                KeyCode::Char(' ') => {
                                    app.toggle_mark_for_deletion();
                                }
                                KeyCode::Char('a') => {
                                    app.mark_all_suggested();
                                }
                                KeyCode::Char('A') => {
                                    // Shift+A: Mark all suggested files across ALL groups
                                    app.mark_all_suggested_all_groups();
                                }
                                KeyCode::Char('o') => {
                                    app.mark_all_except_oldest();
                                }
                                KeyCode::Char('O') => {
                                    // Shift+O: Mark all except oldest across ALL groups
                                    app.mark_all_except_oldest_all_groups();
                                }
                                KeyCode::Char('d') => {
                                    if let Ok(count) = app.delete_marked_files() {
                                        app.set_status_message(format!("Deleted {} file(s)", count));
                                    }
                                }
                                KeyCode::Char('D') => {
                                    // Shift+D: Delete all marked files across ALL groups
                                    if let Ok(count) = app.delete_marked_files_all_groups() {
                                        app.set_status_message(format!("Deleted {} file(s) across all groups", count));
                                    }
                                }
                                KeyCode::Char('s') => {
                                    app.save_state()?;
                                    app.set_status_message("State saved!".to_string());
                                }
                                _ => {}
                            }
                        }
                    }
                }

                // Clear status message on next keypress (but not for commands that set status)
                if !matches!(key.code, KeyCode::Char('s') | KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('o') | KeyCode::Char('O') | KeyCode::Char('d') | KeyCode::Char('D')) {
                    app.clear_status_message();
                }
            }
        }
    }
}


fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(3),
        ])
        .split(f.area());

    // Header
    render_header(f, chunks[0], app);

    // Main content
    match app.state {
        AppState::Scanning => render_scanning(f, chunks[1], app),
        AppState::ReviewingDuplicates => {
            match app.view_mode {
                ViewMode::Duplicates => render_duplicates(f, chunks[1], app),
                ViewMode::Statistics => render_statistics(f, chunks[1], app),
                ViewMode::Help => render_help(f, chunks[1]),
            }
        }
    }

    // Footer
    render_footer(f, chunks[2], app);
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let title = match app.state {
        AppState::Scanning => "Scanning for Files".to_string(),
        AppState::ReviewingDuplicates => {
            let view_name = match app.view_mode {
                ViewMode::Duplicates => "Duplicates",
                ViewMode::Statistics => "Statistics",
                ViewMode::Help => "Help",
            };

            if app.streaming_mode && !app.scan_complete {
                format!("{} (Scanning in Progress...)", view_name)
            } else {
                view_name.to_string()
            }
        }
    };

    let header = Paragraph::new(title)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(header, area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let footer_text = if let Some(msg) = &app.status_message {
        msg.clone()
    } else {
        match app.state {
            AppState::Scanning => "q: quit | p: pause | s: save state".to_string(),
            AppState::ReviewingDuplicates => {
                let base_commands = match app.view_mode {
                    ViewMode::Duplicates => {
                        if app.streaming_mode && !app.scan_complete {
                            format!(
                                "Scanning: {} files, {} groups | j/k: select | n/p: group | space: mark | d: delete",
                                app.scanned_count,
                                app.finder.groups().len()
                            )
                        } else {
                            "j/k: select | n/p: group | space: mark | a/A: auto | o/O: oldest | d/D: delete".to_string()
                        }
                    }
                    ViewMode::Statistics => "View statistics and scan progress".to_string(),
                    ViewMode::Help => "Press Tab or ? to return".to_string(),
                };
                format!("{} | Tab: change view | q: quit", base_commands)
            }
        }
    };

    let footer = Paragraph::new(footer_text)
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(footer, area);
}

fn render_scanning(f: &mut Frame, area: Rect, app: &App) {
    let text = vec![
        Line::from(vec![
            Span::raw("Scanned: "),
            Span::styled(
                format!("{}", app.scanned_count),
                Style::default().fg(Color::Green),
            ),
            Span::raw(" files"),
        ]),
        Line::from(vec![
            Span::raw("Total size: "),
            Span::styled(
                format_size(app.total_size),
                Style::default().fg(Color::Yellow),
            ),
        ]),
        Line::from(""),
        Line::from(if app.paused {
            Span::styled("PAUSED", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
        } else {
            Span::styled("Scanning...", Style::default().fg(Color::Green))
        }),
    ];

    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("Progress"))
        .alignment(Alignment::Left);

    f.render_widget(paragraph, area);
}

fn render_duplicates(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    // Left panel: Groups
    render_groups_panel(f, chunks[0], app);

    // Right panel: Files in current group
    render_files_panel(f, chunks[1], app);
}

fn render_groups_panel(f: &mut Frame, area: Rect, app: &App) {
    let groups = app.finder.groups();

    if groups.is_empty() {
        let text = Paragraph::new("No duplicates found!")
            .style(Style::default().fg(Color::Green))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Groups"));
        f.render_widget(text, area);
        return;
    }

    let items: Vec<ListItem> = groups
        .iter()
        .enumerate()
        .map(|(i, group)| {
            let style = if i == app.current_group_index {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let content = format!(
                "{} files - {} wasted",
                group.file_count(),
                format_size(group.wasted_space)
            );

            ListItem::new(content).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Groups ({}/{})", app.current_group_index + 1, groups.len())),
    );

    f.render_widget(list, area);
}

fn render_files_panel(f: &mut Frame, area: Rect, app: &App) {
    if let Some(group) = app.current_group() {
        let suggestions = SuggestionEngine::suggest_deletions(&group.files);

        let items: Vec<ListItem> = group
            .files
            .iter()
            .enumerate()
            .map(|(i, file)| {
                let is_selected = i == app.selected_file_index;
                let is_marked = i < app.marked_for_deletion.len() && app.marked_for_deletion[i];

                let suggestion = suggestions.iter().find(|s| s.file_index == i);

                let mut spans = vec![];

                // Checkbox
                if is_marked {
                    spans.push(Span::styled("[X] ", Style::default().fg(Color::Red)));
                } else {
                    spans.push(Span::raw("[ ] "));
                }

                // Path (truncate if too long to prevent rendering issues)
                let path_str = file.path.display().to_string();
                let max_path_len = area.width.saturating_sub(20) as usize; // Leave room for checkbox and score
                let truncated_path = if path_str.len() > max_path_len && max_path_len > 20 {
                    format!("...{}", &path_str[path_str.len().saturating_sub(max_path_len - 3)..])
                } else {
                    path_str
                };
                let style = if is_selected {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                spans.push(Span::styled(truncated_path, style));

                // Suggestion indicator - show for ALL files
                if let Some(sugg) = suggestion {
                    // File is suggested for deletion
                    spans.push(Span::styled(
                        format!(" (score: {})", sugg.score),
                        Style::default().fg(Color::Red),
                    ));
                } else {
                    // File is being kept (no suggestions against it)
                    let keeper_index = SuggestionEngine::get_best_keeper(&group.files);
                    if Some(i) == keeper_index {
                        spans.push(Span::styled(
                            " (KEEPER)",
                            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                        ));
                    } else {
                        // File has no negative indicators, but isn't the keeper either
                        spans.push(Span::styled(
                            " (neutral)",
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                }

                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!("Files - {} ({} each)", group.file_count(), format_size(group.files[0].size))),
        );

        f.render_widget(list, area);
    } else {
        let text = Paragraph::new("No group selected")
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Files"));
        f.render_widget(text, area);
    }
}

fn render_statistics(f: &mut Frame, area: Rect, app: &App) {
    use humansize::{format_size as fmt_size, BINARY};

    let scan_status = if app.streaming_mode && !app.scan_complete {
        Span::styled("In Progress", Style::default().fg(Color::Yellow))
    } else {
        Span::styled("Complete", Style::default().fg(Color::Green))
    };

    let total_wasted: u64 = app.finder.groups().iter().map(|g| g.wasted_space).sum();
    let total_marked: usize = app.marked_for_deletion_all_groups.values()
        .map(|marks| marks.iter().filter(|&&m| m).count())
        .sum();

    // Get database and backup paths based on storage location
    let (storage_type, db_path, backup_path) = match &app.storage_location {
        StorageLocation::PerDirectory(dir) => (
            "Per-Directory (.dupscanner)",
            dir.join("scans.db").display().to_string(),
            dir.join("backups").display().to_string(),
        ),
        StorageLocation::InMemory => (
            "In-Memory Only (Read-Only Directory)",
            ":memory: (not persisted)".to_string(),
            "N/A (no backups in memory mode)".to_string(),
        ),
        StorageLocation::Global(dir) => (
            "Global (Legacy)",
            dir.join("scans.db").display().to_string(),
            dir.join("backups").display().to_string(),
        ),
    };

    let stats_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Scan Information", Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("  Scan Location:  "),
            Span::styled(app.config.root_path.display().to_string(), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::raw("  Scan Status:    "),
            scan_status,
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("File Statistics", Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("  Files Scanned:  "),
            Span::styled(format!("{}", app.scanned_count), Style::default().fg(Color::Green)),
        ]),
        Line::from(vec![
            Span::raw("  Total Size:     "),
            Span::styled(fmt_size(app.total_size, BINARY), Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Duplicate Information", Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("  Groups Found:   "),
            Span::styled(format!("{}", app.finder.groups().len()), Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::raw("  Wasted Space:   "),
            Span::styled(fmt_size(total_wasted, BINARY), Style::default().fg(Color::Red)),
        ]),
        Line::from(vec![
            Span::raw("  Files Marked:   "),
            Span::styled(format!("{}", total_marked), Style::default().fg(Color::Magenta)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Storage Locations", Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("  Storage Mode:   "),
            Span::styled(storage_type, Style::default().fg(Color::Yellow)),
        ]),
        Line::from(vec![
            Span::raw("  Database:       "),
            Span::styled(db_path, Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::raw("  Backups:        "),
            Span::styled(backup_path, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Scan Settings", Style::default().add_modifier(Modifier::BOLD).fg(Color::Cyan)),
        ]),
        Line::from(vec![
            Span::raw("  Min File Size:  "),
            Span::styled(fmt_size(app.config.min_size, BINARY), Style::default().fg(Color::White)),
        ]),
        Line::from(vec![
            Span::raw("  Max File Size:  "),
            Span::styled(
                app.config.max_size.map_or("Unlimited".to_string(), |s| fmt_size(s, BINARY)),
                Style::default().fg(Color::White)
            ),
        ]),
        Line::from(vec![
            Span::raw("  Batch Size:     "),
            Span::styled(format!("{} files", app.config.batch_size), Style::default().fg(Color::White)),
        ]),
    ];

    let stats = Paragraph::new(stats_text)
        .block(Block::default().borders(Borders::ALL).title("Statistics"))
        .alignment(Alignment::Left);

    f.render_widget(stats, area);
}

fn render_help(f: &mut Frame, area: Rect) {
    let help_text = vec![
        Line::from(""),
        Line::from(Span::styled("Navigation", Style::default().add_modifier(Modifier::BOLD))),
        Line::from("  j/Down      - Select next file"),
        Line::from("  k/Up        - Select previous file"),
        Line::from("  n/Right     - Next duplicate group"),
        Line::from("  p/Left      - Previous duplicate group"),
        Line::from(""),
        Line::from(Span::styled("Actions", Style::default().add_modifier(Modifier::BOLD))),
        Line::from("  Space       - Toggle mark file for deletion"),
        Line::from("  a           - Auto-mark suggested files (current group)"),
        Line::from("  A (Shift+A) - Auto-mark suggested files (ALL groups)"),
        Line::from("  o           - Mark all except oldest (current group)"),
        Line::from("  O (Shift+O) - Mark all except oldest (ALL groups)"),
        Line::from("  d           - Delete marked files in current group (with backup)"),
        Line::from("  D (Shift+D) - Delete ALL marked files across all groups (with backup)"),
        Line::from("  s           - Save current state"),
        Line::from(""),
        Line::from(Span::styled("Other", Style::default().add_modifier(Modifier::BOLD))),
        Line::from("  Tab         - Change view (Duplicates/Statistics/Help)"),
        Line::from("  ?           - Toggle help"),
        Line::from("  q           - Quit"),
        Line::from(""),
        Line::from(Span::styled("Files are backed up before deletion!", Style::default().fg(Color::Green))),
    ];

    let help = Paragraph::new(help_text)
        .block(Block::default().borders(Borders::ALL).title("Help"))
        .alignment(Alignment::Left);

    f.render_widget(help, area);
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
