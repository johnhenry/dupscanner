use crate::app::{App, Input, Scope, ViewMode};
use crate::preview::Preview;
use crate::selection::SelectMode;
use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use humansize::{format_size, BINARY};
use ratatui_image::{picker::Picker, protocol::StatefulProtocol, Resize, StatefulImage};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

/// Terminal-side image state: which graphics protocol the terminal speaks
/// and the encoded image currently on screen.
pub struct ImagePane {
    picker: Option<Picker>,
    current: Option<(PathBuf, StatefulProtocol)>,
}

impl ImagePane {
    /// Query the terminal for its graphics protocol. Must run after entering
    /// the alternate screen and before reading key events. Falls back to
    /// Unicode half-blocks when the terminal does not answer.
    fn detect() -> Self {
        if std::env::var_os("DUPSCANNER_NO_IMAGES").is_some() {
            return ImagePane { picker: None, current: None };
        }
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::from_fontsize((8, 16)));
        ImagePane {
            picker: Some(picker),
            current: None,
        }
    }

    fn protocol_name(&self) -> &'static str {
        match self.picker.as_ref().map(|p| p.protocol_type()) {
            Some(ratatui_image::picker::ProtocolType::Kitty) => "kitty graphics",
            Some(ratatui_image::picker::ProtocolType::Sixel) => "sixel",
            Some(ratatui_image::picker::ProtocolType::Iterm2) => "iTerm2 inline images",
            Some(ratatui_image::picker::ProtocolType::Halfblocks) => "Unicode half-blocks",
            None => "disabled",
        }
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
}

pub fn run(app: &mut App) -> Result<()> {
    // Make sure a panic anywhere leaves the terminal usable.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        default_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut pane = ImagePane::detect();

    let res = run_app(&mut terminal, app, &mut pane);

    restore_terminal();
    let _ = terminal.show_cursor();
    let _ = std::panic::take_hook();

    res
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, app: &mut App, pane: &mut ImagePane) -> Result<()> {
    loop {
        app.tick();
        app.previewer.poll();
        terminal.draw(|f| ui(f, app, pane))?;

        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        // 1. Confirmation modal swallows every key.
        if app.pending_confirm.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => app.confirm_delete()?,
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => app.cancel_delete(),
                _ => {}
            }
            continue;
        }

        // 2. Text input (filter or rename) in the footer.
        if app.input.is_some() {
            match key.code {
                KeyCode::Esc => app.cancel_input(),
                KeyCode::Enter => app.commit_input(),
                KeyCode::Backspace => app.input_pop(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => app.input_push(c),
                _ => {}
            }
            continue;
        }

        // 3. Mark-mode menu: letter applies to the group, Shift+letter to all.
        if app.show_mark_menu {
            match key.code {
                KeyCode::Esc | KeyCode::Char('m') | KeyCode::Char('q') => app.show_mark_menu = false,
                KeyCode::Char(c) => {
                    if let Some(mode) = SelectMode::from_shortcut(c) {
                        let scope = if c.is_ascii_uppercase() { Scope::AllGroups } else { Scope::CurrentGroup };
                        app.show_mark_menu = false;
                        app.apply_select_mode(mode, scope);
                    }
                }
                _ => {}
            }
            continue;
        }

        let keeps_status = matches!(
            key.code,
            KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('o') | KeyCode::Char('O')
                | KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Char('c') | KeyCode::Char('C')
                | KeyCode::Char('z') | KeyCode::Char('t') | KeyCode::Char('e') | KeyCode::Char('x')
                | KeyCode::Char('N') | KeyCode::Char('v')
        );
        if !keeps_status {
            app.clear_status();
        }

        match key.code {
            KeyCode::Char('q') => return Ok(()),
            KeyCode::Tab => app.cycle_view(),
            KeyCode::Char('?') => {
                app.view_mode = if app.view_mode == ViewMode::Help {
                    ViewMode::Duplicates
                } else {
                    ViewMode::Help
                }
            }
            KeyCode::Esc if app.view_mode != ViewMode::Duplicates => app.view_mode = ViewMode::Duplicates,
            KeyCode::Char('j') | KeyCode::Down => app.select_next_file(),
            KeyCode::Char('k') | KeyCode::Up => app.select_previous_file(),
            KeyCode::Char('n') | KeyCode::Right | KeyCode::PageDown => app.next_group(),
            KeyCode::Char('p') | KeyCode::Left | KeyCode::PageUp => app.previous_group(),
            KeyCode::Char('g') | KeyCode::Home => app.first_group(),
            KeyCode::Char('G') | KeyCode::End => app.last_group(),
            KeyCode::Char(' ') => app.toggle_mark(),
            KeyCode::Char('a') => app.mark_suggested(Scope::CurrentGroup),
            KeyCode::Char('A') => app.mark_suggested(Scope::AllGroups),
            KeyCode::Char('o') => app.mark_all_but_keeper(Scope::CurrentGroup),
            KeyCode::Char('O') => app.mark_all_but_keeper(Scope::AllGroups),
            KeyCode::Char('m') => app.show_mark_menu = true,
            KeyCode::Char('c') => app.clear_marks(Scope::CurrentGroup),
            KeyCode::Char('C') => app.clear_marks(Scope::AllGroups),
            KeyCode::Char('d') => app.request_delete(Scope::CurrentGroup),
            KeyCode::Char('D') => app.request_delete(Scope::AllGroups),
            KeyCode::Char('r') => app.start_rename(),
            KeyCode::Char('N') => app.rename_keeper_to_canonical(),
            KeyCode::Char('e') | KeyCode::Enter => app.open_selected(),
            KeyCode::Char('/') => app.start_filter_input(),
            KeyCode::Char('z') => app.cycle_size_filter(),
            KeyCode::Char('t') => app.cycle_kind_filter(),
            KeyCode::Char('x') => app.clear_filter(),
            KeyCode::Char('v') => app.toggle_preview(),
            _ => {}
        }
    }
}

fn ui(f: &mut Frame, app: &mut App, pane: &mut ImagePane) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(10), Constraint::Length(3)])
        .split(f.area());

    render_header(f, chunks[0], app);
    match app.view_mode {
        ViewMode::Duplicates => render_duplicates(f, chunks[1], app, pane),
        ViewMode::Statistics => render_statistics(f, chunks[1], app, pane),
        ViewMode::Help => render_help(f, chunks[1], app),
    }
    render_footer(f, chunks[2], app);

    if let Some(pending) = &app.pending_confirm {
        render_confirm(f, app, pending);
    } else if app.show_mark_menu {
        render_mark_menu(f);
    }
}

fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let view = match app.view_mode {
        ViewMode::Duplicates => "Duplicates",
        ViewMode::Statistics => "Statistics",
        ViewMode::Help => "Help",
    };
    let mut spans = vec![Span::styled(
        format!("dupscanner · {view} · {}", app.root_path.display()),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )];
    if app.is_scanning() {
        spans.push(Span::styled(
            format!("   scanning… {} files, {} groups", app.progress.scanned_count, app.groups.len()),
            Style::default().fg(Color::Yellow),
        ));
    }
    if app.filter.is_active() {
        spans.push(Span::styled(
            format!("   filter: {}", app.filter.describe()),
            Style::default().fg(Color::Magenta),
        ));
    }
    let header = Paragraph::new(Line::from(spans))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(header, area);
}

fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let (text, style) = match &app.input {
        Some(Input::Filter(t)) => (
            format!("Filter path contains: {t}▏   (Enter apply · Esc cancel)"),
            Style::default().fg(Color::Magenta),
        ),
        Some(Input::Rename { text, .. }) => (
            format!("Rename to: {text}▏   (Enter apply · Esc cancel)"),
            Style::default().fg(Color::Yellow),
        ),
        None => {
            if let Some(msg) = &app.status_message {
                (msg.clone(), Style::default().fg(Color::White))
            } else {
                let keys = match app.view_mode {
                    ViewMode::Duplicates => {
                        "j/k file · n/p group · Space mark · a/A suggested · o/O keeper · m more · c/C clear · d/D delete · r/N rename · e open · / z t filter · ? help · q quit"
                    }
                    ViewMode::Statistics => "Tab: next view · Esc: back · q: quit",
                    ViewMode::Help => "Tab or Esc: back · q: quit",
                };
                (
                    format!(
                        "{keys}   [{} marked, {}, via {}]",
                        app.marked.len(),
                        format_size(app.marked_bytes(), BINARY),
                        app.deleter.method().label()
                    ),
                    Style::default().fg(Color::Gray),
                )
            }
        }
    };
    let footer = Paragraph::new(text)
        .style(style)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(footer, area);
}

fn render_duplicates(f: &mut Frame, area: Rect, app: &mut App, pane: &mut ImagePane) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(area);
    render_groups_panel(f, chunks[0], app);

    let selected = app.selected_file().map(|f| f.path.clone());
    let show_pane = app.show_preview && selected.is_some();
    if show_pane {
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(chunks[1]);
        render_files_panel(f, right[0], app);
        render_preview_panel(f, right[1], app, pane, selected.as_deref().unwrap_or(std::path::Path::new("")));
    } else {
        render_files_panel(f, chunks[1], app);
    }
}

fn render_preview_panel(f: &mut Frame, area: Rect, app: &mut App, pane: &mut ImagePane, path: &std::path::Path) {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Preview · {} · {} · v hides ", truncate_middle(&name, 40), pane.protocol_name()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width < 4 || inner.height < 2 {
        return;
    }

    let placeholder = |f: &mut Frame, text: String, color: Color| {
        let p = Paragraph::new(text)
            .style(Style::default().fg(color))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true });
        f.render_widget(p, inner);
    };

    if pane.picker.is_none() {
        placeholder(f, "Image preview disabled (DUPSCANNER_NO_IMAGES is set). Press e to open the file.".into(), Color::DarkGray);
        return;
    }

    match app.previewer.get(path) {
        Preview::Unsupported => {
            let kind = crate::filters::FileKind::of(path);
            placeholder(
                f,
                format!("No terminal preview for {} files. Press e to open with the default application.", kind.label()),
                Color::DarkGray,
            );
        }
        Preview::Loading => placeholder(f, "Decoding…".into(), Color::Yellow),
        Preview::Failed(e) => placeholder(f, format!("Could not decode image: {e}"), Color::Red),
        Preview::Ready(img) => {
            let needs_new = match &pane.current {
                Some((p, _)) => p != path,
                None => true,
            };
            if needs_new {
                if let Some(picker) = &pane.picker {
                    let protocol = picker.new_resize_protocol((*img).clone());
                    pane.current = Some((path.to_path_buf(), protocol));
                }
            }
            if let Some((_, protocol)) = &mut pane.current {
                let widget = StatefulImage::default().resize(Resize::Fit(None));
                f.render_stateful_widget(widget, inner, protocol);
            }
        }
    }
}

fn render_groups_panel(f: &mut Frame, area: Rect, app: &App) {
    if app.visible.is_empty() {
        let msg = if app.is_scanning() {
            "Scanning… no duplicates yet"
        } else if app.filter.is_active() {
            "No groups match the filter (x clears it)"
        } else {
            "No duplicates found"
        };
        let text = Paragraph::new(msg)
            .style(Style::default().fg(Color::Green))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Groups"));
        f.render_widget(text, area);
        return;
    }

    let rows = area.height.saturating_sub(2) as usize;
    let start = app
        .current_group_index
        .saturating_sub(rows / 2)
        .min(app.visible.len().saturating_sub(rows));
    let items: Vec<ListItem> = app
        .visible
        .iter()
        .enumerate()
        .skip(start)
        .take(rows.max(1))
        .map(|(i, &gi)| {
            let group = &app.groups[gi];
            let marked = app.marked_in_group(group);
            let style = if i == app.current_group_index {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let mark = if marked > 0 { format!(" [{marked}✗]") } else { String::new() };
            ListItem::new(format!(
                "{} × {}  ({} wasted){}",
                group.file_count(),
                format_size(group.file_size(), BINARY),
                format_size(group.wasted_space, BINARY),
                mark
            ))
            .style(style)
        })
        .collect();

    let title = if app.filter.is_active() {
        format!(
            "Groups ({}/{} of {}) · {} wasted",
            app.current_group_index + 1,
            app.visible.len(),
            app.groups.len(),
            format_size(app.total_wasted(), BINARY)
        )
    } else {
        format!(
            "Groups ({}/{}) · {} wasted",
            app.current_group_index + 1,
            app.visible.len(),
            format_size(app.total_wasted(), BINARY)
        )
    };
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

fn render_files_panel(f: &mut Frame, area: Rect, app: &App) {
    let Some(group) = app.current_group() else {
        let text = Paragraph::new("No group selected")
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title("Files"));
        f.render_widget(text, area);
        return;
    };
    let analysis = app.current_analysis();
    let width = area.width.saturating_sub(4) as usize;

    let items: Vec<ListItem> = group
        .files
        .iter()
        .enumerate()
        .map(|(i, file)| {
            let selected = i == app.selected_file_index;
            let marked = app.is_marked(&file.path);
            let is_keeper = analysis.as_ref().and_then(|a| a.keeper) == Some(i);
            let suggestion = analysis.as_ref().and_then(|a| a.files.get(i));

            let checkbox = if marked {
                Span::styled("[✗] ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
            } else {
                Span::raw("[ ] ")
            };
            let path_style = if selected {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else if marked {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            let path_text = truncate_middle(&file.path.display().to_string(), width.saturating_sub(4));
            let first = Line::from(vec![checkbox, Span::styled(path_text, path_style)]);

            let modified: DateTime<Local> = file.modified.into();
            let mut detail = vec![Span::styled(
                format!("    modified {}", modified.format("%Y-%m-%d %H:%M")),
                Style::default().fg(Color::DarkGray),
            )];
            if is_keeper {
                detail.push(Span::styled(
                    "  · KEEP",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ));
            }
            if let Some(s) = suggestion {
                if !s.reasons.is_empty() {
                    let color = if s.score > 0 { Color::Magenta } else { Color::Blue };
                    detail.push(Span::styled(
                        format!("  · {} ({})", s.explain(), s.score),
                        Style::default().fg(color),
                    ));
                }
            }
            ListItem::new(vec![first, Line::from(detail)])
        })
        .collect();

    let canonical = crate::naming::canonical_name(group)
        .map(|c| format!(" · original name: {}{}", c.name, if c.existing.is_none() { " (N restores it)" } else { "" }))
        .unwrap_or_default();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(format!(
        "Files · {} × {} · hash {}{canonical}",
        group.file_count(),
        format_size(group.file_size(), BINARY),
        group.hash.chars().take(12).collect::<String>()
    )));
    f.render_widget(list, area);
}

fn render_statistics(f: &mut Frame, area: Rect, app: &App, pane: &ImagePane) {
    let label = |s: &str| Span::styled(format!("  {s:<18}"), Style::default().fg(Color::Gray));
    let value = |s: String, c: Color| Span::styled(s, Style::default().fg(c));
    let heading = |s: &'static str| Line::from(Span::styled(s, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)));

    let status = if app.is_scanning() { ("In progress", Color::Yellow) } else { ("Complete", Color::Green) };
    let elapsed = app.scan_elapsed();
    let rate = if elapsed.as_secs_f64() > 0.0 {
        app.progress.scanned_count as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    let mut lines = vec![
        Line::from(""),
        heading("Scan"),
        Line::from(vec![label("Location"), value(app.root_path.display().to_string(), Color::White)]),
        Line::from(vec![label("Status"), value(status.0.into(), status.1)]),
        Line::from(vec![label("Elapsed"), value(format!("{:.1}s ({rate:.0} files/s)", elapsed.as_secs_f64()), Color::White)]),
        Line::from(vec![label("Files scanned"), value(app.progress.scanned_count.to_string(), Color::Green)]),
        Line::from(vec![label("Bytes scanned"), value(format_size(app.progress.total_size, BINARY), Color::White)]),
        Line::from(vec![label("Unreadable"), value(app.progress.skipped.to_string(), Color::DarkGray)]),
        Line::from(""),
        heading("Duplicates"),
        Line::from(vec![label("Groups"), value(app.groups.len().to_string(), Color::Yellow)]),
        Line::from(vec![label("Visible"), value(format!("{} ({})", app.visible.len(), app.filter.describe()), Color::Magenta)]),
        Line::from(vec![label("Duplicate files"), value(app.total_duplicate_files().to_string(), Color::Yellow)]),
        Line::from(vec![label("Wasted space"), value(format_size(app.total_wasted(), BINARY), Color::Red)]),
        Line::from(vec![label("Marked"), value(format!("{} files, {}", app.marked.len(), format_size(app.marked_bytes(), BINARY)), Color::Magenta)]),
        Line::from(vec![label("Deleted this run"), value(format!("{} files, {}", app.total_deleted, format_size(app.total_freed, BINARY)), Color::Green)]),
        Line::from(""),
        heading("Safety"),
        Line::from(vec![label("Delete method"), value(app.deleter.method().description().into(), Color::White)]),
        Line::from(vec![label("Image preview"), value(pane.protocol_name().into(), Color::White)]),
        Line::from(vec![
            label("Database"),
            value(
                app.db_path().map(|p| p.display().to_string()).unwrap_or_else(|| "not recorded".into()),
                Color::White,
            ),
        ]),
    ];
    if let Some(cfg) = &app.config {
        lines.push(Line::from(""));
        lines.push(heading("Settings"));
        lines.push(Line::from(vec![label("Min size"), value(format_size(cfg.min_size, BINARY), Color::White)]));
        lines.push(Line::from(vec![
            label("Max size"),
            value(cfg.max_size.map_or("unlimited".to_string(), |s| format_size(s, BINARY)), Color::White),
        ]));
        lines.push(Line::from(vec![label("Exclusions"), value(format!("{} patterns", cfg.exclude_patterns.len()), Color::White)]));
    }

    let stats = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Statistics"))
        .wrap(Wrap { trim: false });
    f.render_widget(stats, area);
}

fn render_help(f: &mut Frame, area: Rect, app: &App) {
    let h = |s: &'static str| Line::from(Span::styled(s, Style::default().add_modifier(Modifier::BOLD)));
    let lines = vec![
        Line::from(""),
        h("Navigation"),
        Line::from("  j / ↓          next file          k / ↑          previous file"),
        Line::from("  n / → / PgDn   next group         p / ← / PgUp   previous group"),
        Line::from("  g / Home       first group        G / End        last group"),
        Line::from(""),
        h("Marking"),
        Line::from("  Space          toggle mark on the selected file"),
        Line::from("  a / A          mark files that look like copies (group / all matching groups)"),
        Line::from("  o / O          mark everything except the keeper (group / all matching groups)"),
        Line::from("  m              more rules: all but oldest, newest, shortest or longest path"),
        Line::from("  c / C          clear marks (group / all)"),
        Line::from(""),
        h("Files"),
        Line::from("  r              rename the selected file (stays in the same folder)"),
        Line::from("  N              rename the keeper to the group's original name (e.g. drop \" (1)\")"),
        Line::from("  e / Enter      open the selected file with its default application"),
        Line::from("  v              show or hide the image preview pane (JPEG, PNG, GIF, WebP, BMP, TIFF)"),
        Line::from(""),
        h("Filtering"),
        Line::from("  /              filter groups by a path substring"),
        Line::from("  z              cycle the size filter        t    cycle the type filter"),
        Line::from("  x              clear all filters"),
        Line::from(""),
        h("Deleting"),
        Line::from("  d / D          delete marked files (group / all matching groups), after confirmation"),
        Line::from(format!("                 method: {}", app.deleter.method().description())),
        Line::from("                 dupscanner never deletes the last copy in a group."),
        Line::from(""),
        h("Views"),
        Line::from("  Tab            cycle Duplicates / Statistics / Help"),
        Line::from("  ?              toggle this help          q    quit"),
        Line::from(""),
        Line::from(Span::styled(
            "KEEP marks the file the heuristics would keep: fewest copy signals, then shallowest path, then oldest.",
            Style::default().fg(Color::Green),
        )),
    ];
    let help = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Help"));
    f.render_widget(help, area);
}

fn render_mark_menu(f: &mut Frame) {
    let area = centered_rect(60, 45, f.area());
    f.render_widget(Clear, area);
    let mut lines = vec![
        Line::from(Span::styled(
            "Mark files for deletion",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "letter: this group    Shift+letter: all matching groups",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
    ];
    for mode in SelectMode::ALL {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {}  ", mode.shortcut()),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::raw(mode.label()),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Each rule replaces existing marks in the groups it touches and never marks every copy.",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::from(Span::styled("Esc: close", Style::default().fg(Color::Gray))));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Auto-select ")
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn render_confirm(f: &mut Frame, app: &App, pending: &crate::app::PendingConfirm) {
    let area = centered_rect(70, 50, f.area());
    f.render_widget(Clear, area);

    let scope = match pending.scope {
        Scope::CurrentGroup => "this group",
        Scope::AllGroups => {
            if app.filter.is_active() {
                "all matching groups"
            } else {
                "all groups"
            }
        }
    };
    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                "Delete {} file(s) from {scope}, freeing {}?",
                pending.plan.len(),
                format_size(pending.plan.bytes(), BINARY)
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            format!("Method: {}", app.deleter.method().description()),
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
    ];
    let max_listed = area.height.saturating_sub(8) as usize;
    let width = area.width.saturating_sub(6) as usize;
    for item in pending.plan.items.iter().take(max_listed) {
        lines.push(Line::from(Span::styled(
            format!("  ✗ {}", truncate_middle(&item.path.display().to_string(), width)),
            Style::default().fg(Color::Red),
        )));
    }
    if pending.plan.len() > max_listed {
        lines.push(Line::from(format!("  … and {} more", pending.plan.len() - max_listed)));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "y / Enter: delete      n / Esc: cancel",
        Style::default().fg(Color::Gray),
    )));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Confirm deletion ")
        .style(Style::default().bg(Color::Black));
    f.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// Shorten a path for display, keeping both ends. Works on characters, so
/// non-ASCII paths never split a code point.
pub fn truncate_middle(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if max < 8 || chars.len() <= max {
        return s.to_string();
    }
    let keep = max - 1;
    let head = keep / 3;
    let tail = keep - head;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(chars[chars.len() - tail..].iter());
    out
}

#[cfg(test)]
mod tests {
    use super::truncate_middle;

    #[test]
    fn truncation_handles_multibyte() {
        let s = "/Users/jörg/Bilder/Käse/🙂🙂🙂🙂/über/lang/pfad/datei.jpg";
        let t = truncate_middle(s, 24);
        assert!(t.chars().count() <= 24);
        assert!(t.contains('…'));
        assert!(t.ends_with("datei.jpg"));
    }

    #[test]
    fn short_strings_untouched() {
        assert_eq!(truncate_middle("abc", 10), "abc");
    }
}
