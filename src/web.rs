//! Local web UI (`dupscanner serve`).
//!
//! Contract for the implementation:
//! * bind to 127.0.0.1 only, never 0.0.0.0;
//! * drive a scan with `engine::ScanSession` (or load a recorded scan when
//!   `scan_id` is set) and stream progress/groups to the browser over SSE;
//! * every mutating or file-reading endpoint must resolve the requested path,
//!   check it is inside `config.root_path`, and check it belongs to a current
//!   duplicate group; deletions go through `deletion::plan_deletions` and a
//!   `deletion::Deleter` so no group ever loses its last copy;
//! * static assets are embedded with `include_str!` from `assets/`.

use crate::deletion::DeleteMethod;
use crate::scanner::ScanConfig;
use anyhow::{bail, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub config: ScanConfig,
    /// Port to listen on; 0 picks a free port.
    pub port: u16,
    pub open_browser: bool,
    pub delete_method: DeleteMethod,
    /// Where to record the finished scan. `None` disables persistence.
    pub db_path: Option<PathBuf>,
    /// Serve a previously recorded scan instead of scanning.
    pub scan_id: Option<i64>,
}

pub fn serve(_opts: ServeOptions) -> Result<()> {
    bail!("the web UI is not available in this build yet")
}
