mod app;
mod backup;
mod database;
mod demo;
mod duplicates;
mod scanner;
mod state;
mod suggestions;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "dupscanner")]
#[command(about = "Intelligent duplicate file scanner with TUI", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan directories for duplicate files
    Scan {
        /// Directory to scan
        #[arg(value_name = "PATH", default_value = ".")]
        path: PathBuf,

        /// Minimum file size to scan (in bytes)
        #[arg(short, long, default_value = "1")]
        min_size: u64,

        /// Maximum file size to scan (in bytes, 0 = unlimited)
        #[arg(short = 'x', long, default_value = "0")]
        max_size: u64,

        /// Save scan state for pause/resume
        #[arg(short, long)]
        save_state: bool,

        /// Resume from previous scan
        #[arg(short, long)]
        resume: bool,

        /// YOLO mode: automatically delete duplicates in real-time (keeps best file)
        #[arg(long)]
        yolo: bool,

        /// Exclude patterns (glob format, can be specified multiple times)
        #[arg(short = 'e', long = "exclude", value_name = "PATTERN")]
        exclude: Vec<String>,

        /// Disable default exclusions (like .git/, node_modules/, etc.)
        #[arg(long)]
        no_default_excludes: bool,

        /// Use backup instead of trash (backup creates copies, trash moves to system recycle bin)
        #[arg(long)]
        use_backup: bool,
    },

    /// Resume a previous scan
    Resume {
        /// State file to resume from
        #[arg(value_name = "STATE_FILE")]
        state_file: Option<PathBuf>,
    },

    /// List saved scan states
    List,

    /// List scan history from database
    History {
        /// Number of scans to show
        #[arg(short = 'n', long, default_value = "10")]
        count: usize,

        /// Database file (optional, uses default if not specified)
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// View a completed scan from database
    View {
        /// Scan ID to view
        scan_id: i64,

        /// Database file (optional, uses default if not specified)
        #[arg(long)]
        db: Option<PathBuf>,
    },

    /// Generate demo test data with duplicates
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

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Scan {
            path,
            min_size,
            max_size,
            save_state,
            resume,
            yolo,
            exclude,
            no_default_excludes,
            use_backup,
        } => {
            // Merge default exclusions with user-provided ones
            let exclude_patterns = if no_default_excludes {
                exclude
            } else {
                let mut patterns = scanner::get_default_exclusions();
                patterns.extend(exclude);
                patterns
            };

            if yolo {
                // YOLO mode: non-interactive real-time duplicate removal
                yolo_scan(path, min_size, max_size, exclude_patterns, use_backup).await?;
            } else if resume {
                // Try to resume from default state file
                let state_file = state::get_default_state_file(&path)?;
                if state_file.exists() {
                    resume_scan(state_file).await?;
                } else {
                    eprintln!("No previous scan state found for this directory");
                    start_new_scan(path, min_size, max_size, save_state, exclude_patterns).await?;
                }
            } else {
                start_new_scan(path, min_size, max_size, save_state, exclude_patterns).await?;
            }
        }
        Commands::Resume { state_file } => {
            let state_file = if let Some(sf) = state_file {
                sf
            } else {
                state::get_latest_state_file()?
            };
            resume_scan(state_file).await?;
        }
        Commands::List => {
            list_saved_states()?;
        }
        Commands::History { count, db } => {
            show_scan_history(count, db).await?;
        }
        Commands::View { scan_id, db } => {
            view_scan(scan_id, db).await?;
        }
        Commands::Demo { path, num_files, duplicates } => {
            demo::generate_demo_data(&path, num_files, duplicates)?;
        }
    }

    Ok(())
}

async fn start_new_scan(
    path: PathBuf,
    min_size: u64,
    max_size: u64,
    save_state: bool,
    exclude_patterns: Vec<String>,
) -> Result<()> {
    let config = scanner::ScanConfig {
        root_path: path,
        min_size,
        max_size: if max_size == 0 { None } else { Some(max_size) },
        save_state,
        exclude_patterns,
        batch_size: 1000,
    };

    let mut app = app::App::new(config);
    tui::run(&mut app).await?;

    Ok(())
}

async fn resume_scan(state_file: PathBuf) -> Result<()> {
    let mut app = app::App::from_state_file(state_file)?;
    tui::run(&mut app).await?;

    Ok(())
}

fn list_saved_states() -> Result<()> {
    let states = state::list_state_files()?;

    if states.is_empty() {
        println!("No saved scan states found");
        return Ok(());
    }

    println!("Saved scan states:");
    for (i, state_info) in states.iter().enumerate() {
        println!(
            "  {}. {} - {} files scanned ({})",
            i + 1,
            state_info.timestamp,
            state_info.files_scanned,
            state_info.path.display()
        );
    }

    Ok(())
}

async fn yolo_scan(path: PathBuf, min_size: u64, max_size: u64, exclude_patterns: Vec<String>, use_backup: bool) -> Result<()> {
    use backup::BackupManager;
    use colored::Colorize;
    use humansize::{format_size, BINARY};
    use scanner::{Scanner, ScanConfig};
    use std::time::Instant;
    use suggestions::SuggestionEngine;

    println!("{}", "🚀 YOLO MODE ACTIVATED".bold().bright_yellow());
    println!("   {}", "Scanning and auto-deleting duplicates in real-time...".dimmed());
    println!("   {}: {}", "Path".bold(), path.display());
    println!("   {}: {}", "Min size".bold(), format_size(min_size, BINARY));
    if max_size > 0 {
        println!("   {}: {}", "Max size".bold(), format_size(max_size, BINARY));
    }
    if !exclude_patterns.is_empty() {
        let default_count = scanner::get_default_exclusions().len();
        let user_count = exclude_patterns.len().saturating_sub(default_count);
        if user_count > 0 {
            println!("   {}: {} default + {} custom patterns",
                "Exclusions".bold().yellow(),
                default_count,
                user_count);
        } else {
            println!("   {}: {} default patterns",
                "Exclusions".bold().yellow(),
                default_count);
        }
    }
    println!("   {}: {}",
        "Deletion method".bold(),
        if use_backup {
            "Backup (copy to ~/.local/share/dupscanner/backups/)".cyan()
        } else {
            "Trash (move to system recycle bin)".green()
        }
    );
    println!();

    let start_time = Instant::now();

    // Open database
    let db_path = get_default_db_path()?;
    let db = database::ScanDatabase::open(&db_path)?;
    let scan_id = db.start_scan(&path)?;

    let config = ScanConfig {
        root_path: path,
        min_size,
        max_size: if max_size == 0 { None } else { Some(max_size) },
        save_state: false,
        exclude_patterns,
        batch_size: 1000,
    };

    let scanner = Scanner::new(config);
    let mut backup_manager = BackupManager::new()?;
    backup_manager.load_records()?;
    let mut finder = duplicates::DuplicateFinder::new();

    let mut total_scanned = 0;
    let mut total_deleted = 0;
    let mut total_space_freed: u64 = 0;
    let mut processed_groups = std::collections::HashSet::new();

    println!("{}", "📊 Scanning files and removing duplicates...".bold().cyan());
    println!();

    let scan_start = Instant::now();

    // Start streaming scan
    let (file_rx, progress_rx) = scanner.scan_streaming();

    // Process files as they arrive
    loop {
        // Check for progress updates
        while let Ok(progress) = progress_rx.try_recv() {
            total_scanned = progress.scanned_count;
            if total_scanned % 100 == 0 {
                let elapsed = scan_start.elapsed().as_secs_f64();
                let rate = if elapsed > 0.0 { total_scanned as f64 / elapsed } else { 0.0 };
                print!("\r   {} {} files ({:.1} files/sec, {} groups)",
                    "Scanned:".bold(),
                    total_scanned.to_string().bright_green(),
                    rate,
                    finder.groups().len().to_string().yellow());
                use std::io::Write;
                std::io::stdout().flush().unwrap();
            }
        }

        // Process incoming batches
        match file_rx.try_recv() {
            Ok(batch) => {
                // Process all files in this batch
                for file in batch {
                    let _ = finder.process_file(file);
                }

                // Periodically sort groups (every 100 files)
                if total_scanned % 100 == 0 {
                    finder.ensure_sorted();
                }

                // Check for new duplicate groups and delete immediately
                for group in finder.groups() {
                    if processed_groups.contains(&group.hash) {
                        continue; // Already processed
                    }

                    processed_groups.insert(group.hash.clone());

                    // Save to database
                    let _ = db.save_duplicate_group(scan_id, group);

                    // Determine which file to keep
                    if let Some(keeper_index) = SuggestionEngine::get_best_keeper(&group.files) {
                        let keeper_path = group.files[keeper_index].path.display().to_string();
                        let file_size = group.files[0].size;

                        println!("\n   {} {} duplicates ({}, hash: {}...)",
                            "Found".bold(),
                            group.files.len().to_string().yellow(),
                            format_size(file_size, BINARY).dimmed(),
                            &group.hash[..8].dimmed());
                        println!("   {} {}", "✓ Keeping:".green().bold(), keeper_path);

                        // Delete all files except the keeper
                        for (i, file) in group.files.iter().enumerate() {
                            if i == keeper_index {
                                continue;
                            }

                            let result = if use_backup {
                                backup_manager.delete_with_backup(&file.path).map(|_| ())
                            } else {
                                trash::delete(&file.path).map_err(|e| anyhow::anyhow!("Failed to move to trash: {}", e))
                            };

                            match result {
                                Ok(_) => {
                                    let method = if use_backup { "Deleted (backed up):" } else { "Moved to trash:" };
                                    println!("   {} {}", method.red().bold(), file.path.display().to_string().dimmed());
                                    total_deleted += 1;
                                    total_space_freed += file.size;
                                }
                                Err(e) => {
                                    eprintln!("   {} {}: {}",
                                        "⚠ Failed to delete".yellow().bold(),
                                        file.path.display(),
                                        e.to_string().dimmed());
                                }
                            }
                        }
                    }
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                break; // Scan complete
            }
        }
    }

    let scan_elapsed = scan_start.elapsed();
    println!("\r   {} {} files in {:.2}s",
        "✓ Scanned:".bold().green(),
        total_scanned.to_string().bright_green(),
        scan_elapsed.as_secs_f64());
    println!();

    // Complete scan in database
    let _ = db.complete_scan(scan_id, total_scanned, finder.groups().len());

    // Print summary
    let total_elapsed = start_time.elapsed();
    println!("{}", "✅ YOLO mode complete!".bold().bright_green());
    println!();
    println!("{}", "📊 Summary:".bold());
    println!("   {}: {}",
        "Files scanned".bold(),
        total_scanned.to_string().bright_cyan());
    println!("   {}: {}",
        "Duplicate groups found".bold(),
        finder.groups().len().to_string().bright_yellow());
    println!("   {}: {}",
        "Duplicates deleted".bold(),
        total_deleted.to_string().bright_red());
    println!("   {}: {}",
        "Space freed".bold(),
        format_size(total_space_freed, BINARY).bright_green());
    println!("   {}: {:.2}s",
        "Total time".bold(),
        total_elapsed.as_secs_f64());

    if use_backup {
        println!("   {}: {}",
            "Backups location".bold(),
            "~/.local/share/dupscanner/backups/".dimmed());
        println!();
        println!("{} {}", "💡".bright_yellow(), "Tip: All deleted files were backed up and can be restored if needed.".dimmed());
    } else {
        println!("   {}: {}",
            "Files location".bold(),
            "System trash/recycle bin".dimmed());
        println!();
        println!("{} {}", "💡".bright_yellow(), "Tip: All deleted files are in your system trash and can be restored from there.".dimmed());
    }

    Ok(())
}

fn get_default_db_path() -> Result<PathBuf> {
    let data_dir = dirs::data_local_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine local data directory"))?;
    let app_dir = data_dir.join("dupscanner");
    std::fs::create_dir_all(&app_dir)?;
    Ok(app_dir.join("scans.db"))
}

async fn show_scan_history(count: usize, db_path: Option<PathBuf>) -> Result<()> {
    use colored::Colorize;
    use chrono::{DateTime, Utc};

    let db_path = db_path.unwrap_or_else(|| get_default_db_path().unwrap());
    let db = database::ScanDatabase::open(&db_path)?;
    let scans = db.list_scans()?;

    if scans.is_empty() {
        println!("{}", "No scans found in database.".yellow());
        println!("\n{} Run a scan with database support:", "💡".bright_yellow());
        println!("   dupscanner scan /path/to/scan");
        return Ok(());
    }

    println!("{}", "📊 Scan History".bold().cyan());
    println!();

    for (i, scan) in scans.iter().take(count).enumerate() {
        let start_time = DateTime::from_timestamp(scan.start_time, 0)
            .unwrap_or_else(|| Utc::now());

        let status = if scan.end_time.is_some() {
            "✓ Complete".green()
        } else {
            "⏳ In Progress".yellow()
        };

        println!("{:3}. {} {}",
            (i + 1).to_string().bright_cyan(),
            format!("ID {}", scan.id).bold(),
            status
        );
        println!("     {}: {}", "Path".bold(), scan.root_path.display());
        println!("     {}: {}", "Started".bold(), start_time.format("%Y-%m-%d %H:%M:%S"));

        if scan.end_time.is_some() {
            println!("     {}: {} files, {} duplicate groups",
                "Results".bold(),
                scan.files_scanned.to_string().bright_green(),
                scan.groups_found.to_string().yellow()
            );
        }
        println!();
    }

    println!("{} Use 'dupscanner view <id>' to review a scan", "💡".bright_yellow());

    Ok(())
}

async fn view_scan(scan_id: i64, db_path: Option<PathBuf>) -> Result<()> {
    use colored::Colorize;
    use humansize::{format_size, BINARY};

    let db_path = db_path.unwrap_or_else(|| get_default_db_path().unwrap());
    let db = database::ScanDatabase::open(&db_path)?;

    // Get scan info
    let scan = db.get_scan_info(scan_id)
        .ok_or_else(|| anyhow::anyhow!("Scan ID {} not found", scan_id))?;

    // Get duplicate groups
    let groups = db.load_duplicate_groups(scan_id)?;

    println!("{}", format!("📊 Scan #{}", scan_id).bold().cyan());
    println!();
    println!("{}: {}", "Path".bold(), scan.root_path.display());
    println!("{}: {}", "Files scanned".bold(), scan.files_scanned.to_string().bright_green());
    println!("{}: {}", "Duplicate groups".bold(), scan.groups_found.to_string().yellow());
    println!();

    if groups.is_empty() {
        println!("{}", "No duplicates found!".green());
        return Ok(());
    }

    println!("{}", "🔍 Duplicate Groups:".bold().cyan());
    println!();

    for (i, group) in groups.iter().enumerate() {
        let file_size = group.files.first().map(|f| f.size).unwrap_or(0);
        let wasted_space = file_size * (group.files.len() as u64 - 1);

        println!("{}. {} duplicates ({} wasted, hash: {}...)",
            (i + 1).to_string().bright_cyan(),
            group.files.len().to_string().yellow(),
            format_size(wasted_space, BINARY).red(),
            &group.hash[..8].dimmed()
        );

        for (j, file) in group.files.iter().enumerate() {
            let marker = if j == 0 { "→" } else { " " };
            println!("   {} {}", marker, file.path.display().to_string().dimmed());
        }
        println!();
    }

    let total_wasted: u64 = groups.iter()
        .map(|g| {
            let size = g.files.first().map(|f| f.size).unwrap_or(0);
            size * (g.files.len() as u64 - 1)
        })
        .sum();

    println!("{}: {}",
        "Total wasted space".bold(),
        format_size(total_wasted, BINARY).bright_red()
    );

    Ok(())
}
