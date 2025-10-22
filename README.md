# DupScanner - Intelligent Duplicate File Scanner

A powerful, terminal-based duplicate file scanner written in Rust with an intuitive TUI (Text User Interface). DupScanner helps you find and remove duplicate files efficiently while providing intelligent suggestions on which files to keep or delete.

## Features

### Core Functionality
- **Fast Scanning**: Efficiently scans directories using file size and SHA-256 hash detection
- **Intelligent Suggestions**: Smart algorithm suggests which duplicates to delete based on:
  - File location (temporary/downloads directories)
  - Path depth (prefers files closer to root)
  - File age (prefers older, more established files)
  - Filename patterns (detects "copy", "duplicate", etc.)
- **Interactive TUI**: Beautiful, intuitive terminal interface for reviewing duplicates
- **Safe Deletion**: All files are backed up before deletion
- **Pause & Resume**: Save scan state and resume later
- **Real-time Processing**: Review and delete duplicates as they're found
- **Grouped Display**: Duplicates are grouped by hash and sorted by wasted space

### Safety Features
- Automatic backup before any deletion
- Backup records with restore capability
- State persistence for crash recovery
- Manual file selection with visual feedback

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

### Basic Scan
```bash
# Scan a directory
dupscanner scan /path/to/directory

# Scan with minimum file size (10KB)
dupscanner scan /path/to/directory --min-size 10240

# Scan with size range (1KB to 100MB)
dupscanner scan /path/to/directory --min-size 1024 --max-size 104857600

# Scan and save state for resume
dupscanner scan /path/to/directory --save-state

# Resume previous scan
dupscanner scan /path/to/directory --resume
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

## TUI Controls

### Navigation
- `j` / `Down Arrow` - Select next file in current group
- `k` / `Up Arrow` - Select previous file in current group
- `n` / `Right Arrow` - Next duplicate group
- `p` / `Left Arrow` - Previous duplicate group

### Actions
- `Space` - Toggle mark file for deletion
- `a` - Auto-mark suggested files (based on intelligent algorithm)
- `o` - Mark all except oldest file
- `d` - Delete marked files (creates backup first)
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
│   ├── scanner.rs        # File scanning and hashing
│   ├── duplicates.rs     # Duplicate detection and grouping
│   ├── state.rs          # State persistence for pause/resume
│   ├── suggestions.rs    # Intelligent deletion suggestions
│   ├── backup.rs         # Backup management before deletion
│   ├── app.rs            # Application state and logic
│   └── tui.rs            # Terminal UI with ratatui
├── Cargo.toml
└── README.md
```

### Key Components

#### Scanner (`scanner.rs`)
- Walks directory tree using `walkdir`
- Filters files by size constraints
- Groups files by size (quick duplicate detection)
- Computes SHA-256 hashes for verification
- Provides progress callbacks for UI updates

#### Duplicate Finder (`duplicates.rs`)
- Takes size-grouped files from scanner
- Computes hashes only for files with matching sizes
- Creates duplicate groups by hash
- Calculates wasted space per group
- Supports real-time duplicate addition

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
- Files grouped by size before hashing (avoids unnecessary hashing)
- SHA-256 streaming with 8KB buffer (memory efficient)
- Lazy hash computation (only when needed)
- Parallel processing ready (can add rayon for multi-threading)

### Typical Performance
- **Small files** (<1MB): ~1000 files/second
- **Large files** (>100MB): Limited by I/O and hash computation
- **Memory**: ~100MB for 100k files

## Examples

### Example 1: Clean Downloads Folder
```bash
dupscanner scan ~/Downloads --min-size 1048576 --save-state
# Scans Downloads for files >1MB, saves state
# In TUI: press 'a' to auto-mark suggested duplicates
# Review marked files, press 'd' to delete
```

### Example 2: Large Media Library
```bash
dupscanner scan /media/photos --min-size 10240
# Scans photos, minimum 10KB
# Navigate through duplicate groups with 'n'/'p'
# Use 'o' to mark all except oldest in each group
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
- Auto-delete files without user confirmation
- Delete the last remaining copy of any file
- Cross filesystem boundaries by default
- Follow symbolic links (prevents loops)

### Best Practices
1. Always use `--save-state` for large scans
2. Review suggestions before deleting
3. Keep backups for at least 30 days
4. Test on small directories first
5. Exclude system directories (`/etc`, `/bin`, etc.)

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
