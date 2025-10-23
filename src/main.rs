mod app;
mod backup;
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
        } => {
            if yolo {
                // YOLO mode: non-interactive real-time duplicate removal
                yolo_scan(path, min_size, max_size).await?;
            } else if resume {
                // Try to resume from default state file
                let state_file = state::get_default_state_file(&path)?;
                if state_file.exists() {
                    resume_scan(state_file).await?;
                } else {
                    eprintln!("No previous scan state found for this directory");
                    start_new_scan(path, min_size, max_size, save_state).await?;
                }
            } else {
                start_new_scan(path, min_size, max_size, save_state).await?;
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
) -> Result<()> {
    let config = scanner::ScanConfig {
        root_path: path,
        min_size,
        max_size: if max_size == 0 { None } else { Some(max_size) },
        save_state,
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

async fn yolo_scan(path: PathBuf, min_size: u64, max_size: u64) -> Result<()> {
    use backup::BackupManager;
    use scanner::{Scanner, ScanConfig};
    use std::collections::HashMap;
    use suggestions::SuggestionEngine;

    println!("🚀 YOLO MODE ACTIVATED");
    println!("   Scanning and auto-deleting duplicates in real-time...");
    println!("   Path: {}", path.display());
    println!("   Min size: {} bytes", min_size);
    if max_size > 0 {
        println!("   Max size: {} bytes", max_size);
    }
    println!();

    let config = ScanConfig {
        root_path: path,
        min_size,
        max_size: if max_size == 0 { None } else { Some(max_size) },
        save_state: false,
    };

    let mut scanner = Scanner::new(config);
    let mut backup_manager = BackupManager::new()?;
    backup_manager.load_records()?;

    let mut total_scanned = 0;
    let mut total_deleted = 0;
    let mut total_space_freed: u64 = 0;

    println!("📊 Scanning files...\n");

    // Scan and process duplicates in real-time
    let size_groups_result = scanner.scan(|count, _path| {
        total_scanned = count;
        if count % 100 == 0 {
            print!("\r   Scanned: {} files, Deleted: {} duplicates", count, total_deleted);
            use std::io::Write;
            std::io::stdout().flush().unwrap();
        }
    })?;

    println!("\r   Scanned: {} files, Deleted: {} duplicates", total_scanned, total_deleted);
    println!();

    // Process size groups to find duplicates
    println!("🔍 Finding and removing duplicates...\n");

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

            println!("   Found {} duplicates (hash: {}...)", group_files.len(), &hash[..8]);
            println!("   ✓ Keeping: {}", keeper_path);

            // Delete all files except the keeper
            for (i, file) in group_files.iter().enumerate() {
                if i == keeper_index {
                    continue;
                }

                match backup_manager.delete_with_backup(&file.path) {
                    Ok(_) => {
                        println!("   ✗ Deleted: {}", file.path.display());
                        total_deleted += 1;
                        total_space_freed += file.size;
                    }
                    Err(e) => {
                        eprintln!("   ⚠ Failed to delete {}: {}", file.path.display(), e);
                    }
                }
            }

            println!();
        }
    }

    // Print summary
    println!("✅ YOLO mode complete!\n");
    println!("📊 Summary:");
    println!("   Files scanned: {}", total_scanned);
    println!("   Duplicates deleted: {}", total_deleted);
    println!("   Space freed: {}", format_size(total_space_freed));
    println!("   Backups location: ~/.local/share/dupscanner/backups/");
    println!();
    println!("💡 Tip: All deleted files were backed up and can be restored if needed.");

    Ok(())
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
