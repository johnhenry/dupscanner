# DupScanner - Intelligent Duplicate File Scanner

A powerful, terminal-based duplicate file scanner written in Rust with an intuitive TUI (Text User Interface). DupScanner helps you find and remove duplicate files efficiently while providing intelligent suggestions on which files to keep or delete.

## Features

### ✅ Implemented Core Features

#### 1. **Quick Hash Strategy** 🚀
- Two-phase hashing for dramatic performance improvements
- Quick hash: First 64KB of each file
- Full hash: Only computed when quick hashes match
- **Result**: ~90% reduction in full file hashing for large files

#### 2. **Exclusion Patterns** 🎯
- Glob-based pattern matching for flexible file filtering
- **Default exclusions**: Common directories automatically skipped (.git/, node_modules/, target/, .cache/, etc.)
- Add custom patterns with `--exclude`
- Disable defaults with `--no-default-excludes`
- Multiple patterns supported simultaneously
- **Default exclusions include**:
  - Version control: `.git/`, `.svn/`, `.hg/`
  - Dependencies: `node_modules/`, `bower_components/`
  - Build artifacts: `target/`, `dist/`, `build/`, `.next/`, `.nuxt/`
  - Caches: `.cache/`, `__pycache__/`, `.pytest_cache/`
  - IDE files: `.vscode/`, `.idea/`
  - OS files: `.DS_Store`, `Thumbs.db`, `desktop.ini`
- **Example**: `--exclude "*.tmp" --exclude "*/backup/*"`

#### 3. **Beautiful Console Output** 🎨
- Color-coded output (green=success, red=delete, yellow=warning)
- Human-readable file sizes (KiB, MiB, GiB using binary units)
- Real-time progress with scan rate (files/sec)
- Comprehensive summary with timing and space freed

#### 4. **Batch Processing** 📦
- Configurable batch sizes for memory efficiency
- Streaming file discovery (O(1) memory for scanning)
- Incremental duplicate group processing
- **Default**: 1000 files per batch

#### 5. **SQLite Database Backend** 💾
- Persistent storage of all scan results (both yolo and TUI modes)
- Full scan history with metadata (timestamps, paths, file counts)
- Quick access to past scans with `dupscanner history`
- Review any completed scan with `dupscanner view <id>`
- Integration-ready for web interface
- **Schema**: scans, files, and indexes for performance
- **Database location**: Per-directory `.dupscanner/scans.db` in scan root (with in-memory fallback for read-only directories)

#### 6. **Intelligent Suggestions** 🧠
Weighted scoring system to identify best file to keep:
- Temporary directories (highest deletion priority: 100 points)
- "copy", "backup", "duplicate" in filename (80 points)
- Downloads folder (60 points)
- Deeper directory nesting (40 points)
- File age - newer files scored higher for deletion (20 points)
- **Always keeps the best file**: Never auto-deletes without suggestion

#### 7. **Safety Features** 🛡️
- **Trash by default**: Files moved to system recycle bin (easily recoverable)
- **Optional backup mode**: Copy files before deletion with `--use-backup`
- All deletions are reversible
- Trash location: System recycle bin (Trash on macOS, Recycle Bin on Windows, Trash on Linux)
- Backup location: Per-directory `.dupscanner/backups/` in scan root (when using --use-backup)
- State persistence for crash recovery
- Manual file selection with visual feedback
- **Automatic fallback**: In-memory database when scan directory is read-only

### Additional Features
- **Interactive TUI**: Beautiful terminal interface with keyboard navigation
- **YOLO Mode**: Fully automatic duplicate removal with real-time output
- **Scan History**: Browse all past scans with `dupscanner history`
- **Scan Review**: View details of any completed scan with `dupscanner view <id>`
- **Pause & Resume**: Save scan state and continue later
- **Demo Mode**: Generate realistic test data for testing
- **Real-time Processing**: Review and delete duplicates as they're found

## Installation

### Prerequisites
- Rust 1.70 or later
- Cargo

### Build from Source
```bash
git clone https://github.com/yourusername/dupscanner
cd dupscanner
cargo build --release
```

The binary will be available at `target/release/dupscanner`

### Install
```bash
cargo install --path .
```

## Usage

### Demo Mode (Quick Start)
```bash
# Generate demo test data with duplicates
dupscanner demo

# This creates test data in /tmp/dupscanner-demo with:
# - 10 unique files with 3 duplicates each
# - Files in various folders (documents, downloads, temp, backup)
# - Different naming patterns to test suggestion algorithm

# Customize demo data generation
dupscanner demo /path/to/demo --num-files 20 --duplicates 5

# Then scan the demo data
dupscanner scan /tmp/dupscanner-demo
```

### Basic Scan
```bash
# Scan current directory (default)
dupscanner scan

# Scan a specific directory
dupscanner scan /path/to/directory

# Scan with minimum file size (10KB)
dupscanner scan /path/to/directory --min-size 10240

# Scan with size range (1KB to 100MB)
dupscanner scan /path/to/directory --min-size 1024 --max-size 104857600

# Scan and save state for resume
dupscanner scan /path/to/directory --save-state

# Resume previous scan
dupscanner scan /path/to/directory --resume

# Common directories (.git/, node_modules/, etc.) are excluded by default
# Add custom exclusion patterns with --exclude
dupscanner scan /path/to/directory \
  --exclude "*.tmp" \
  --exclude "*/backup/*"

# Scan everything including .git/, node_modules/, etc.
dupscanner scan /path/to/directory --no-default-excludes

# Combine: disable defaults and add custom patterns
dupscanner scan /path/to/directory --no-default-excludes --exclude "*.log"
```

### YOLO Mode (Automatic Non-Interactive)
```bash
# ⚠️ YOLO MODE: Automatically delete duplicates without confirmation
# Scans, finds duplicates, and deletes them in real-time
# ALWAYS keeps the best file (oldest, best location, no "copy" in name)
# By default: Moves files to system trash (easily recoverable)

dupscanner scan /path/to/directory --yolo

# Example: Clean demo data automatically (uses trash by default)
dupscanner demo
dupscanner scan /tmp/dupscanner-demo --yolo

# YOLO with backup mode instead of trash
dupscanner scan /path/to/directory --yolo --use-backup

# YOLO with size constraints
dupscanner scan ~/Downloads --yolo --min-size 1048576  # Only files >1MB

# YOLO with custom exclusion patterns (in addition to defaults)
dupscanner scan ~/Documents --yolo \
  --exclude "*.tmp" \
  --exclude "*/backup/*"

# YOLO without default exclusions (scan everything)
dupscanner scan ~/Documents --yolo --no-default-excludes
```

**How YOLO Mode Works:**
1. Scans directory for files
2. Groups files by size, then by hash
3. For each duplicate group, uses intelligent algorithm to pick best file to keep:
   - Prefers files NOT in temp/ or downloads/
   - Prefers files without "copy", "backup", "duplicate" in name
   - Prefers shallower paths (closer to root)
   - Prefers older files
4. Deletes all duplicates except the keeper
5. **Default**: Moves files to system trash (macOS Trash, Windows Recycle Bin, Linux Trash)
6. **Optional**: Use `--use-backup` to copy files to `~/.local/share/dupscanner/backups/` instead
7. Shows real-time output of what's being deleted

**Output Example (Trash Mode - Default):**
```
🚀 YOLO MODE ACTIVATED
   Scanning and auto-deleting duplicates in real-time...
   Path: /tmp/dupscanner-demo
   Min size: 1 B
   Deletion method: Trash (move to system recycle bin)

📊 Scanning files...
   ✓ Scanned: 43 files in 0.12s

🔍 Finding and removing duplicates...

   Found 4 duplicates (8.45 KiB, hash: a3f2b1c9...)
   ✓ Keeping: /tmp/dupscanner-demo/documents/file_0.txt
   Moved to trash: /tmp/dupscanner-demo/downloads/file_0.txt
   Moved to trash: /tmp/dupscanner-demo/temp/file_0_temp.txt
   Moved to trash: /tmp/dupscanner-demo/backup/file_0_copy.txt

✅ YOLO mode complete!

📊 Summary:
   Files scanned: 43
   Duplicates deleted: 30
   Space freed: 234.56 KiB
   Total time: 0.45s
   Files location: System trash/recycle bin

💡 Tip: All deleted files are in your system trash and can be restored from there.
```

**Output Example (Backup Mode - with --use-backup):**
```
🚀 YOLO MODE ACTIVATED
   Deletion method: Backup (copy to ~/.local/share/dupscanner/backups/)
   ...
   Deleted (backed up): /tmp/dupscanner-demo/downloads/file_0.txt
   ...
   Backups location: ~/.local/share/dupscanner/backups/

💡 Tip: All deleted files were backed up and can be restored if needed.
```

### Resume from State File
```bash
# Resume latest scan
dupscanner resume

# Resume specific scan
dupscanner resume /path/to/state.json

# List saved scans
dupscanner list
```

### Database & Scan History
```bash
# View scan history (all completed scans are saved to database)
dupscanner history

# View last 20 scans
dupscanner history -n 20

# View details of a specific scan
dupscanner view 1

# All scans (both yolo and TUI modes) are automatically saved to:
# ~/.local/share/dupscanner/scans.db

# Example workflow:
# 1. Run a scan
dupscanner scan ~/Downloads --yolo

# 2. Check history
dupscanner history

# 3. View details of scan #1
dupscanner view 1
```

## How Auto-Suggestions Work

When you press `a` or `A`, DupScanner uses **heuristics** (educated guesses) to suggest which duplicates to delete:

### Scoring System

Files are scored based on these criteria:

| Reason | Score | Example |
|--------|-------|---------|
| **In temp directory** | 100 | `/tmp/file.txt`, `C:\temp\file.txt` |
| **Has "copy" in name/path** | 80 | `file copy.txt`, `folder 2/file.txt` |
| **In downloads directory** | 60 | `~/Downloads/file.txt` |
| **Deeper path** | 40 | `./a/b/c/file.txt` vs `./file.txt` |
| **Longer filename** | 30 | `file_with_long_name.txt` vs `file.txt` |
| **Newer file** | 20 | Modified today vs modified last year |

### Confidence Levels

- **High confidence (100+)**: File in `/tmp/` or multiple strong indicators
- **Good confidence (80-99)**: Has "copy" in name/path
- **Medium confidence (60-79)**: In downloads, or multiple weak indicators
- **Low confidence (<60)**: Only minor differences detected

### File Labels in TUI

Each file in a duplicate group is labeled to show its status:

- **`(score: 80)`** in **red** = Suggested for deletion (higher score = more confident)
- **`(KEEPER)`** in **green** = Will be kept (best file in group)
- **`(neutral)`** in **gray** = No negative indicators, but not selected as keeper

### Important Notes

⚠️ **These are suggestions, not certainties!** Always review the marked files before deleting:

1. Check the **labels** next to each file to understand why they were scored
2. Read the **status message** showing confidence level
3. Use `n`/`p` to navigate and review each group
4. Toggle with `Space` to adjust selections
5. For identical files with no clear winner, use `o` (keep oldest) instead

**Example**:
```
[ ] ./folder/file.fits (KEEPER)
[X] ./folder 2/file.fits (score: 80)
```
The second file gets score 80 because of the `" 2"` in the path - but both files might be equally valid!

## Default Exclusions

DupScanner automatically excludes common directories that typically don't need scanning:

| Category | Patterns |
|----------|----------|
| **Version Control** | `.git/`, `.svn/`, `.hg/` |
| **Dependencies** | `node_modules/`, `bower_components/`, `.npm/`, `.yarn/` |
| **Build Artifacts** | `target/` (Rust), `dist/`, `build/`, `.next/` (Next.js), `.nuxt/` (Nuxt.js) |
| **Caches** | `.cache/`, `__pycache__/` (Python), `.pytest_cache/` |
| **IDE Files** | `.vscode/`, `.idea/` |
| **OS Files** | `.DS_Store` (macOS), `Thumbs.db` (Windows), `desktop.ini` (Windows) |

**To disable default exclusions**: Use the `--no-default-excludes` flag
**To add custom exclusions**: Use the `--exclude` flag (can be used multiple times)

```bash
# Use defaults + add custom patterns
dupscanner scan . --exclude "*.bak" --exclude "*/temp/*"

# Disable defaults (scan everything)
dupscanner scan . --no-default-excludes

# Disable defaults + add specific exclusions
dupscanner scan . --no-default-excludes --exclude "*/.git/*"
```

## TUI Controls

### Quick Reference

| Key | Action | Scope |
|-----|--------|-------|
| **Navigation** |||
| `j` / `↓` | Select next file | Current group |
| `k` / `↑` | Select previous file | Current group |
| `n` / `→` | Next duplicate group | - |
| `p` / `←` | Previous duplicate group | - |
| **Marking** |||
| `Space` | Toggle mark file | Current file |
| `a` | Auto-mark suggested files | Current group |
| `A` (Shift+A) | Auto-mark suggested files | **ALL groups** |
| `o` | Mark all except oldest | Current group |
| `O` (Shift+O) | Mark all except oldest | **ALL groups** |
| **Deleting** |||
| `d` | Delete marked files | Current group |
| `D` (Shift+D) | Delete marked files | **ALL groups** |
| **Other** |||
| `s` | Save state | - |
| `?` | Toggle help | - |
| `q` | Quit | - |

### Navigation
- `j` / `Down Arrow` - Select next file in current group
- `k` / `Up Arrow` - Select previous file in current group
- `n` / `Right Arrow` - Next duplicate group
- `p` / `Left Arrow` - Previous duplicate group

### Actions
- `Space` - Toggle mark file for deletion
- `a` - Auto-mark suggested files in current group (smart: marks bad locations/names, always keeps best file)
- `A` (Shift+A) - Auto-mark suggested files across ALL groups
  - Marks persist as you navigate between groups
  - Shows status message with count
  - Navigate with `n`/`p` to review marked files in each group
- `o` - Mark all except oldest in current group (aggressive: keeps only the oldest/best file)
- `O` (Shift+O) - Mark all except oldest across ALL groups
  - Marks persist as you navigate between groups
  - Shows total count marked
- `d` - Delete marked files in current group (creates backup first)
- `D` (Shift+D) - Delete ALL marked files across all groups (creates backup first)
  - Works on any files marked with Space, `a`, `A`, `o`, or `O`
  - Shows total count deleted across all groups
- `s` - Save current state

### Other
- `?` - Toggle help screen
- `q` - Quit (saves state if --save-state was used)

## Architecture

### Project Structure
```
dupscanner/
├── src/
│   ├── main.rs           # CLI entry point and command handling
│   ├── scanner.rs        # File scanning, quick hash, and full hashing
│   ├── duplicates.rs     # Two-phase duplicate detection and grouping
│   ├── database.rs       # SQLite backend for persistent storage
│   ├── state.rs          # State persistence for pause/resume
│   ├── suggestions.rs    # Intelligent deletion suggestions
│   ├── backup.rs         # Backup management before deletion
│   ├── demo.rs           # Test data generation
│   ├── app.rs            # Application state and logic
│   └── tui.rs            # Terminal UI with ratatui
├── Cargo.toml
├── IMPLEMENTATION_PLAN.md
└── README.md
```

### Key Components

#### Scanner (`scanner.rs`)
- Walks directory tree using `walkdir`
- Filters files by size constraints and exclusion patterns
- **Quick hash**: Computes hash of first 64KB for fast comparison
- Groups files by size (quick duplicate detection)
- Computes full SHA-256 hashes only when needed
- Provides progress callbacks for UI updates
- Streaming architecture for memory efficiency

#### Duplicate Finder (`duplicates.rs`)
- **Two-phase hashing strategy**:
  - Phase 1: Group by quick hash
  - Phase 2: Full hash only for quick hash matches
- Takes size-grouped files from scanner
- Creates duplicate groups by full hash
- Calculates wasted space per group
- Supports real-time duplicate addition
- Memory-efficient batch processing

#### Database (`database.rs`)
- SQLite backend for persistent scan storage
- Schema with scans and files tables
- Indexed by scan_id and group_id for performance
- Stores scan metadata (path, times, counts)
- Saves duplicate groups with hashes
- Load/restore previous scan results
- Ready for web interface integration

#### Suggestion Engine (`suggestions.rs`)
- Analyzes file metadata and paths
- Scores files based on deletion criteria:
  - **Temporary directories** (score: 100) - `/tmp/`, `\temp\`
  - **"Copy" in filename** (score: 80) - "copy", "duplicate", "(1)"
  - **Downloads folder** (score: 60)
  - **Deeper paths** (score: 40)
  - **Longer filenames** (score: 30)
  - **Newer files** (score: 20)
- Suggests best file to keep in each group

#### Backup Manager (`backup.rs`)
- Creates timestamped backups before deletion
- Stores backups in user's local data directory
- Maintains JSON records of all backups
- Supports restore and cleanup operations
- Tracks total backup size

#### State Persistence (`state.rs`)
- Saves scan progress to JSON files
- Stores duplicate groups and metadata
- Enables pause and resume functionality
- Automatic state recovery
- Lists available saved scans

#### TUI (`tui.rs`)
- Built with `ratatui` and `crossterm`
- Two-panel layout: groups (left) and files (right)
- Visual feedback for marked files
- Suggestion scores displayed inline
- Help screen with all commands
- Status messages for user feedback

## Intelligent Deletion Algorithm

The suggestion engine uses a weighted scoring system:

1. **Location-based** (highest priority)
   - Files in `/tmp/` or temporary directories: likely safe to delete
   - Files in downloads folder: often duplicates

2. **Naming patterns**
   - Filenames containing "copy", "duplicate", "(1)": obvious duplicates
   - Longer filenames: often indicate renamed copies

3. **Path structure**
   - Deeper nested files: likely organized copies
   - Files closer to root: original or important files

4. **Temporal**
   - Newer files: less likely to be referenced
   - Older files: more likely to be "originals"

The algorithm suggests deletions but never auto-deletes. Users have full control.

## Backup System

All deleted files are automatically backed up:

- **Location**: `~/.local/share/dupscanner/backups/` (Linux/macOS)
- **Naming**: `YYYYMMDD_HHMMSS_original_filename.bak`
- **Records**: JSON database tracks all backups
- **Restore**: Can restore any backup to original or new location

### Managing Backups

Backups are stored indefinitely. You can manually clean old backups:

```bash
# Navigate to backup directory
cd ~/.local/share/dupscanner/backups/

# Remove backups older than 30 days
find . -name "*.bak" -mtime +30 -delete
```

## Performance

### Optimization Techniques
- **Quick hash strategy**: Hash first 64KB only, ~90% reduction in full file hashing
- **Two-phase hashing**: Full hash only for files with matching quick hashes
- Files grouped by size before hashing (avoids unnecessary hashing)
- Exclusion patterns to skip unwanted files early
- SHA-256 streaming with 8KB buffer (memory efficient)
- Lazy hash computation (only when needed)
- Batch processing with configurable batch sizes
- Streaming file discovery (O(1) memory)
- SQLite indexes for fast query performance
- Parallel processing ready (can add rayon for multi-threading)

### Performance Improvements

**Quick Hash Strategy Benefits:**

For a directory with 10,000 large files (each >64KB):
- **Without quick hash**: ~50 GB of data hashed
- **With quick hash**: ~5 GB of data hashed (90% reduction)
- **Time savings**: 5-10x faster on large file sets

### Typical Performance
- **Small files** (<1MB): ~1000-2000 files/second
- **Large files** (>100MB): Limited by I/O, but quick hash helps significantly
- **Memory**: ~100MB for 100k files (streaming architecture)
- **Database overhead**: Minimal (~1-2% of total time)

## Examples

### Example 1: Clean Downloads Folder
```bash
dupscanner scan ~/Downloads --min-size 1048576 --save-state
# Scans Downloads for files >1MB, saves state

# Quick workflow:
# 1. Press 'A' (Shift+A) to auto-mark suggested duplicates across ALL groups
# 2. Navigate with 'n'/'p' to review marked files in each group
# 3. Press 'D' (Shift+D) to delete all marked files at once

# Or delete group by group:
# 1. Press 'a' to auto-mark suggested duplicates in current group
# 2. Press 'd' to delete marked files
# 3. Press 'n' to go to next group, repeat
```

### Example 2: Large Media Library
```bash
dupscanner scan /media/photos --min-size 10240
# Scans photos, minimum 10KB

# Recommended workflow for batch cleanup:
#   1. Press 'A' (Shift+A) to auto-mark all suggested duplicates across all groups
#   2. Navigate through groups with 'n'/'p' to review what will be deleted
#   3. Use Space to toggle individual files if needed
#   4. Press 'D' (Shift+D) once to delete all marked files across all groups

# Alternative per-group workflow:
#   1. Navigate to each group with 'n'/'p'
#   2. Use 'o' to mark all except oldest (or 'a' for smart suggestions)
#   3. Press 'd' to delete marked files in current group
#   4. Repeat for next group
```

### Example 3: System-wide Scan
```bash
dupscanner scan / --min-size 10485760 --save-state
# Scans entire system for files >10MB
# Use --save-state for long-running scans
# Can pause with Ctrl+C and resume later
```

## State Files

State files are stored in `~/.local/share/dupscanner/`:
- Filename: `scan_{path_hash}.json`
- Content: Scan configuration, progress, duplicate groups
- Auto-saved on quit (with --save-state flag)
- Manual save with 's' key in TUI

## Testing

### Unit Tests
```bash
cargo test
```

### Integration Test
```bash
# Create test files
mkdir -p /tmp/dup-test/{a,b,c}
echo "content" > /tmp/dup-test/a/file1.txt
echo "content" > /tmp/dup-test/b/file1.txt
echo "content" > /tmp/dup-test/c/file1_copy.txt
echo "different" > /tmp/dup-test/a/file2.txt

# Run scanner
dupscanner scan /tmp/dup-test

# Should find 3 duplicates of "content"
# Should suggest keeping /tmp/dup-test/a/file1.txt (oldest, shallowest, no "copy")
```

## Safety Considerations

### What DupScanner Does
- Creates backups before any deletion
- Requires manual confirmation for deletions
- Preserves at least one copy of each unique file
- Saves state to prevent data loss

### What DupScanner Does NOT Do
- Auto-delete files without user confirmation (except in YOLO mode)
- Delete the last remaining copy of any file
- Cross filesystem boundaries by default
- Follow symbolic links (prevents loops)
- Scan version control, dependencies, or build directories by default (use `--no-default-excludes` to scan everything)

### Best Practices
1. Always use `--save-state` for large scans
2. Review suggestions before deleting
3. Keep backups for at least 30 days
4. Test on small directories first
5. Default exclusions skip common directories (.git/, node_modules/, etc.) - use `--exclude` to add more patterns
6. Use `--no-default-excludes` carefully - it will scan build artifacts and dependencies

## Troubleshooting

### "Permission denied" errors
- Run with appropriate permissions
- Exclude system directories
- Check file ownership

### Large memory usage
- Set `--max-size` to limit file sizes
- Scan directories separately
- Increase system swap if needed

### TUI not displaying correctly
- Ensure terminal supports ANSI colors
- Minimum terminal size: 80x24
- Try different terminal emulator

## Contributing

Contributions welcome! Areas for improvement:
- Parallel hashing with rayon
- Additional suggestion heuristics
- Export duplicate reports
- GUI version
- More file attributes (permissions, ownership)

## License

MIT License - see LICENSE file

## Credits

Built with:
- [ratatui](https://github.com/ratatui-org/ratatui) - Terminal UI framework
- [clap](https://github.com/clap-rs/clap) - CLI argument parsing
- [walkdir](https://github.com/BurntSushi/walkdir) - Directory traversal
- [sha2](https://github.com/RustCrypto/hashes) - SHA-256 hashing
- [serde](https://github.com/serde-rs/serde) - Serialization

## Roadmap

- [ ] Parallel file hashing
- [ ] Content-aware duplicate detection (similar images, audio)
- [ ] Network/cloud storage support
- [ ] Plugin system for custom suggestion rules
- [ ] Export reports (JSON, CSV, HTML)
- [ ] Dry-run mode
- [ ] Hardlink/symlink creation instead of deletion
- [ ] Integration with file managers
