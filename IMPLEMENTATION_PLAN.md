# DupScanner Implementation Plan

This document outlines detailed implementation plans for all new features based on learnings from the Super Dee Duper project.

## Feature 1: Quick Hash Strategy ⚡

### Priority: HIGH (Easy Win)
### Estimated Time: 2-3 hours
### Complexity: Medium

### Overview
Implement a two-phase hashing strategy to dramatically speed up large file scans by avoiding unnecessary full file hashing.

### Current Behavior
- Scans all files by size
- Computes full SHA-256 hash for ALL files with matching sizes
- Slow for large files and large directories

### Proposed Behavior
1. Scan all files by size (unchanged)
2. For size-matched groups, compute quick hash (first 64KB only)
3. Only compute full hash for files with matching quick hashes
4. Drastically reduces hashing operations

### Implementation Steps

#### 1. Update FileInfo Structure (`scanner.rs`)
```rust
pub struct FileInfo {
    pub path: PathBuf,
    pub size: u64,
    pub quick_hash: Option<String>,  // NEW: First 64KB hash
    pub hash: Option<String>,         // Full file hash (lazy)
    pub modified: SystemTime,
    pub depth: usize,
}
```

#### 2. Add Quick Hash Function (`scanner.rs`)
```rust
pub fn compute_quick_hash(path: &Path) -> Result<String> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 65536]; // 64KB buffer

    // Read only first 64KB
    let bytes_read = reader.read(&mut buffer)?;
    hasher.update(&buffer[..bytes_read]);

    Ok(format!("{:x}", hasher.finalize()))
}
```

#### 3. Update Scanner Logic
- Compute quick_hash during initial scan
- Group by size, then by quick_hash
- Only compute full hash for quick_hash matches

#### 4. Update DuplicateFinder Logic (`duplicates.rs`)
```rust
// Pseudocode:
for (size, files) in size_groups {
    // Group by quick hash
    let quick_hash_groups = group_by_quick_hash(files);

    for (quick_hash, group) in quick_hash_groups {
        if group.len() > 1 {
            // Only now compute full hashes
            for file in group {
                file.hash = compute_file_hash(&file.path)?;
            }
            // Group by full hash
            group_by_full_hash_and_add_to_results(group);
        }
    }
}
```

### Testing Strategy
1. Unit tests for quick_hash function
2. Test with small files (<64KB) - quick hash should equal full hash
3. Test with large files - verify correct grouping
4. Performance benchmarks comparing old vs new
5. Test with demo data

### Success Metrics
- 50-70% faster scanning on large files
- No false positives (files incorrectly marked as duplicates)
- No false negatives (duplicates missed)

### Files to Modify
- `src/scanner.rs` - Add quick_hash, update FileInfo
- `src/duplicates.rs` - Update grouping logic
- `Cargo.toml` - No new dependencies needed

### Rollback Plan
Quick hash is additive - can be disabled by setting quick_hash = None

---

## Feature 2: Exclusion Patterns 🚫

### Priority: HIGH (Easy Win)
### Estimated Time: 1-2 hours
### Complexity: Low

### Overview
Add ability to exclude files/directories using glob patterns (e.g., `**/.git/**`, `**/node_modules/**`)

### Current Behavior
- Scans ALL files in directory
- No way to exclude specific patterns

### Proposed Behavior
- Accept `--exclude` flag with glob patterns
- Skip files/directories matching patterns during scan
- Support multiple patterns

### Implementation Steps

#### 1. Update CLI Arguments (`main.rs`)
```rust
Scan {
    // ... existing fields

    /// Glob patterns to exclude from scan
    #[arg(short = 'e', long = "exclude", value_name = "PATTERN")]
    exclude: Vec<String>,
}
```

#### 2. Update ScanConfig (`scanner.rs`)
```rust
pub struct ScanConfig {
    pub root_path: PathBuf,
    pub min_size: u64,
    pub max_size: Option<u64>,
    pub save_state: bool,
    pub exclude_patterns: Vec<String>,  // NEW
}
```

#### 3. Add Glob Matching to Scanner
```rust
use glob::Pattern;

impl Scanner {
    fn should_exclude(&self, path: &Path) -> bool {
        let path_str = path.to_string_lossy();
        self.config.exclude_patterns.iter().any(|pattern| {
            Pattern::new(pattern)
                .map(|p| p.matches(&path_str))
                .unwrap_or(false)
        })
    }

    pub fn scan<F>(&mut self, mut progress_callback: F) -> Result<...> {
        for entry in walker {
            if self.should_exclude(entry.path()) {
                continue; // Skip this file/directory
            }
            // ... rest of logic
        }
    }
}
```

#### 4. Update walkdir to Skip Directories
```rust
let walker = WalkDir::new(&self.config.root_path)
    .follow_links(false)
    .into_iter()
    .filter_entry(|e| {
        !is_hidden(e) && !self.should_exclude(e.path())
    });
```

### Testing Strategy
1. Test excluding single file
2. Test excluding directory
3. Test multiple patterns
4. Test glob wildcards: `*.log`, `**/.git/**`
5. Test with demo data, exclude backup/

### Success Metrics
- Excluded files/directories are not scanned
- Performance improvement on large repos with node_modules, .git, etc.

### Files to Modify
- `src/main.rs` - Add CLI flag
- `src/scanner.rs` - Add exclusion logic
- `Cargo.toml` - glob crate already present

---

## Feature 3: Improve Console Output 📊

### Priority: HIGH (Easy Win)
### Estimated Time: 1-2 hours
### Complexity: Low

### Overview
Improve YOLO mode and scan output with better formatting, emoji indicators, and time tracking.

### Current Behavior
- Basic text output
- No time tracking
- No phase indicators

### Proposed Behavior
- Emoji indicators for different phases
- Human-readable time elapsed
- Better progress formatting
- Clear phase transitions

### Implementation Steps

#### 1. Add Dependencies (`Cargo.toml`)
```toml
[dependencies]
# ... existing
indicatif = "0.17"  # Already present
humantime = "2.1"   # For duration formatting
```

#### 2. Create Output Formatting Module (`src/output.rs`)
```rust
pub fn format_duration(duration: Duration) -> String {
    // e.g., "2m 34s" or "1h 5m 12s"
}

pub fn format_phase(phase: ScanPhase) -> &'static str {
    match phase {
        ScanPhase::Scanning => "📊 Scanning files",
        ScanPhase::QuickHash => "⚡ Quick hashing",
        ScanPhase::FullHash => "🔍 Full hashing",
        ScanPhase::Deleting => "✗ Deleting duplicates",
        ScanPhase::Complete => "✅ Complete",
    }
}

pub enum ScanPhase {
    Scanning,
    QuickHash,
    FullHash,
    Deleting,
    Complete,
}
```

#### 3. Update YOLO Mode Output (`main.rs`)
```rust
async fn yolo_scan(...) -> Result<()> {
    let start_time = Instant::now();

    println!("🚀 YOLO MODE ACTIVATED");
    println!("   Path: {}", path.display());
    println!("   Started: {}\n", Local::now().format("%Y-%m-%d %H:%M:%S"));

    // During scan
    let size_groups = scanner.scan(|count, _| {
        if count % 100 == 0 {
            let elapsed = format_duration(start_time.elapsed());
            print!("\r📊 Scanning: {} files ({})  ", count, elapsed);
            stdout().flush().unwrap();
        }
    })?;

    // During hashing
    println!("\n⚡ Quick hashing potential duplicates...");

    // During full hashing
    println!("🔍 Full hashing confirmed duplicates...");

    // During deletion
    println!("✗ Deleting duplicates...\n");

    // Summary
    let total_time = format_duration(start_time.elapsed());
    println!("✅ Complete in {}\n", total_time);
}
```

#### 4. Add Progress Bar for TUI Mode
```rust
use indicatif::{ProgressBar, ProgressStyle};

let pb = ProgressBar::new(estimated_files);
pb.set_style(
    ProgressStyle::default_bar()
        .template("{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} {msg}")
        .progress_chars("#>-")
);

// Update progress
pb.set_message(format!("Phase: {}", phase));
pb.inc(1);
```

### Testing Strategy
1. Test YOLO mode output formatting
2. Test with small dataset (quick)
3. Test with large dataset (verify time tracking)
4. Verify emoji display in different terminals

### Success Metrics
- Clear, readable output
- Accurate time tracking
- Helpful phase indicators
- Professional appearance

### Files to Modify
- `src/main.rs` - Update YOLO mode output
- `src/output.rs` - NEW: Create formatting module
- `Cargo.toml` - Add humantime dependency

---

## Feature 4: Batch Processing 📦

### Priority: HIGH (Performance)
### Estimated Time: 2 hours
### Complexity: Medium

### Overview
Process files in batches to improve performance and reduce memory pressure.

### Current Behavior
- Scans all files into memory
- Processes all at once
- High memory usage for large scans

### Proposed Behavior
- Process files in batches (e.g., 1000 at a time)
- Flush results periodically
- Lower memory footprint
- Better progress tracking

### Implementation Steps

#### 1. Add Batch Processing to Scanner (`scanner.rs`)
```rust
const BATCH_SIZE: usize = 1000;

impl Scanner {
    pub fn scan_batched<F>(
        &mut self,
        mut batch_callback: F
    ) -> Result<HashMap<u64, Vec<FileInfo>>>
    where
        F: FnMut(&[FileInfo], usize), // (batch, total_count)
    {
        let mut batch = Vec::with_capacity(BATCH_SIZE);
        let mut total_count = 0;

        for entry in walker {
            // ... process entry into FileInfo
            batch.push(file_info);
            total_count += 1;

            if batch.len() >= BATCH_SIZE {
                batch_callback(&batch, total_count);
                // Process batch into size_groups
                for file in batch.drain(..) {
                    self.size_groups
                        .entry(file.size)
                        .or_insert_with(Vec::new)
                        .push(file);
                }
            }
        }

        // Process remaining batch
        if !batch.is_empty() {
            batch_callback(&batch, total_count);
            // Process final batch
        }

        Ok(self.size_groups.clone())
    }
}
```

#### 2. Update DuplicateFinder for Streaming (`duplicates.rs`)
```rust
impl DuplicateFinder {
    pub fn process_size_group_batch(&mut self, files: Vec<FileInfo>) -> Result<()> {
        // Process a batch of files
        // This can be called multiple times for streaming processing
    }
}
```

#### 3. Add Memory-Efficient YOLO Mode
```rust
// In yolo_scan:
scanner.scan_batched(|batch, total| {
    // Process each batch immediately
    // Don't wait for full scan to complete
})?;
```

### Testing Strategy
1. Test with small dataset (single batch)
2. Test with large dataset (multiple batches)
3. Verify memory usage reduction
4. Compare performance vs non-batched

### Success Metrics
- Lower peak memory usage
- Same or better performance
- No data loss between batches

### Files to Modify
- `src/scanner.rs` - Add batching logic
- `src/duplicates.rs` - Update for streaming
- `src/main.rs` - Use batched processing in YOLO mode

---

## Feature 5: SQLite Database Backend 💾

### Priority: MEDIUM (Major Feature)
### Estimated Time: 4-6 hours
### Complexity: High

### Overview
Replace JSON state files with SQLite database for better persistence, performance, and features.

### Current Behavior
- State saved to JSON files
- No scan history
- Can't resume interrupted scans reliably
- Limited metadata tracking

### Proposed Behavior
- All scans stored in SQLite database
- Track scan history and metadata
- Resume interrupted scans
- Query old scan results
- Better performance on large datasets

### Implementation Steps

#### 1. Add Dependencies (`Cargo.toml`)
```toml
[dependencies]
rusqlite = { version = "0.31", features = ["bundled"] }
```

#### 2. Create Database Module (`src/database.rs`)
```rust
use rusqlite::{Connection, Result as SqliteResult};

pub struct ScanDatabase {
    conn: Connection,
}

impl ScanDatabase {
    pub fn new(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for better concurrency
        conn.pragma_update(None, "journal_mode", "WAL")?;

        Ok(Self { conn })
    }

    pub fn initialize(&self) -> Result<()> {
        self.conn.execute_batch(r#"
            CREATE TABLE IF NOT EXISTS scans (
                id INTEGER PRIMARY KEY,
                root_path TEXT NOT NULL,
                start_time INTEGER NOT NULL,
                end_time INTEGER,
                files_scanned INTEGER DEFAULT 0,
                groups_found INTEGER DEFAULT 0,
                completed INTEGER DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL,
                path TEXT NOT NULL,
                size INTEGER NOT NULL,
                quick_hash TEXT,
                full_hash TEXT,
                modified INTEGER NOT NULL,
                depth INTEGER NOT NULL,
                FOREIGN KEY(scan_id) REFERENCES scans(id)
            );

            CREATE TABLE IF NOT EXISTS duplicate_groups (
                id INTEGER PRIMARY KEY,
                scan_id INTEGER NOT NULL,
                hash TEXT NOT NULL,
                file_count INTEGER NOT NULL,
                total_size INTEGER NOT NULL,
                wasted_space INTEGER NOT NULL,
                FOREIGN KEY(scan_id) REFERENCES scans(id)
            );

            CREATE INDEX IF NOT EXISTS idx_files_scan_id ON files(scan_id);
            CREATE INDEX IF NOT EXISTS idx_files_size ON files(size);
            CREATE INDEX IF NOT EXISTS idx_files_quick_hash ON files(quick_hash);
            CREATE INDEX IF NOT EXISTS idx_files_full_hash ON files(full_hash);
            CREATE INDEX IF NOT EXISTS idx_groups_scan_id ON duplicate_groups(scan_id);
        "#)?;

        Ok(())
    }

    pub fn start_scan(&self, root_path: &Path) -> Result<i64> {
        // Insert new scan record
    }

    pub fn add_file(&self, scan_id: i64, file_info: &FileInfo) -> Result<()> {
        // Insert file record
    }

    pub fn update_scan_progress(&self, scan_id: i64, files: usize, groups: usize) -> Result<()> {
        // Update scan progress
    }

    pub fn complete_scan(&self, scan_id: i64) -> Result<()> {
        // Mark scan as completed
    }

    pub fn get_incomplete_scans(&self) -> Result<Vec<ScanInfo>> {
        // Get scans that weren't completed
    }

    pub fn resume_scan(&self, scan_id: i64) -> Result<ScanState> {
        // Load scan state for resuming
    }
}
```

#### 3. Update State Module (`src/state.rs`)
```rust
// Keep for backwards compatibility, but primary storage is now SQLite
pub fn migrate_json_to_sqlite(json_path: &Path, db: &ScanDatabase) -> Result<()> {
    // Migrate old JSON state files to SQLite
}
```

#### 4. Update CLI Commands (`src/main.rs`)
```rust
Commands::Scan {
    // ... existing fields

    /// Resume from specific scan ID
    #[arg(long)]
    resume_scan: Option<i64>,

    /// Database path
    #[arg(long, default_value = "~/.local/share/dupscanner/scans.db")]
    db_path: PathBuf,
}

// Add new command
Commands::History {
    /// Database path
    #[arg(long)]
    db_path: Option<PathBuf>,
}
```

#### 5. Implement Batch Inserts for Performance
```rust
// Use transactions for batching
let tx = conn.transaction()?;
for file in batch {
    tx.execute("INSERT INTO files ...")?;
}
tx.commit()?;
```

### Testing Strategy
1. Test database creation and initialization
2. Test storing scan data
3. Test resuming interrupted scan
4. Test querying old scans
5. Test migration from JSON
6. Performance test with large datasets

### Success Metrics
- All scans persisted reliably
- Can resume any interrupted scan
- Can view scan history
- Better performance than JSON for large datasets

### Files to Modify
- `src/database.rs` - NEW: Database module
- `src/state.rs` - Update to use database
- `src/main.rs` - Add database flags
- `src/scanner.rs` - Integrate database calls
- `Cargo.toml` - Add rusqlite dependency

---

## Feature 6: Serve Old Scans 📂

### Priority: MEDIUM
### Estimated Time: 2-3 hours
### Complexity: Medium
### Depends On: Feature 5 (SQLite Database)

### Overview
Load and view results from previous scans without re-scanning.

### Current Behavior
- Must re-scan to view duplicates
- No way to access old scan results

### Proposed Behavior
- Load scan results from database
- View in TUI or output to console
- Much faster than re-scanning

### Implementation Steps

#### 1. Add CLI Command (`src/main.rs`)
```rust
Commands::View {
    /// Scan ID to view (default: latest)
    #[arg(value_name = "SCAN_ID")]
    scan_id: Option<i64>,

    /// Database path
    #[arg(long)]
    db_path: Option<PathBuf>,

    /// Show in console instead of TUI
    #[arg(short, long)]
    no_tui: bool,
}
```

#### 2. Load Scan from Database
```rust
async fn view_scan(scan_id: Option<i64>, db_path: PathBuf, no_tui: bool) -> Result<()> {
    let db = ScanDatabase::open(&db_path)?;

    let scan_id = if let Some(id) = scan_id {
        id
    } else {
        db.get_latest_scan_id()?
    };

    let scan_info = db.get_scan_info(scan_id)?;
    let duplicate_groups = db.get_duplicate_groups(scan_id)?;

    println!("📂 Loading scan from {}", scan_info.start_time);
    println!("   Path: {}", scan_info.root_path.display());
    println!("   Files scanned: {}", scan_info.files_scanned);
    println!("   Groups found: {}", scan_info.groups_found);

    if no_tui {
        display_console_output(&duplicate_groups);
    } else {
        // Launch TUI with pre-loaded data
        let mut app = App::from_database_scan(scan_info, duplicate_groups)?;
        tui::run(&mut app).await?;
    }

    Ok(())
}
```

#### 3. Update App to Support Read-Only Mode
```rust
impl App {
    pub fn from_database_scan(
        scan_info: ScanInfo,
        groups: Vec<DuplicateGroup>
    ) -> Result<Self> {
        // Create app from database data
        // Mark as read-only (can't delete files from old scan)
    }
}
```

#### 4. Add Scan History Command
```rust
Commands::History {
    /// Number of scans to show
    #[arg(short = 'n', long, default_value = "10")]
    count: usize,
}

fn show_history(db_path: &Path, count: usize) -> Result<()> {
    let db = ScanDatabase::open(db_path)?;
    let scans = db.get_recent_scans(count)?;

    println!("📊 Recent Scans:\n");
    for (i, scan) in scans.iter().enumerate() {
        println!("{:2}. ID: {} - {}",
            i + 1,
            scan.id,
            scan.start_time.format("%Y-%m-%d %H:%M:%S")
        );
        println!("    Path: {}", scan.root_path.display());
        println!("    Files: {}, Groups: {}",
            scan.files_scanned,
            scan.groups_found
        );
        println!();
    }

    Ok(())
}
```

### Testing Strategy
1. Create scan with demo data
2. View scan by ID
3. View latest scan
4. Test console vs TUI output
5. Test scan history listing

### Success Metrics
- Can view any completed scan
- Instant loading (no re-scan)
- Same TUI experience as live scan

### Files to Modify
- `src/main.rs` - Add view and history commands
- `src/database.rs` - Add query methods
- `src/app.rs` - Support read-only mode

---

## Feature 7: Web Interface 🌐

### Priority: FUTURE (Major Feature)
### Estimated Time: 8-12 hours
### Complexity: Very High
### Depends On: Feature 5 (SQLite Database)

### Overview
Add web-based interface for interactive duplicate management with real-time updates.

### Implementation Plan

#### Technology Stack
- **Backend**: `axum` web framework
- **Real-time**: Server-Sent Events (SSE)
- **Frontend**: HTML + `htmx` (simple, no build step)
- **Styling**: Tailwind CSS via CDN

#### 1. Add Dependencies (`Cargo.toml`)
```toml
[dependencies]
axum = "0.7"
tower = "0.4"
tower-http = { version = "0.5", features = ["fs", "trace"] }
tokio-stream = "0.1"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

#### 2. Create Web Server Module (`src/web/mod.rs`)
```rust
use axum::{
    Router,
    routing::{get, post},
    extract::State,
    response::sse::{Event, Sse},
};

pub struct WebServer {
    db: Arc<ScanDatabase>,
    scan_state: Arc<RwLock<Option<ScanProgress>>>,
}

pub async fn serve(
    db_path: PathBuf,
    port: u16,
) -> Result<()> {
    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/scans", get(list_scans))
        .route("/api/scan/:id", get(get_scan))
        .route("/api/scan/:id/groups", get(get_groups))
        .route("/api/delete", post(delete_file))
        .route("/api/rename", post(rename_file))
        .route("/api/events", get(sse_handler))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(app_state);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
```

#### 3. Frontend with htmx (`templates/index.html`)
```html
<!DOCTYPE html>
<html>
<head>
    <title>DupScanner</title>
    <script src="https://unpkg.com/htmx.org@1.9.10"></script>
    <script src="https://cdn.tailwindcss.com"></script>
</head>
<body class="bg-gray-900 text-white">
    <div class="container mx-auto p-4">
        <h1 class="text-3xl font-bold mb-4">DupScanner</h1>

        <!-- Scan list -->
        <div hx-get="/api/scans" hx-trigger="load" hx-target="#scan-list">
            <div id="scan-list"></div>
        </div>

        <!-- Duplicate groups -->
        <div id="groups"></div>

        <!-- SSE for real-time updates -->
        <script>
            const eventSource = new EventSource('/api/events');
            eventSource.onmessage = (e) => {
                // Update UI with real-time scan progress
            };
        </script>
    </div>
</body>
</html>
```

#### 4. API Endpoints
- `GET /` - Serve HTML interface
- `GET /api/scans` - List all scans
- `GET /api/scan/:id` - Get scan details
- `GET /api/scan/:id/groups` - Get duplicate groups
- `POST /api/delete` - Delete a file
- `POST /api/rename` - Rename a file
- `GET /api/events` - SSE endpoint for real-time updates

### Testing Strategy
1. Test web server startup
2. Test API endpoints
3. Test SSE real-time updates
4. Test file deletion via web
5. Test file renaming
6. UI/UX testing

### Files to Create
- `src/web/mod.rs` - Web server
- `src/web/handlers.rs` - Request handlers
- `templates/index.html` - Main page
- `templates/components/*.html` - htmx components

---

## Feature 8: Content Similarity Detection 🔍

### Priority: FUTURE (Advanced)
### Estimated Time: 6-8 hours
### Complexity: Very High

### Overview
Find similar (not just identical) files using perceptual hashing and content analysis.

### Use Cases
- Find near-duplicate images with different compression
- Find similar documents with minor edits
- Find renamed files with same content

### Implementation Plan

#### 1. Add Dependencies
```toml
[dependencies]
image = "0.24"  # For image perceptual hashing
img_hash = "3.2"  # Perceptual image hashing
edit_distance = "2.1"  # Text similarity
```

#### 2. Add Similarity Detection Module (`src/similarity.rs`)
```rust
pub enum SimilarityMethod {
    PerceptualHash,  // For images
    ContentHash,     // For text
    Acoustic,        // For audio
}

pub fn calculate_similarity(
    file1: &Path,
    file2: &Path,
    method: SimilarityMethod
) -> Result<f64> {
    // Returns 0.0 to 1.0 similarity score
}
```

#### 3. Add Similarity Threshold to Config
```rust
pub struct ScanConfig {
    // ... existing fields
    pub similarity_threshold: f64,  // 0.0 to 1.0, default 1.0 (exact match)
}
```

### Testing Strategy
1. Test with identical files (score = 1.0)
2. Test with slightly modified images
3. Test with edited text files
4. Verify threshold behavior

---

## Implementation Order

### Phase 1: Quick Wins (Week 1)
1. ✅ Quick Hash Strategy (2-3 hours)
2. ✅ Exclusion Patterns (1-2 hours)
3. ✅ Improve Console Output (1-2 hours)
4. ✅ Batch Processing (2 hours)

**Total: 6-9 hours**

### Phase 2: Major Features (Week 2)
5. ✅ SQLite Database Backend (4-6 hours)
6. ✅ Serve Old Scans (2-3 hours)

**Total: 6-9 hours**

### Phase 3: Advanced Features (Future)
7. ⏳ Web Interface (8-12 hours)
8. ⏳ Content Similarity (6-8 hours)

**Total: 14-20 hours**

---

## Testing & Quality Assurance

### After Each Feature
1. Unit tests for new functions
2. Integration tests with demo data
3. Performance benchmarks
4. Git commit with detailed message

### Before Each Commit
- Run `cargo test`
- Run `cargo clippy`
- Test with real data
- Update README if user-facing

---

## Success Metrics

### Performance
- 50%+ faster scans with quick hash
- Lower memory usage with batching
- Sub-second loading of old scans

### Usability
- Clear, helpful output
- Flexible exclusion patterns
- Easy access to scan history

### Reliability
- No data loss
- Graceful error handling
- Can resume interrupted scans

---

## Risk Mitigation

### Backwards Compatibility
- Migrate old JSON state files to SQLite
- Keep JSON as fallback option
- Version database schema

### Data Safety
- SQLite WAL mode for crash recovery
- Transaction-based writes
- Backup before destructive operations

### Performance
- Benchmark before/after each feature
- Profile with large datasets
- Optimize hot paths

---

Ready to implement! Starting with Feature 1: Quick Hash Strategy.
