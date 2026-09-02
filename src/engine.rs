//! Shared scan driver used by every front end (TUI, web UI, yolo, JSON).
//!
//! A `ScanSession` owns a walker thread (from `Scanner`) and a hashing
//! thread (which owns the `DuplicateFinder` while the scan runs). Front ends
//! receive `EngineEvent`s over a channel: progress updates, snapshots of the
//! current duplicate groups, and finally the finished `DuplicateFinder`.

use crate::duplicates::{DuplicateFinder, DuplicateGroup};
use crate::scanner::{ScanConfig, ScanProgress, Scanner};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub enum EngineEvent {
    /// Walker progress. Sent whenever the walker reports.
    Progress(ScanProgress),
    /// Current duplicate groups, sorted by wasted space. Sent after each
    /// processed batch, throttled to a few times per second.
    Groups(Vec<DuplicateGroup>),
    /// Scan finished. Carries the finder with all state.
    Complete {
        finder: DuplicateFinder,
        progress: ScanProgress,
        elapsed: Duration,
    },
}

/// Paths removed by a front end while the scan is still running. The
/// engine thread drops them from its state so the final result is accurate.
#[derive(Clone, Default)]
pub struct RemovedPaths(Arc<Mutex<HashSet<PathBuf>>>);

impl RemovedPaths {
    pub fn add(&self, path: PathBuf) {
        if let Ok(mut set) = self.0.lock() {
            set.insert(path);
        }
    }

    pub fn add_all<I: IntoIterator<Item = PathBuf>>(&self, paths: I) {
        if let Ok(mut set) = self.0.lock() {
            set.extend(paths);
        }
    }

    fn take(&self) -> HashSet<PathBuf> {
        self.0
            .lock()
            .map(|mut set| std::mem::take(&mut *set))
            .unwrap_or_default()
    }
}

pub struct ScanSession {
    events: Receiver<EngineEvent>,
    removed: RemovedPaths,
}

impl ScanSession {
    /// Start scanning immediately.
    pub fn start(config: ScanConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        let removed = RemovedPaths::default();
        let removed_for_thread = removed.clone();
        thread::spawn(move || run_engine(config, tx, removed_for_thread));
        ScanSession { events: rx, removed }
    }

    /// Handle used to tell the engine about files deleted mid-scan.
    pub fn removed_paths(&self) -> RemovedPaths {
        self.removed.clone()
    }

    /// Non-blocking poll for the next event.
    pub fn try_next(&self) -> Option<EngineEvent> {
        match self.events.try_recv() {
            Ok(ev) => Some(ev),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }

    /// Blocking wait for the next event; `None` once the engine is gone.
    pub fn next(&self) -> Option<EngineEvent> {
        self.events.recv().ok()
    }

    pub fn next_timeout(&self, timeout: Duration) -> Option<EngineEvent> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Run to completion, ignoring intermediate snapshots. Useful for
    /// non-interactive modes. Calls `on_progress` for each progress event.
    pub fn run_to_completion<F: FnMut(&ScanProgress, usize)>(
        self,
        mut on_progress: F,
    ) -> Option<(DuplicateFinder, ScanProgress, Duration)> {
        let mut group_count = 0usize;
        while let Some(ev) = self.next() {
            match ev {
                EngineEvent::Progress(p) => on_progress(&p, group_count),
                EngineEvent::Groups(g) => group_count = g.len(),
                EngineEvent::Complete { finder, progress, elapsed } => {
                    return Some((finder, progress, elapsed));
                }
            }
        }
        None
    }
}

fn run_engine(config: ScanConfig, tx: Sender<EngineEvent>, removed: RemovedPaths) {
    let started = Instant::now();
    let scanner = Scanner::new(config);
    let (batches, progress_rx) = scanner.scan_streaming();
    let mut finder = DuplicateFinder::new();
    let mut last_progress = ScanProgress::default();
    let mut last_snapshot = Instant::now() - Duration::from_secs(1);
    let mut dirty = false;

    loop {
        // Drain progress without blocking.
        while let Ok(p) = progress_rx.try_recv() {
            last_progress = p;
            if tx.send(EngineEvent::Progress(p)).is_err() {
                return;
            }
        }

        match batches.recv_timeout(Duration::from_millis(100)) {
            Ok(batch) => {
                finder.process_batch(batch);
                let gone = removed.take();
                if !gone.is_empty() {
                    finder.remove_paths(&gone);
                }
                dirty = true;
                if last_snapshot.elapsed() >= Duration::from_millis(250) {
                    finder.ensure_sorted();
                    if tx.send(EngineEvent::Groups(finder.groups().to_vec())).is_err() {
                        return;
                    }
                    last_snapshot = Instant::now();
                    dirty = false;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if dirty && last_snapshot.elapsed() >= Duration::from_millis(250) {
                    finder.ensure_sorted();
                    if tx.send(EngineEvent::Groups(finder.groups().to_vec())).is_err() {
                        return;
                    }
                    last_snapshot = Instant::now();
                    dirty = false;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    while let Ok(p) = progress_rx.try_recv() {
        last_progress = p;
    }
    let gone = removed.take();
    if !gone.is_empty() {
        finder.remove_paths(&gone);
    }
    finder.ensure_sorted();
    let _ = tx.send(EngineEvent::Groups(finder.groups().to_vec()));
    let _ = tx.send(EngineEvent::Complete {
        finder,
        progress: last_progress,
        elapsed: started.elapsed(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn session_reports_pairs_and_completes() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), b"pair").unwrap();
        fs::write(dir.path().join("b.txt"), b"pair").unwrap();
        fs::write(dir.path().join("c.txt"), b"solo").unwrap();

        let session = ScanSession::start(ScanConfig {
            root_path: dir.path().to_path_buf(),
            min_size: 1,
            max_size: None,
            exclude_patterns: vec![],
            batch_size: 1000,
        });
        let (finder, progress, _) = session.run_to_completion(|_, _| {}).unwrap();
        assert_eq!(progress.scanned_count, 3);
        assert_eq!(finder.groups().len(), 1);
        assert_eq!(finder.groups()[0].file_count(), 2);
    }

    #[test]
    fn removed_paths_are_dropped_from_final_result() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("a.txt"), b"pair").unwrap();
        fs::write(dir.path().join("b.txt"), b"pair").unwrap();

        let session = ScanSession::start(ScanConfig {
            root_path: dir.path().to_path_buf(),
            min_size: 1,
            max_size: None,
            exclude_patterns: vec![],
            batch_size: 1000,
        });
        session.removed_paths().add(dir.path().join("b.txt"));
        let (finder, _, _) = session.run_to_completion(|_, _| {}).unwrap();
        assert!(finder.groups().is_empty());
    }
}
