//! Background image decoding for the terminal preview pane.
//!
//! Decoding happens on a worker thread so a large photo never stalls the
//! frame loop. Results are downscaled once and cached by path. Only the
//! most recent request is decoded; older requests still queued are dropped,
//! because the user has already moved on.

use crate::filters::FileKind;
use image::DynamicImage;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;

/// Longest side of a cached preview, in pixels. Terminal cells are far
/// coarser than this even with the pixel protocols.
const MAX_SIDE: u32 = 1200;
const CACHE_ENTRIES: usize = 24;

#[derive(Debug, Clone)]
pub enum Preview {
    /// Not an image format we can decode.
    Unsupported,
    Loading,
    Ready(Arc<DynamicImage>),
    Failed(String),
}

pub struct Previewer {
    requests: Sender<PathBuf>,
    results: Receiver<(PathBuf, Result<DynamicImage, String>)>,
    cache: HashMap<PathBuf, Preview>,
    order: VecDeque<PathBuf>,
    pending: HashSet<PathBuf>,
}

impl Previewer {
    pub fn new() -> Self {
        let (req_tx, req_rx) = mpsc::channel::<PathBuf>();
        let (res_tx, res_rx) = mpsc::channel();
        thread::spawn(move || worker(req_rx, res_tx));
        Previewer {
            requests: req_tx,
            results: res_rx,
            cache: HashMap::new(),
            order: VecDeque::new(),
            pending: HashSet::new(),
        }
    }

    /// Can we decode this file? Decided by extension, matching the
    /// decoders compiled into the `image` crate.
    pub fn supports(path: &Path) -> bool {
        if FileKind::of(path) != FileKind::Image {
            return false;
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        matches!(
            ext.as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "bmp" | "tif" | "tiff" | "ico" | "qoi"
        )
    }

    /// Current state for `path`, requesting a decode if needed.
    pub fn get(&mut self, path: &Path) -> Preview {
        self.poll();
        if !Self::supports(path) {
            return Preview::Unsupported;
        }
        if let Some(p) = self.cache.get(path) {
            return p.clone();
        }
        if self.pending.insert(path.to_path_buf()) {
            let _ = self.requests.send(path.to_path_buf());
        }
        Preview::Loading
    }

    /// Drain finished decodes into the cache. Returns true if anything new
    /// arrived, so the caller can redraw.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.results.try_recv() {
                Ok((path, result)) => {
                    self.pending.remove(&path);
                    let entry = match result {
                        Ok(img) => Preview::Ready(Arc::new(img)),
                        Err(e) => Preview::Failed(e),
                    };
                    self.insert(path, entry);
                    changed = true;
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        changed
    }

    /// Forget a path (after a rename or deletion).
    pub fn forget(&mut self, path: &Path) {
        self.cache.remove(path);
        self.order.retain(|p| p != path);
    }

    fn insert(&mut self, path: PathBuf, preview: Preview) {
        if !self.cache.contains_key(&path) {
            self.order.push_back(path.clone());
        }
        self.cache.insert(path, preview);
        while self.order.len() > CACHE_ENTRIES {
            if let Some(old) = self.order.pop_front() {
                self.cache.remove(&old);
            }
        }
    }
}

impl Default for Previewer {
    fn default() -> Self {
        Self::new()
    }
}

fn worker(requests: Receiver<PathBuf>, results: Sender<(PathBuf, Result<DynamicImage, String>)>) {
    while let Ok(first) = requests.recv() {
        // Collapse a burst of requests to the newest one; the others were
        // files the user scrolled past.
        let mut latest = first;
        let mut skipped = Vec::new();
        while let Ok(next) = requests.try_recv() {
            skipped.push(std::mem::replace(&mut latest, next));
        }
        for s in skipped {
            let _ = results.send((s, Err("skipped".into())));
        }
        let result = decode(&latest);
        if results.send((latest, result)).is_err() {
            return;
        }
    }
}

fn decode(path: &Path) -> Result<DynamicImage, String> {
    let mut reader = image::ImageReader::open(path).map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(768 * 1024 * 1024);
    reader.limits(limits);
    let reader = reader.with_guessed_format().map_err(|e| e.to_string())?;
    let img = reader.decode().map_err(|e| e.to_string())?;
    if img.width() > MAX_SIDE || img.height() > MAX_SIDE {
        Ok(img.thumbnail(MAX_SIDE, MAX_SIDE))
    } else {
        Ok(img)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn decodes_png_in_background_and_caches() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("dot.png");
        let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 20, 30, 255]));
        img.save(&p).unwrap();

        let mut pv = Previewer::new();
        assert!(matches!(pv.get(&p), Preview::Loading));
        let start = Instant::now();
        loop {
            if let Preview::Ready(im) = pv.get(&p) {
                assert_eq!((im.width(), im.height()), (4, 3));
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "decode timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(pv.get(&p), Preview::Ready(_)));
    }

    #[test]
    fn unsupported_and_broken_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let txt = dir.path().join("notes.txt");
        std::fs::write(&txt, b"hi").unwrap();
        let bad = dir.path().join("broken.jpg");
        std::fs::write(&bad, b"not a jpeg").unwrap();

        let mut pv = Previewer::new();
        assert!(matches!(pv.get(&txt), Preview::Unsupported));
        assert!(matches!(pv.get(&bad), Preview::Loading));
        let start = Instant::now();
        loop {
            if let Preview::Failed(_) = pv.get(&bad) {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
