# DupScanner Architecture

This document provides detailed information about the internal architecture of DupScanner.

## Overview

DupScanner is built using a modular architecture with clear separation of concerns:

```
┌─────────────────────────────────────────────────────────┐
│                         CLI                              │
│                      (main.rs)                          │
└────────────────────┬────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────┐
│                    Application                          │
│                     (app.rs)                            │
│  ┌──────────┬──────────┬──────────┬──────────────────┐ │
│  │ Scanner  │  Finder  │  Backup  │  State Manager   │ │
│  └──────────┴──────────┴──────────┴──────────────────┘ │
└────────────────────┬────────────────────────────────────┘
                     │
                     ▼
┌─────────────────────────────────────────────────────────┐
│                         TUI                              │
│                     (tui.rs)                            │
│                   [ratatui + crossterm]                 │
└─────────────────────────────────────────────────────────┘
```

## Module Details

### 1. Main (`main.rs`)

**Responsibility**: CLI interface and command routing

**Key Features**:
- Command-line argument parsing with `clap`
- Subcommand routing (scan, resume, list)
- Application initialization
- Error handling at top level

**Data Flow**:
```
User Input → CLI Parser → Command Handler → App Creation → TUI Launch
```

### 2. Scanner (`scanner.rs`)

**Responsibility**: File discovery and hashing

**Key Structures**:
```rust
pub struct Scanner {
    config: ScanConfig,
    size_groups: HashMap<u64, Vec<FileInfo>>,
    scanned_count: usize,
    total_size: u64,
}

pub struct FileInfo {
    path: PathBuf,
    size: u64,
    hash: Option<String>,
    modified: SystemTime,
    depth: usize,
}
```

**Algorithm**:
1. Walk directory tree (using `walkdir`)
2. Filter files by size constraints
3. Group files by size (HashMap<u64, Vec<FileInfo>>)
4. Compute SHA-256 hash lazily (only when needed)

**Performance Optimizations**:
- Size-based pre-filtering avoids hashing unique files
- Streaming hash computation (8KB buffer)
- Lazy evaluation - hashes computed only for size matches
- Progress callbacks for UI responsiveness

### 3. Duplicate Finder (`duplicates.rs`)

**Responsibility**: Identify actual duplicates using hashes

**Key Structures**:
```rust
pub struct DuplicateFinder {
    groups: Vec<DuplicateGroup>,
    total_duplicates: usize,
    total_wasted_space: u64,
}

pub struct DuplicateGroup {
    hash: String,
    files: Vec<FileInfo>,
    total_size: u64,
    wasted_space: u64,
}
```

**Algorithm**:
```
1. Receive size_groups from Scanner
2. For each group with len > 1:
   a. Compute hash for each file
   b. Group by hash
   c. Keep only groups with len > 1
3. Sort groups by wasted_space (descending)
4. Calculate statistics
```

**Wasted Space Calculation**:
```rust
wasted_space = file_size × (duplicate_count - 1)
```

### 4. Suggestion Engine (`suggestions.rs`)

**Responsibility**: Intelligently suggest which files to delete

**Scoring System**:
```rust
Temporary directory:  100 points
"copy" in filename:    80 points
Downloads directory:   60 points
Deeper path:           40 points
Longer filename:       30 points
Newer file:            20 points
```

**Decision Algorithm**:
```
1. Score each file based on criteria
2. Identify files NOT suggested for deletion
3. Among non-suggested files, prefer oldest
4. If all files suggested, keep one with lowest score
```

**Example**:
```
File A: /home/user/document.pdf         (score: 0)
File B: /tmp/document.pdf               (score: 100)
File C: /home/user/Downloads/document.pdf (score: 60)

Suggestion: Keep A, delete B and C
```

### 5. Backup Manager (`backup.rs`)

**Responsibility**: Safe file deletion with backup

**Key Structures**:
```rust
pub struct BackupManager {
    backup_dir: PathBuf,
    records: Vec<BackupRecord>,
}

pub struct BackupRecord {
    original_path: PathBuf,
    backup_path: PathBuf,
    size: u64,
    timestamp: DateTime<Utc>,
}
```

**Backup Process**:
```
1. Validate file exists
2. Create timestamped backup filename
3. Copy file to backup directory
4. Record backup in JSON database
5. Delete original file
6. Update statistics
```

**Backup Location**:
- Linux/macOS: `~/.local/share/dupscanner/backups/`
- Windows: `%APPDATA%\dupscanner\backups\`

### 6. State Persistence (`state.rs`)

**Responsibility**: Save/restore scan state

**Key Structures**:
```rust
pub struct ScanState {
    config: ScanConfig,
    size_groups: HashMap<u64, Vec<FileInfo>>,
    duplicate_groups: Vec<DuplicateGroup>,
    scanned_count: usize,
    total_size: u64,
    timestamp: DateTime<Utc>,
    completed: bool,
}
```

**State File Format** (JSON):
```json
{
  "config": {
    "root_path": "/path/to/scan",
    "min_size": 1024,
    "max_size": null,
    "save_state": true
  },
  "scanned_count": 12345,
  "duplicate_groups": [...],
  "timestamp": "2025-01-15T10:30:00Z",
  "completed": true
}
```

**State Management**:
- Auto-save on quit (if --save-state enabled)
- Manual save with 's' key
- Resume from last state
- State file per scan directory (using path hash)

### 7. Application (`app.rs`)

**Responsibility**: Central application state and coordination

**State Machine**:
```
Scanning → FindingDuplicates → ReviewingDuplicates → Completed
    ↓              ↓                    ↓
  (can pause)  (automated)      (user interaction)
```

**Key Methods**:
- `new()` - Create fresh scan
- `from_state_file()` - Resume from saved state
- `save_state()` - Persist current state
- `delete_marked_files()` - Execute deletions with backup
- `next_group()` / `previous_group()` - Navigate duplicates
- `mark_all_suggested()` - Auto-mark based on suggestions

### 8. TUI (`tui.rs`)

**Responsibility**: Interactive terminal interface

**Layout**:
```
┌────────────────────────────────────────────┐
│              Header                        │
├──────────────────┬─────────────────────────┤
│                  │                         │
│   Group List     │    File List            │
│   (30%)          │    (70%)                │
│                  │                         │
│  Group 1 ───►    │  [ ] /path/file1.txt   │
│  Group 2         │  [X] /tmp/file2.txt    │
│  Group 3         │  [ ] /path/file3.txt   │
│                  │                         │
├──────────────────┴─────────────────────────┤
│              Footer / Status               │
└────────────────────────────────────────────┘
```

**Event Loop**:
```
1. Draw UI frame
2. Poll for input (100ms timeout)
3. Handle keyboard events
4. Update application state
5. Repeat
```

**Key Bindings**:
- Navigation: j/k (vim) or arrows
- Group switching: n/p or left/right
- Marking: space, a (auto), o (keep oldest)
- Actions: d (delete), s (save), q (quit)
- Help: ?

## Data Flow

### Complete Scan Flow

```
1. User runs: dupscanner scan /path

2. CLI parses arguments → ScanConfig

3. App initializes:
   - Create Scanner with config
   - Create DuplicateFinder
   - Create BackupManager
   - Load BackupManager records

4. TUI launches:
   - Enable raw mode
   - Enter alternate screen
   - Start event loop

5. Scanning Phase:
   - Scanner walks directory
   - Groups files by size
   - Updates UI with progress
   - User can pause/resume

6. Finding Duplicates Phase:
   - For each size group with >1 files:
     - Compute hashes
     - Group by hash
     - Create DuplicateGroup
   - Sort by wasted space

7. Review Phase:
   - Display groups in TUI
   - Show suggestions with scores
   - User marks files for deletion
   - User confirms deletion

8. Deletion:
   - For each marked file:
     - Create backup
     - Delete original
     - Update records
   - Remove empty groups
   - Save state

9. Cleanup:
   - Save final state
   - Restore terminal
   - Exit
```

## Design Patterns

### 1. Builder Pattern
Used for `ScanConfig` construction from CLI args

### 2. State Pattern
`AppState` enum manages application lifecycle

### 3. Repository Pattern
`BackupManager` and `StateManager` abstract persistence

### 4. Observer Pattern
Scanner uses callbacks for progress updates

### 5. Strategy Pattern
`SuggestionEngine` uses pluggable scoring criteria

## Error Handling

**Error Types**:
- `anyhow::Error` for general errors
- `io::Error` for file operations
- `serde_json::Error` for serialization

**Error Propagation**:
```rust
fn operation() -> Result<()> {
    file_operation().context("Failed during file op")?;
    Ok(())
}
```

**User-Facing Errors**:
- Displayed in TUI status bar
- Logged but don't crash application
- Allow recovery when possible

## Testing Strategy

### Unit Tests
Each module has tests for:
- Core functionality
- Edge cases
- Error conditions

### Integration Tests
- End-to-end scan scenarios
- State persistence/restore
- Backup/restore operations

### Manual Testing
- TUI interaction
- Large directory scans
- Resume functionality

## Performance Considerations

### Memory Usage
- File metadata: ~200 bytes per file
- 100k files ≈ 20MB memory
- HashMap overhead included

### CPU Usage
- SHA-256: ~400 MB/s (single core)
- I/O bound for large files
- Can parallelize with rayon

### Disk I/O
- Sequential reads for hashing
- Minimal writes (state saves)
- Backup is copy operation

### Optimization Opportunities
1. **Parallel hashing**: Use rayon for multi-core
2. **Incremental scanning**: Track filesystem changes
3. **Content-based chunking**: Deduplicate file parts
4. **Bloom filters**: Quick duplicate checks
5. **Memory-mapped I/O**: Faster hashing for large files

## Security Considerations

### Safety Measures
- No automatic deletion without user confirmation
- All deletions create backups
- Symlinks not followed (prevents loops)
- Permission checks before operations

### Potential Issues
- Backup directory can grow large
- State files contain full paths
- No encryption of backups
- No privilege escalation protection

## Future Enhancements

### Planned Features
1. Parallel processing with rayon
2. Content-aware detection (perceptual hashing)
3. Network/cloud storage support
4. Plugin system for custom rules
5. Export reports (JSON, CSV, HTML)

### Architecture Changes
1. Move to async I/O (tokio)
2. Add database backend (SQLite)
3. Separate CLI and TUI into different binaries
4. Add REST API for remote control
5. Implement plugin system with dynamic loading

## Dependencies

### Core
- `clap` - CLI parsing
- `ratatui` - TUI framework
- `crossterm` - Terminal control
- `serde` + `serde_json` - Serialization

### Utilities
- `walkdir` - Directory traversal
- `sha2` - Hashing
- `chrono` - Timestamps
- `dirs` - User directories
- `anyhow` - Error handling

### Development
- `tempfile` - Testing
- `criterion` - Benchmarking (future)

## Building and Development

### Build Profiles

**Debug** (fast compile, slow runtime):
```bash
cargo build
```

**Release** (slow compile, fast runtime):
```bash
cargo build --release
```

### Code Organization

**Style Guide**:
- Rust standard formatting (`rustfmt`)
- Clippy lints enabled
- Documentation for public APIs
- Tests in same file as implementation

**Module Privacy**:
- Public types exported at crate root
- Internal types kept private
- Clear API boundaries

## Contribution Guidelines

See CONTRIBUTING.md for:
- Code style requirements
- Testing expectations
- PR process
- Architecture decision records

## License

MIT License - See LICENSE file
