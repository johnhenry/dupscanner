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
        } => {
            if resume {
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
