mod app;
mod backup;
mod database;
mod deletion;
mod demo;
mod duplicates;
mod edits;
mod engine;
mod filters;
mod naming;
mod preview;
mod paths;
mod report;
mod scanner;
mod selection;
mod suggestions;
mod tui;
mod web;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use colored::Colorize;
use humansize::{format_size, BINARY};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::backup::BackupManager;
use crate::database::ScanDatabase;
use crate::deletion::{plan_deletions, DeleteMethod, Deleter};
use crate::engine::ScanSession;
use crate::scanner::ScanConfig;
use crate::suggestions::SuggestionEngine;

#[derive(Parser)]
#[command(name = "dupscanner", version, about = "Find and remove duplicate files, safely")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DeleteMethodArg {
    /// Move to the system trash / recycle bin (default)
    Trash,
    /// Copy into dupscanner's backup store, then delete (see `restore`)
    Backup,
    /// Delete permanently, no undo
    Permanent,
}

impl From<DeleteMethodArg> for DeleteMethod {
    fn from(v: DeleteMethodArg) -> Self {
        match v {
            DeleteMethodArg::Trash => DeleteMethod::Trash,
            DeleteMethodArg::Backup => DeleteMethod::Backup,
            DeleteMethodArg::Permanent => DeleteMethod::Permanent,
        }
    }
}

#[derive(Args, Clone, Debug)]
struct ScanArgs {
    /// Directory to scan
    #[arg(value_name = "PATH", default_value = ".")]
    path: PathBuf,

    /// Ignore files smaller than this (e.g. 1, 10KB, 5MB)
    #[arg(short = 'm', long, default_value = "1", value_parser = parse_size)]
    min_size: u64,

    /// Ignore files larger than this (e.g. 2GB)
    #[arg(short = 'x', long, value_parser = parse_size)]
    max_size: Option<u64>,

    /// Extra glob patterns to exclude (repeatable). Bare names prune whole directories.
    #[arg(short = 'e', long = "exclude", value_name = "PATTERN")]
    exclude: Vec<String>,

    /// Do not apply the built-in exclusions (.git, node_modules, target, ...)
    #[arg(long)]
    no_default_excludes: bool,

    /// How deletions are performed
    #[arg(long, value_enum, default_value_t = DeleteMethodArg::Trash)]
    delete_method: DeleteMethodArg,

    /// Scan database to record results in (default: ~/.local/share/dupscanner/scans.db)
    #[arg(long, value_name = "FILE")]
    db: Option<PathBuf>,

    /// Do not record this scan in the database
    #[arg(long)]
    no_record: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan a directory and review duplicates in the terminal UI
    Scan {
        #[command(flatten)]
        args: ScanArgs,

        /// Non-interactive: keep the best file in each group and delete the rest
        #[arg(long)]
        yolo: bool,

        /// Non-interactive: print the duplicate groups as JSON and exit
        #[arg(long)]
        json: bool,

        /// With --yolo: after deleting, rename each keeper to the group's
        /// original name when its copies carried markers like " (1)"
        #[arg(long, requires = "yolo")]
        rename_keepers: bool,

        /// With --yolo: delete copies as soon as a group is confirmed instead
        /// of after the whole scan. Safe to interrupt with Ctrl+C: files
        /// already removed stay removed, and running again continues.
        #[arg(long, requires = "yolo")]
        stream: bool,
    },

    /// Scan a directory and review duplicates in a local web UI
    Serve {
        #[command(flatten)]
        args: ScanArgs,

        /// Port to listen on (127.0.0.1 only); 0 picks a free port
        #[arg(short, long, default_value = "8080")]
        port: u16,

        /// Do not open the browser automatically
        #[arg(long)]
        no_open: bool,

        /// Serve a recorded scan from the database instead of scanning
        #[arg(long, value_name = "ID")]
        scan_id: Option<i64>,
    },

    /// List recorded scans
    History {
        /// Number of scans to show
        #[arg(short = 'n', long, default_value = "10")]
        count: usize,

        #[arg(long, value_name = "FILE")]
        db: Option<PathBuf>,
    },

    /// Review a recorded scan (terminal UI by default)
    View {
        scan_id: i64,

        /// Print the groups as JSON instead of opening the UI
        #[arg(long)]
        json: bool,

        /// Print a plain-text listing instead of opening the UI
        #[arg(long)]
        plain: bool,

        #[arg(long, value_enum, default_value_t = DeleteMethodArg::Trash)]
        delete_method: DeleteMethodArg,

        #[arg(long, value_name = "FILE")]
        db: Option<PathBuf>,
    },

    /// Delete a recorded scan from the database
    Forget {
        scan_id: i64,
        #[arg(long, value_name = "FILE")]
        db: Option<PathBuf>,
    },

    /// Manage files deleted with `--delete-method backup`
    Restore {
        #[command(subcommand)]
        action: RestoreAction,
    },

    /// Generate demo data with known duplicates
    Demo {
        /// Directory to create test data in
        #[arg(value_name = "PATH", default_value = "/tmp/dupscanner-demo")]
        path: PathBuf,

        /// Number of unique files to create
        #[arg(short = 'n', long, default_value = "10")]
        num_files: usize,

        /// Number of duplicates per file
        #[arg(short = 'd', long, default_value = "3")]
        duplicates: usize,
    },
}

#[derive(Subcommand)]
enum RestoreAction {
    /// List backed-up files
    List,
    /// Restore one backup (by backup path or original path)
    File {
        /// Backup file path, or the original path of the deleted file
        path: PathBuf,
        /// Restore to a different location
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Delete backups older than N days
    Clean {
        #[arg(long, default_value = "30")]
        older_than: i64,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{} {e:#}", "error:".red().bold());
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Scan {
            args,
            yolo,
            json,
            rename_keepers,
            stream,
        } => {
            if yolo && json {
                bail!("--yolo and --json cannot be combined");
            }
            let config = build_config(&args)?;
            let (deleter, database) = build_deleter_and_db(args.delete_method.into(), args.db.clone(), args.no_record)?;
            if json {
                scan_json(config)
            } else if yolo && stream {
                yolo_stream(config, deleter, database, rename_keepers)
            } else if yolo {
                yolo_scan(config, deleter, database, rename_keepers)
            } else {
                let mut app = app::App::new_scan(config, deleter, database);
                tui::run(&mut app)?;
                print_exit_summary(&app);
                Ok(())
            }
        }
        Commands::Serve {
            args,
            port,
            no_open,
            scan_id,
        } => {
            let config = build_config(&args)?;
            let db_path = if args.no_record { None } else { Some(resolve_db_path(args.db.clone())?) };
            web::serve(web::ServeOptions {
                config,
                port,
                open_browser: !no_open,
                delete_method: args.delete_method.into(),
                db_path,
                scan_id,
            })
        }
        Commands::History { count, db } => show_history(count, db),
        Commands::View {
            scan_id,
            json,
            plain,
            delete_method,
            db,
        } => view_scan(scan_id, json, plain, delete_method.into(), db),
        Commands::Forget { scan_id, db } => {
            let db = ScanDatabase::open(&resolve_db_path(db)?)?;
            if db.get_scan_info(scan_id).is_none() {
                bail!("Scan {scan_id} not found");
            }
            db.delete_scan(scan_id)?;
            println!("Forgot scan {scan_id}");
            Ok(())
        }
        Commands::Restore { action } => restore(action),
        Commands::Demo {
            path,
            num_files,
            duplicates,
        } => demo::generate_demo_data(&path, num_files, duplicates),
    }
}

// ---------------------------------------------------------------------------

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim().to_ascii_uppercase();
    let (num, unit) = match s.find(|c: char| c.is_ascii_alphabetic()) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s.as_str(), ""),
    };
    let value: f64 = num.trim().parse().map_err(|_| format!("invalid size: {s}"))?;
    let mult: f64 = match unit.trim_end_matches('B') {
        "" => 1.0,
        "K" | "KI" => 1024.0,
        "M" | "MI" => 1024.0 * 1024.0,
        "G" | "GI" => 1024.0 * 1024.0 * 1024.0,
        "T" | "TI" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return Err(format!("unknown size unit: {unit}")),
    };
    Ok((value * mult) as u64)
}

fn build_config(args: &ScanArgs) -> Result<ScanConfig> {
    let root = args
        .path
        .canonicalize()
        .with_context(|| format!("Cannot access {}", args.path.display()))?;
    if !root.is_dir() {
        bail!("{} is not a directory", root.display());
    }
    let exclude_patterns = if args.no_default_excludes {
        args.exclude.clone()
    } else {
        let mut p = scanner::get_default_exclusions();
        p.extend(args.exclude.iter().cloned());
        p
    };
    Ok(ScanConfig {
        root_path: root,
        min_size: args.min_size,
        max_size: args.max_size,
        exclude_patterns,
        batch_size: 1000,
    })
}

fn resolve_db_path(db: Option<PathBuf>) -> Result<PathBuf> {
    match db {
        Some(p) => Ok(p),
        None => paths::default_db_path(),
    }
}

fn build_deleter(method: DeleteMethod) -> Result<Deleter> {
    let backup = if method == DeleteMethod::Backup {
        Some(BackupManager::open_default()?)
    } else {
        None
    };
    Ok(Deleter::new(method, backup))
}

fn build_deleter_and_db(
    method: DeleteMethod,
    db: Option<PathBuf>,
    no_record: bool,
) -> Result<(Deleter, Option<ScanDatabase>)> {
    let deleter = build_deleter(method)?;
    let database = if no_record {
        None
    } else {
        Some(ScanDatabase::open(&resolve_db_path(db)?)?)
    };
    Ok((deleter, database))
}

fn print_exit_summary(app: &app::App) {
    if app.total_deleted > 0 {
        println!(
            "Deleted {} file(s), freed {} (via {}).",
            app.total_deleted,
            format_size(app.total_freed, BINARY),
            app.deleter.method().label()
        );
    }
    if let Some(id) = app.scan_id {
        println!("Scan recorded as #{id}. Review later with `dupscanner view {id}`.");
    }
}

// ---------------------------------------------------------------------------

fn scan_json(config: ScanConfig) -> Result<()> {
    let root = config.root_path.clone();
    let session = ScanSession::start(config);
    let (finder, progress, elapsed) = session
        .run_to_completion(|p, groups| {
            eprint!("\r  scanned {} files, {} groups so far", p.scanned_count, groups);
        })
        .context("scan did not complete")?;
    eprintln!();
    let report = report::scan_report(&root, finder.groups(), &progress, Some(elapsed));
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer_pretty(&mut out, &report)?;
    let _ = writeln!(out);
    Ok(())
}

fn yolo_scan(
    config: ScanConfig,
    mut deleter: Deleter,
    mut database: Option<ScanDatabase>,
    rename_keepers: bool,
) -> Result<()> {
    println!("{}", "YOLO mode: keep the best copy in each group, delete the rest".bold().bright_yellow());
    println!("   {}: {}", "Path".bold(), config.root_path.display());
    println!("   {}: {}", "Delete method".bold(), deleter.method().description());
    println!();

    let root = config.root_path.clone();
    let session = ScanSession::start(config);
    let (finder, progress, elapsed) = session
        .run_to_completion(|p, groups| {
            print!("\r   scanned {} files, {} duplicate groups", p.scanned_count, groups);
            let _ = std::io::stdout().flush();
        })
        .context("scan did not complete")?;
    println!();
    println!(
        "   {} {} files in {:.1}s, {} duplicate groups",
        "Scanned".green().bold(),
        progress.scanned_count,
        elapsed.as_secs_f64(),
        finder.groups().len()
    );
    println!();

    let groups = finder.groups().to_vec();
    let scan_id = match database.as_mut() {
        Some(db) => Some(db.record_completed_scan(&root, progress.scanned_count, &groups)?),
        None => None,
    };

    let mut wanted = HashSet::new();
    let mut keeper_renames: Vec<(PathBuf, String)> = Vec::new();
    for group in &groups {
        let analysis = SuggestionEngine::analyze(&group.files);
        let keeper = analysis.keeper.unwrap_or(0);
        if rename_keepers {
            if let Some(r) = naming::suggested_rename(group, keeper) {
                keeper_renames.push(r);
            }
        }
        println!(
            "{} {} × {}  keeping {}",
            "•".dimmed(),
            group.file_count(),
            format_size(group.file_size(), BINARY),
            group.files[keeper].path.display().to_string().green()
        );
        for (i, f) in group.files.iter().enumerate() {
            if i != keeper {
                wanted.insert(f.path.clone());
            }
        }
    }
    if wanted.is_empty() {
        println!("{}", "No duplicates found.".green());
        return Ok(());
    }

    let plan = plan_deletions(&groups, &wanted)?;
    let report = deleter.delete_planned(&plan);
    for (path, err) in report.failures() {
        eprintln!("   {} {}: {err}", "failed".yellow().bold(), path.display());
    }
    if let Some(db) = database.as_mut() {
        let _ = db.record_deletions(scan_id, &root, deleter.method().label(), &report);
    }

    let deleted = report.deleted_paths();
    let mut remaining: Vec<_> = groups
        .iter()
        .cloned()
        .map(|mut g| {
            g.remove_paths(&deleted);
            g
        })
        .filter(|g| !g.is_empty())
        .collect();

    let mut renamed = 0usize;
    for (path, new_name) in &keeper_renames {
        // Only rename when every marked copy of that group is gone, so the
        // clean name cannot collide with a copy that failed to delete.
        if remaining.iter().any(|g| g.files.iter().any(|f| &f.path == path)) {
            continue;
        }
        let mut solo = vec![crate::duplicates::DuplicateGroup::new(
            String::new(),
            groups
                .iter()
                .flat_map(|g| g.files.iter())
                .filter(|f| &f.path == path)
                .cloned()
                .collect(),
        )];
        match edits::rename_in_groups(&mut solo, path, new_name) {
            Ok((new, _)) => {
                println!("   {} {} -> {}", "renamed".cyan().bold(), path.display(), new.display());
                renamed += 1;
            }
            Err(e) => eprintln!("   {} {}: {e}", "not renamed".yellow().bold(), path.display()),
        }
    }
    if rename_keepers && renamed > 0 {
        remaining.retain(|g| !g.is_empty());
    }

    if let (Some(db), Some(id)) = (database.as_mut(), scan_id) {
        let _ = db.save_groups(id, &remaining);
    }

    println!();
    println!(
        "{} deleted {} file(s), freed {}, {} failed.",
        "Done:".bold().bright_green(),
        report.deleted_count(),
        format_size(report.bytes_freed(), BINARY),
        report.failed_count()
    );
    match deleter.method() {
        DeleteMethod::Trash => println!("Files are in the system trash."),
        DeleteMethod::Backup => println!("Files are backed up; see `dupscanner restore list`."),
        DeleteMethod::Permanent => {}
    }
    Ok(())
}


/// Streaming yolo: delete the copies of each group as soon as the group is
/// confirmed by full hashes, while the walk continues. Ctrl+C stops after
/// the deletion in flight; everything removed so far stays removed, and the
/// same command can simply be run again.
fn yolo_stream(
    config: ScanConfig,
    deleter: Deleter,
    database: Option<ScanDatabase>,
    rename_keepers: bool,
) -> Result<()> {
    use crate::engine::EngineEvent;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    println!("{}", "YOLO streaming mode: delete copies as groups are confirmed".bold().bright_yellow());
    println!("   {}: {}", "Path".bold(), config.root_path.display());
    println!("   {}: {}", "Delete method".bold(), deleter.method().description());
    println!("   {}", "Ctrl+C stops after the current deletion; run again later to continue.".dimmed());
    println!();

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || {
            if stop.swap(true, Ordering::SeqCst) {
                eprintln!("\nQuitting now.");
                std::process::exit(130);
            }
            eprintln!("\n{}", "Stopping after the current deletion… (Ctrl+C again to quit immediately)".yellow());
        })
        .context("could not install the Ctrl+C handler")?;
    }

    let root = config.root_path.clone();
    let session = ScanSession::start(config);
    let removed = session.removed_paths();
    let mut st = StreamState {
        root: root.clone(),
        scan_id: None,
        deleter,
        database,
        removed,
        stop: stop.clone(),
        edits: crate::edits::ScanEdits::default(),
        members: std::collections::HashMap::new(),
        groups_seen: HashSet::new(),
        progress: crate::scanner::ScanProgress::default(),
        total_deleted: 0,
        total_failed: 0,
        total_freed: 0,
    };
    if let Some(db) = st.database.as_mut() {
        st.scan_id = Some(db.start_scan(&root)?);
    }
    let started = std::time::Instant::now();
    let mut final_groups: Option<Vec<crate::duplicates::DuplicateGroup>> = None;

    let interrupted = loop {
        if stop.load(Ordering::SeqCst) {
            break true;
        }
        match session.next_timeout(Duration::from_millis(200)) {
            Ok(None) => continue,
            Err(()) => break false,
            Ok(Some(EngineEvent::Progress(p))) => {
                st.progress = p;
                st.print_status();
            }
            Ok(Some(EngineEvent::Groups(groups))) => st.handle_groups(groups),
            Ok(Some(EngineEvent::Complete { finder, progress: p, .. })) => {
                st.progress = p;
                st.handle_groups(finder.groups().to_vec());
                let mut groups = finder.groups().to_vec();
                st.edits.apply(&mut groups);
                final_groups = Some(groups);
                break stop.load(Ordering::SeqCst);
            }
        }
    };
    println!();

    // Rename keepers to the original name once their groups can no longer grow.
    let mut renamed = 0usize;
    if rename_keepers && !interrupted {
        for (hash, all) in &st.members {
            if !st.groups_seen.contains(hash) {
                continue;
            }
            let Some(canonical) = naming::canonical_name_of(all) else { continue };
            let survivors: Vec<&crate::scanner::FileInfo> = all.iter().filter(|f| f.path.exists()).collect();
            let [keeper] = survivors.as_slice() else { continue };
            let current = keeper.path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            if current.eq_ignore_ascii_case(&canonical.name) {
                continue;
            }
            let target = keeper.path.parent().unwrap_or(Path::new("")).join(&canonical.name);
            if target.exists() {
                continue;
            }
            let mut solo = vec![crate::duplicates::DuplicateGroup::new(String::new(), vec![(*keeper).clone()])];
            match edits::rename_in_groups(&mut solo, &keeper.path, &canonical.name) {
                Ok((new, _)) => {
                    println!("   {} {} -> {}", "renamed".cyan().bold(), keeper.path.display(), new.display());
                    renamed += 1;
                }
                Err(e) => eprintln!("   {} {}: {e}", "not renamed".yellow().bold(), keeper.path.display()),
            }
        }
    }

    let scan_id = st.scan_id;
    if let (Some(db), Some(id)) = (st.database.as_mut(), scan_id) {
        if let Some(groups) = &final_groups {
            let _ = db.save_groups(id, groups);
        }
        if interrupted {
            let _ = db.update_progress(id, st.progress.scanned_count, st.groups_seen.len());
        } else {
            let _ = db.complete_scan(id, st.progress.scanned_count, st.groups_seen.len());
        }
    }

    println!();
    if interrupted {
        println!("{}", "Interrupted.".bold().yellow());
    } else {
        println!("{}", "Done.".bold().bright_green());
    }
    println!(
        "   scanned {} files in {:.1}s, {} duplicate groups, deleted {} file(s), freed {}, {} failed{}",
        st.progress.scanned_count,
        started.elapsed().as_secs_f64(),
        st.groups_seen.len(),
        st.total_deleted,
        format_size(st.total_freed, BINARY),
        st.total_failed,
        if renamed > 0 { format!(", renamed {renamed} keeper(s)") } else { String::new() }
    );
    match st.deleter.method() {
        DeleteMethod::Trash => println!("   Files are in the system trash."),
        DeleteMethod::Backup => println!("   Files are backed up; see `dupscanner restore list`."),
        DeleteMethod::Permanent => {}
    }
    if interrupted {
        println!("   Files already removed stay removed. Run the same command again to continue.");
        if let Some(id) = scan_id {
            println!("   Deletions so far are recorded under scan #{id} (`dupscanner view {id} --plain`).");
        }
        std::process::exit(130);
    }
    Ok(())
}

/// Mutable state of a streaming yolo run.
struct StreamState {
    root: PathBuf,
    scan_id: Option<i64>,
    deleter: Deleter,
    database: Option<ScanDatabase>,
    removed: crate::engine::RemovedPaths,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    edits: crate::edits::ScanEdits,
    /// Every member ever seen per hash, for the original-name rename at the end.
    members: std::collections::HashMap<String, Vec<crate::scanner::FileInfo>>,
    groups_seen: HashSet<String>,
    progress: crate::scanner::ScanProgress,
    total_deleted: usize,
    total_failed: usize,
    total_freed: u64,
}

impl StreamState {
    fn print_status(&self) {
        print!(
            "\r   scanned {} files · {} groups · {} deleted · {} freed   ",
            self.progress.scanned_count,
            self.groups_seen.len(),
            self.total_deleted,
            format_size(self.total_freed, BINARY)
        );
        let _ = std::io::stdout().flush();
    }

    /// Delete the copies in every confirmed group of a snapshot, keeping the
    /// best file. A group that grows later is handled again: its keeper is
    /// re-chosen among the survivors and the new arrivals are removed.
    fn handle_groups(&mut self, mut groups: Vec<crate::duplicates::DuplicateGroup>) {
        use std::sync::atomic::Ordering;
        self.edits.apply(&mut groups);
        for group in &groups {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let all = self.members.entry(group.hash.clone()).or_default();
            for f in &group.files {
                if !all.iter().any(|m| m.path == f.path) {
                    all.push(f.clone());
                }
            }
            let analysis = SuggestionEngine::analyze(&group.files);
            let Some(keeper) = analysis.keeper else { continue };
            let wanted: HashSet<PathBuf> = group
                .files
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != keeper)
                .map(|(_, f)| f.path.clone())
                .collect();
            if wanted.is_empty() {
                continue;
            }
            let plan = match plan_deletions(std::slice::from_ref(group), &wanted) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("\n   {} {e}", "skipped".yellow().bold());
                    continue;
                }
            };
            let first_time = self.groups_seen.insert(group.hash.clone());
            let report = self.deleter.delete_planned(&plan);
            let deleted = report.deleted_paths();
            self.edits.record_deleted(deleted.iter().cloned());
            self.removed.add_all(deleted.iter().cloned());
            if let Some(db) = self.database.as_mut() {
                let _ = db.record_deletions(self.scan_id, &self.root, self.deleter.method().label(), &report);
            }
            self.total_deleted += report.deleted_count();
            self.total_failed += report.failed_count();
            self.total_freed += report.bytes_freed();

            println!(
                "\r{} {} × {}  keeping {}{}",
                "•".dimmed(),
                group.file_count(),
                format_size(group.file_size(), BINARY),
                group.files[keeper].path.display().to_string().green(),
                if first_time { "" } else { "  (more copies found)" }
            );
            for p in &deleted {
                println!("   {} {}", "deleted".red().bold(), p.display().to_string().dimmed());
            }
            for (p, e) in report.failures() {
                eprintln!("   {} {}: {e}", "failed".yellow().bold(), p.display());
            }
        }
        self.print_status();
    }
}

// ---------------------------------------------------------------------------

fn show_history(count: usize, db: Option<PathBuf>) -> Result<()> {
    let db_path = resolve_db_path(db)?;
    let db = ScanDatabase::open(&db_path)?;
    let scans = db.list_scans()?;
    if scans.is_empty() {
        println!("No recorded scans in {}", db_path.display());
        return Ok(());
    }
    println!("{}", format!("Recorded scans ({})", db_path.display()).bold().cyan());
    for scan in scans.iter().take(count) {
        let started = chrono::DateTime::from_timestamp(scan.start_time, 0)
            .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        let status = if scan.end_time.is_some() { "complete".green() } else { "incomplete".yellow() };
        let deleted = if scan.deleted_count > 0 {
            format!(
                ", {} deleted ({})",
                scan.deleted_count,
                format_size(scan.deleted_bytes.max(0) as u64, BINARY)
            )
        } else {
            String::new()
        };
        println!(
            "  {:>4}  {started}  {status:<10}  {} files, {} groups{deleted}  {}",
            format!("#{}", scan.id).bold(),
            scan.files_scanned,
            scan.groups_found,
            scan.root_path.display().to_string().dimmed()
        );
    }
    println!("\nReview one with `dupscanner view <id>` or `dupscanner serve --scan-id <id>`.");
    Ok(())
}

fn view_scan(scan_id: i64, json: bool, plain: bool, method: DeleteMethod, db: Option<PathBuf>) -> Result<()> {
    let db_path = resolve_db_path(db)?;
    let mut db = ScanDatabase::open(&db_path)?;
    let info = db.get_scan_info(scan_id).with_context(|| format!("Scan {scan_id} not found"))?;
    let mut groups = db.load_duplicate_groups(scan_id)?;

    // Drop files that no longer exist so the view reflects the disk.
    let mut changed = false;
    for g in &mut groups {
        let gone: std::collections::HashSet<PathBuf> =
            g.files.iter().filter(|f| !f.path.exists()).map(|f| f.path.clone()).collect();
        if !gone.is_empty() {
            g.remove_paths(&gone);
            changed = true;
        }
    }
    groups.retain(|g| !g.is_empty());
    if changed {
        let _ = db.save_groups(scan_id, &groups);
    }

    let progress = scanner::ScanProgress {
        scanned_count: info.files_scanned.max(0) as usize,
        ..Default::default()
    };

    if json {
        let report = report::scan_report(&info.root_path, &groups, &progress, None);
        serde_json::to_writer_pretty(std::io::stdout().lock(), &report)?;
        println!();
        return Ok(());
    }
    if plain {
        println!("{}", format!("Scan #{scan_id}: {}", info.root_path.display()).bold().cyan());
        println!("{} files scanned, {} duplicate groups\n", info.files_scanned, groups.len());
        for (i, g) in groups.iter().enumerate() {
            let analysis = SuggestionEngine::analyze(&g.files);
            println!(
                "{}. {} × {} ({} wasted)",
                i + 1,
                g.file_count(),
                format_size(g.file_size(), BINARY),
                format_size(g.wasted_space, BINARY).red()
            );
            for (j, f) in g.files.iter().enumerate() {
                let marker = if analysis.keeper == Some(j) { "keep".green() } else { "    ".normal() };
                println!("   {marker} {}", f.path.display());
            }
        }
        let wasted: u64 = groups.iter().map(|g| g.wasted_space).sum();
        println!("\nTotal wasted space: {}", format_size(wasted, BINARY).bright_red());
        let deletions = db.deletions_for_scan(scan_id)?;
        if !deletions.is_empty() {
            println!("\n{}", format!("Deleted during this scan ({}):", deletions.len()).bold());
            for (path, size, method, at) in deletions.iter().take(200) {
                let when = chrono::DateTime::from_timestamp(*at, 0)
                    .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                println!("   {when}  {:>10}  {method:<9} {}", format_size(*size, BINARY), path.display());
            }
            if deletions.len() > 200 {
                println!("   … and {} more", deletions.len() - 200);
            }
        }
        return Ok(());
    }

    let deleter = build_deleter(method)?;
    let mut app = app::App::from_recorded(
        info.root_path.clone(),
        groups,
        progress.scanned_count,
        deleter,
        Some(db),
        Some(scan_id),
    );
    tui::run(&mut app)?;
    print_exit_summary(&app);
    Ok(())
}

fn restore(action: RestoreAction) -> Result<()> {
    let mut manager = BackupManager::open_default()?;
    match action {
        RestoreAction::List => {
            let records = manager.list_backups();
            if records.is_empty() {
                println!("No backups in {}", manager.backup_dir().display());
                return Ok(());
            }
            println!("{}", format!("Backups in {}", manager.backup_dir().display()).bold().cyan());
            for r in records {
                println!(
                    "  {}  {:>10}  {}",
                    r.timestamp.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S"),
                    format_size(r.size, BINARY),
                    r.original_path.display()
                );
                println!("      backup: {}", r.backup_path.display().to_string().dimmed());
            }
            println!(
                "\n{} backups, {} total. Restore with `dupscanner restore file <path>`.",
                records.len(),
                format_size(manager.total_backup_size(), BINARY)
            );
        }
        RestoreAction::File { path, to } => {
            let backup_path = if manager.list_backups().iter().any(|r| r.backup_path == path) {
                path.clone()
            } else {
                let matches = manager.find_by_original(&path);
                match matches.len() {
                    0 => bail!("No backup found for {}", path.display()),
                    1 => matches[0].backup_path.clone(),
                    n => {
                        eprintln!("{n} backups exist for {}; pass one of these backup paths:", path.display());
                        for m in matches {
                            eprintln!("  {}", m.backup_path.display());
                        }
                        bail!("ambiguous restore");
                    }
                }
            };
            let restored = manager.restore_file(&backup_path, to.as_deref())?;
            println!("Restored {}", restored.display());
        }
        RestoreAction::Clean { older_than } => {
            let n = manager.clean_old_backups(older_than)?;
            println!("Removed {n} backup(s) older than {older_than} days");
        }
    }
    Ok(())
}

#[allow(dead_code)]
fn _assert_path_types(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::parse_size;

    #[test]
    fn sizes_parse() {
        assert_eq!(parse_size("1").unwrap(), 1);
        assert_eq!(parse_size("10KB").unwrap(), 10 * 1024);
        assert_eq!(parse_size("5mb").unwrap(), 5 * 1024 * 1024);
        assert_eq!(parse_size("1.5GiB").unwrap(), (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert!(parse_size("ten").is_err());
    }
}
