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
        #[arg(value_name = "PATH")]
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
            use_backup,
        } => {
            if yolo {
                // YOLO mode: non-interactive real-time duplicate removal
                yolo_scan(path, min_size, max_size, exclude, use_backup).await?;
            } else if resume {
                // Try to resume from default state file
                let state_file = state::get_default_state_file(&path)?;
                if state_file.exists() {
                    resume_scan(state_file).await?;
                } else {
                    eprintln!("No previous scan state found for this directory");
                    start_new_scan(path, min_size, max_size, save_state, exclude).await?;
                }
            } else {
                start_new_scan(path, min_size, max_size, save_state, exclude).await?;
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
    use std::collections::HashMap;
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
        println!("   {}: {}", "Excluding".bold().yellow(), exclude_patterns.join(", ").dimmed());
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

    let config = ScanConfig {
        root_path: path,
        min_size,
        max_size: if max_size == 0 { None } else { Some(max_size) },
        save_state: false,
        exclude_patterns,
        batch_size: 1000,
    };

    let mut scanner = Scanner::new(config);
    let mut backup_manager = BackupManager::new()?;
    backup_manager.load_records()?;

    let mut total_scanned = 0;
    let mut total_deleted = 0;
    let mut total_space_freed: u64 = 0;

    println!("{}", "📊 Scanning files...".bold().cyan());
    println!();

    let scan_start = Instant::now();

    // Scan and process duplicates in real-time
    let size_groups_result = scanner.scan(|count, _path| {
        total_scanned = count;
        if count % 100 == 0 {
            let elapsed = scan_start.elapsed().as_secs_f64();
            let rate = if elapsed > 0.0 { count as f64 / elapsed } else { 0.0 };
            print!("\r   {} {} files ({:.1} files/sec)",
                "Scanned:".bold(),
                count.to_string().bright_green(),
                rate);
            use std::io::Write;
            std::io::stdout().flush().unwrap();
        }
    })?;

    let scan_elapsed = scan_start.elapsed();
    println!("\r   {} {} files in {:.2}s",
        "✓ Scanned:".bold().green(),
        total_scanned.to_string().bright_green(),
        scan_elapsed.as_secs_f64());
    println!();

    // Process size groups to find duplicates
    println!("{}", "🔍 Finding and removing duplicates...".bold().cyan());
    println!();

    for (_size, mut files) in size_groups_result {
        if files.len() < 2 {
            continue;
        }

        // Compute hashes for this size group
        for file in &mut files {
            let _ = file.get_or_compute_hash();
        }

        // Group by hash
        let mut hash_groups: HashMap<String, Vec<scanner::FileInfo>> = HashMap::new();
        for file in files {
            if let Some(ref hash) = file.hash {
                hash_groups
                    .entry(hash.clone())
                    .or_insert_with(Vec::new)
                    .push(file);
            }
        }

        // Process each hash group
        for (hash, group_files) in hash_groups {
            if group_files.len() < 2 {
                continue;
            }

            // Determine which file to keep
            let keeper_index = SuggestionEngine::get_best_keeper(&group_files);

            if keeper_index.is_none() {
                // Shouldn't happen, but be safe
                continue;
            }

            let keeper_index = keeper_index.unwrap();
            let keeper_path = group_files[keeper_index].path.display().to_string();
            let file_size = group_files[0].size;

            println!("   {} {} duplicates ({}, hash: {}...)",
                "Found".bold(),
                group_files.len().to_string().yellow(),
                format_size(file_size, BINARY).dimmed(),
                &hash[..8].dimmed());
            println!("   {} {}", "✓ Keeping:".green().bold(), keeper_path);

            // Delete all files except the keeper
            for (i, file) in group_files.iter().enumerate() {
                if i == keeper_index {
                    continue;
                }

                let result = if use_backup {
                    // Use backup method (copy then delete)
                    backup_manager.delete_with_backup(&file.path).map(|_| ())
                } else {
                    // Use trash (move to recycle bin)
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

            println!();
        }
    }

    // Print summary
    let total_elapsed = start_time.elapsed();
    println!("{}", "✅ YOLO mode complete!".bold().bright_green());
    println!();
    println!("{}", "📊 Summary:".bold());
    println!("   {}: {}",
        "Files scanned".bold(),
        total_scanned.to_string().bright_cyan());
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
