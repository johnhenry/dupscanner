//! Where dupscanner keeps its own data.

use anyhow::{Context, Result};
use std::path::PathBuf;

/// `<local data dir>/dupscanner`, created on first use.
pub fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .context("Could not determine the local data directory")?
        .join("dupscanner");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create {}", dir.display()))?;
    Ok(dir)
}

/// The single scan database shared by every mode.
pub fn default_db_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("scans.db"))
}
