use crate::app::{App, AppState};
use crate::suggestions::SuggestionEngine;
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;
use std::path::Path;
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
        scan_files(terminal, app).await?;
    }

    // Main event loop
    loop {
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
                    AppState::FindingDuplicates => {
                        // Just wait for this to complete
                    }
                    AppState::ReviewingDuplicates => {
                        if app.show_help {
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
                                KeyCode::Char('o') => {
                                    app.mark_all_except_oldest();
                                }
                                KeyCode::Char('d') => {
                                    if let Ok(count) = app.delete_marked_files() {
                                        app.set_status_message(format!("Deleted {} file(s)", count));
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
                    AppState::Completed => {
                        match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            _ => {}
                        }
                    }
                }

                // Clear status message on next keypress
                if !matches!(key.code, KeyCode::Char('s') | KeyCode::Char('d')) {
                    app.clear_status_message();
                }
            }
        }
    }
}

async fn scan_files<B: Backend>(terminal: &mut Terminal<B>, app: &mut App) -> Result<()> {
    let mut last_path = String::new();

    if let Some(scanner) = &mut app.scanner {
        let size_groups = scanner.scan(|count, path: &Path| {
            app.scanned_count = count;
            last_path = path.display().to_string();
        })?;

        app.total_size = scanner.total_size();
        app.scan_complete = true;

        // Update state
        app.state = AppState::FindingDuplicates;
        terminal.draw(|f| ui(f, app))?;

        // Find duplicates
        app.finder.find_duplicates(size_groups)?;

        // Update state to reviewing
        app.state = AppState::ReviewingDuplicates;
        app.update_marked_for_deletion();

        // Save initial state
        app.save_state()?;
    }

    Ok(())
}

fn ui<B: Backend>(f: &mut Frame, app: &App) {
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
        AppState::FindingDuplicates => render_finding_duplicates(f, chunks[1], app),
        AppState::ReviewingDuplicates => {
            if app.show_help {
                render_help(f, chunks[1]);
            } else {
                render_duplicates(f, chunks[1], app);
            }
        }
        AppState::Completed => render_completed(f, chunks[1], app),
    }

    // Footer
    render_footer(f, chunks[2], app);
}

fn render_header<B: Backend>(f: &mut Frame, area: Rect, app: &App) {
    let title = match app.state {
        AppState::Scanning => "Scanning for Files",
        AppState::FindingDuplicates => "Finding Duplicates",
        AppState::ReviewingDuplicates => "Review Duplicate Files",
        AppState::Completed => "Scan Complete",
    };

    let header = Paragraph::new(title)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(header, area);
}

fn render_footer<B: Backend>(f: &mut Frame, area: Rect, app: &App) {
    let footer_text = if let Some(msg) = &app.status_message {
        msg.clone()
    } else {
        match app.state {
            AppState::Scanning => "q: quit | p: pause | s: save state".to_string(),
            AppState::ReviewingDuplicates => {
                "q: quit | ?: help | j/k: select | n/p: next/prev group | space: mark | d: delete | a: auto-mark | o: keep oldest".to_string()
            }
            _ => "q: quit".to_string(),
        }
    };

    let footer = Paragraph::new(footer_text)
        .style(Style::default().fg(Color::Gray))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(footer, area);
}

fn render_scanning<B: Backend>(f: &mut Frame, area: Rect, app: &App) {
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

fn render_finding_duplicates<B: Backend>(f: &mut Frame, area: Rect, app: &App) {
    let text = vec![
        Line::from("Finding duplicate files..."),
        Line::from(""),
        Line::from(vec![
            Span::raw("Total files scanned: "),
            Span::styled(
                format!("{}", app.scanned_count),
                Style::default().fg(Color::Green),
            ),
        ]),
    ];

    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("Processing"))
        .alignment(Alignment::Center);

    f.render_widget(paragraph, area);
}

fn render_duplicates<B: Backend>(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    // Left panel: Groups
    render_groups_panel(f, chunks[0], app);

    // Right panel: Files in current group
    render_files_panel(f, chunks[1], app);
}

fn render_groups_panel<B: Backend>(f: &mut Frame, area: Rect, app: &App) {
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

fn render_files_panel<B: Backend>(f: &mut Frame, area: Rect, app: &App) {
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

                // Path
                let path_str = file.path.display().to_string();
                let style = if is_selected {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                spans.push(Span::styled(path_str, style));

                // Suggestion indicator
                if let Some(sugg) = suggestion {
                    spans.push(Span::styled(
                        format!(" (score: {})", sugg.score),
                        Style::default().fg(Color::Red),
                    ));
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

fn render_help<B: Backend>(f: &mut Frame, area: Rect) {
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
        Line::from("  a           - Auto-mark suggested files"),
        Line::from("  o           - Mark all except oldest"),
        Line::from("  d           - Delete marked files (with backup)"),
        Line::from("  s           - Save current state"),
        Line::from(""),
        Line::from(Span::styled("Other", Style::default().add_modifier(Modifier::BOLD))),
        Line::from("  ?           - Toggle this help"),
        Line::from("  q           - Quit"),
        Line::from(""),
        Line::from(Span::styled("Files are backed up before deletion!", Style::default().fg(Color::Green))),
    ];

    let help = Paragraph::new(help_text)
        .block(Block::default().borders(Borders::ALL).title("Help"))
        .alignment(Alignment::Left);

    f.render_widget(help, area);
}

fn render_completed<B: Backend>(f: &mut Frame, area: Rect, app: &App) {
    let text = vec![
        Line::from(Span::styled("Scan Complete!", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(format!("Total files scanned: {}", app.scanned_count)),
        Line::from(format!("Duplicate groups found: {}", app.finder.groups().len())),
        Line::from(format!("Total wasted space: {}", format_size(app.finder.total_wasted_space()))),
    ];

    let paragraph = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title("Results"))
        .alignment(Alignment::Center);

    f.render_widget(paragraph, area);
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
