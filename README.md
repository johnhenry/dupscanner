# dupscanner

Find duplicate files and remove them safely, from the terminal or a local web page.

dupscanner walks a directory, groups files by size, then by a hash of their first 64 KiB, then by a full SHA-256, so only real duplicates are ever grouped. It suggests which copy to keep, lets you review every group before anything is removed, and deletes through the system trash by default.

This is a single Rust binary with no runtime dependencies. It merges two earlier projects: the Rust TUI scanner and the Node.js "Super Dee Duper" web interface. The web UI now runs inside the same binary and shares the engine, the suggestion rules and the deletion safeguards with the terminal UI.

## Install

```sh
cargo install --git https://github.com/johnhenry/dupscanner
```

Or build from a checkout with `cargo build --release`; the binary is `target/release/dupscanner`.

## Quick start

```sh
# Review duplicates in the terminal
dupscanner scan ~/Pictures

# Same scan, reviewed in your browser at http://127.0.0.1:8080
dupscanner serve ~/Pictures

# Machine-readable results
dupscanner scan ~/Pictures --json > dupes.json

# Non-interactive: keep the best copy of each group, trash the rest
dupscanner scan ~/Downloads --yolo

# Same, but delete as you go on a huge tree; Ctrl+C any time, rerun to continue
dupscanner scan /Volumes/Archive --yolo --stream
```

Try it on generated data first:

```sh
dupscanner demo /tmp/dupscanner-demo
dupscanner scan /tmp/dupscanner-demo
```

## How it decides what to keep

Every file in a group gets a score. Positive signals make it a better candidate for deletion; a negative signal protects it.

| Signal | Weight | Example |
|---|---|---|
| Inside a temp or cache directory | +100 | `/tmp/report.pdf` |
| Filename looks like a copy | +80 | `report copy.pdf`, `report (1).pdf`, `report_backup.pdf`; `report 2.pdf` only when `report.pdf` is in the same group |
| Inside Downloads | +60 | `~/Downloads/report.pdf` |
| Inside a backup, old or archive folder | +50 | `~/backup/2019/report.pdf` |
| Deepest path in the group | +20 | |
| Longest filename in the group (by more than 5 characters) | +10 | |
| Inside Documents, Pictures, Photos, Music, Videos, Movies, Projects, src or Desktop | -40 | `~/Documents/report.pdf` |
| Already carries the group's original name (see below) | -30 | `report.pdf` next to `report (1).pdf` |

The keeper is the lowest score. Ties go to the shallower path, then the older file, then the shorter path, so the choice is the same every run. Only the filename is inspected for copy patterns, so a folder called "Copy Editing" does not taint the files inside it.

### Original names

Copies arrive with predictable decorations: `report (1).pdf`, `report copy.pdf`, `report - Copy (2).pdf`, `Copy of report.pdf`, `photo 2.jpg`. dupscanner strips those markers from every member of a group and takes the most common base plus extension as the group's original name. That name is used three ways:

- A file that already has it is preferred as the keeper.
- Files that only differ from it by a marker are scored as copies. A bare trailing digit (`photo 2.jpg`) counts only when `photo.jpg` is also in the group, so sequential names like `chapter 12.md` are left alone.
- When no surviving copy has the original name and the name is free, dupscanner offers to rename the keeper: `N` in the TUI, the "Rename keeper to ..." button in the web UI, or `--rename-keepers` with `--yolo`.

Names never decide what is a duplicate. Size and full SHA-256 do that first; names only decide which copy to keep and what to call it.

In the UIs, `a` (or "Mark suggested copies") marks files with a positive score. `o` (or "Mark all but keeper") marks everything except the keeper. You can always mark files by hand.

## Safety

- **Trash by default.** Deleted files go to the system trash or recycle bin. `--delete-method backup` copies each file into `~/.local/share/dupscanner/backups/` first and `dupscanner restore` puts it back. `--delete-method permanent` is available when you mean it.
- **Never the last copy.** Every deletion is checked against the current groups. A request that would remove all copies of a file is refused, in the TUI, in the web UI and in yolo mode.
- **Only scan results.** The web UI can only read, rename or delete paths that are inside the scanned directory and part of a duplicate group. The server listens on 127.0.0.1 only.
- **Re-checked before removal.** A file whose size changed since it was hashed is skipped.
- **Confirmation.** The TUI and the web UI show what will be removed and how before doing it.

## Terminal UI

```
j / ↓  k / ↑        select file            n / →  p / ←     next / previous group
g / G               first / last group      Space            mark or unmark file
a / A               mark suggested copies (group / all matching groups)
o / O               mark all but keeper (group / all matching groups)
m                   more rules: all but oldest, newest, shortest or longest path
c / C               clear marks (group / all)
r                   rename the selected file     N           rename the keeper to the original name
e / Enter           open with the default app    v           show / hide the image preview pane
/                   filter by path substring     z / t       cycle size / type filter     x  clear filters
d / D               delete marked files (group / all matching groups), after confirmation
Tab                 Duplicates / Statistics / Help      ?  help      q  quit
```

Groups appear while the scan is still running; you can start reviewing immediately. Marks are tied to file paths, so they survive re-sorting as new groups arrive, and files you delete or rename mid-scan stay deleted or renamed in later snapshots.

Both UIs use the same rules for filtering (size buckets, file types, path substring), for auto-selecting (the six rules under `m`, which replace existing marks in the groups they touch and never mark every copy), and for renaming and deleting. What you can do in one you can do in the other.

### Image preview in the terminal

The preview pane (toggle with `v`) shows the selected image for JPEG, PNG, GIF, WebP, BMP, TIFF, ICO and QOI files. Decoding runs on a background thread and results are cached, so scrolling stays responsive. The pane picks the best graphics protocol the terminal offers:

| Terminal | Output |
|---|---|
| kitty, WezTerm, Ghostty, recent Konsole | pixel-perfect (kitty graphics protocol) |
| iTerm2, WezTerm on macOS | pixel-perfect (iTerm2 inline images) |
| foot, mlterm, xterm with sixel, iTerm2 | pixel-perfect (sixel) |
| Terminal.app, tmux without passthrough, everything else | Unicode half-block mosaic, about two pixels per cell |

Other file types show a note and `e` opens them in their default application. Set `DUPSCANNER_NO_IMAGES=1` to disable the pane entirely.

## Unattended runs

`dupscanner scan PATH --yolo` scans everything first, then keeps the best copy of each group and removes the rest. Nothing is freed until the walk finishes.

`dupscanner scan PATH --yolo --stream` removes copies as soon as a group is confirmed by full hashes, while the walk continues. On a large directory space is freed from the first minute, and the run can be stopped at any time:

- **Ctrl+C** finishes the deletion in flight, writes the journal, prints what was done and exits with status 130. A second Ctrl+C quits immediately.
- **Files already removed stay removed.** There is no cache and nothing to resume; running the same command again simply walks the remaining files and continues. Re-hashing unchanged files is the only cost of a restart.
- A group that grows after its first copies were removed is handled again: the keeper is re-chosen among the survivors by the same heuristics and the new arrivals are removed. Because members share a hash, this never loses content.
- `--rename-keepers` still works and runs at the end, once groups can no longer grow, so it is skipped on an interrupted run.

Both modes accept `--delete-method trash|backup|permanent`. Trash is the default even here.

### Deletion journal

Every removal made by any mode (TUI, web UI, `--yolo`, `--yolo --stream`) is written to the scan database with its path, size, method and time. `dupscanner history` shows the count and bytes per scan, including interrupted runs, and `dupscanner view ID --plain` lists the individual files. This is a record of what happened, not an undo; undo is the trash or `dupscanner restore`.

## Web UI

`dupscanner serve PATH` scans and serves a single-page app on `http://127.0.0.1:8080` (use `--port`, or `--port 0` for a free port; `--no-open` skips launching the browser). It offers live progress, filtering by path, size and type, pagination, previews for images, video, audio, PDF and text, inline and batch rename, the same auto-select rules as the TUI (applied to the current page or to all matching groups, and computed by the server so the two UIs can never disagree), per-group mark and delete buttons, a confirmation dialog that lists every file, a statistics panel, and a help dialog. `dupscanner serve --scan-id N` serves a recorded scan instead of scanning again.

The TUI's keyboard is available in the browser too: `j`/`k` and `n`/`p` move a cursor between files and groups, `Space` marks, `a`/`A`, `o`/`O` and `m` auto-select, `c`/`C` clear, `d`/`D` delete after confirmation, `r` renames, `Enter` previews, `/`, `z` and `t` drive the filters, `Tab` toggles statistics and `?` opens help.

HTML, SVG and script files are only ever offered as downloads, never rendered, so a malicious filename or file cannot run inside the page.

## Commands

```
dupscanner scan [PATH] [--json | --yolo [--stream] [--rename-keepers]] [scan options]
dupscanner serve [PATH] [--port N] [--no-open] [--scan-id ID] [scan options]
dupscanner history [-n COUNT] [--db FILE]
dupscanner view ID [--json | --plain] [--delete-method M] [--db FILE]
dupscanner forget ID [--db FILE]
dupscanner restore list | file PATH [--to DEST] | clean [--older-than DAYS]
dupscanner excludes
dupscanner demo [PATH] [-n FILES] [-d DUPLICATES]
```

Scan options, shared by `scan` and `serve`:

```
-m, --min-size SIZE          ignore smaller files (1, 10KB, 5MB, 1.5GiB)   [default: 1]
-x, --max-size SIZE          ignore larger files
-e, --exclude PATTERN        extra glob to exclude; a bare name prunes whole directories
    --no-default-excludes    do not apply the built-in exclusions (see `dupscanner excludes`)
    --delete-method METHOD   trash (default), backup, permanent
    --db FILE                scan database                                  [default: ~/.local/share/dupscanner/scans.db]
    --no-record              do not record this scan
```

Every completed scan is recorded in one SQLite database, whatever mode produced it. `history` lists them, `view` reopens one in the TUI (or prints it), and `serve --scan-id` reopens one in the browser. Files that have disappeared since the scan are dropped when a scan is reopened.

## Built-in exclusions

Some things should never be deduplicated blindly, so they are skipped unless you pass `--no-default-excludes`. `dupscanner excludes` prints the full list with a reason for each entry. The categories:

- **Volume and OS metadata**: `.Trashes`, `.Trash`, `.Spotlight-V100`, `.fseventsd`, `.DocumentRevisions-V100`, `.TemporaryItems`, `.MobileBackups`, `$RECYCLE.BIN`, `System Volume Information`, `lost+found`, `.snapshots`, `.zfs`, plus `.DS_Store`, `Thumbs.db` and `desktop.ini` files.
- **Backups**: `Backups.backupdb`, `*.backupbundle`, `*.sparsebundle`. Deduplicating inside a backup defeats its purpose.
- **Bundles that are really one document or program**: `*.app`, `*.framework`, `*.kext`, `*.plugin`, `*.photoslibrary`, `*.musiclibrary`, `*.imovielibrary`, `*.fcpbundle`, `*.logicx`, `*.lrcat`, `*.lrdata`, virtual machines (`*.pvm`, `*.vmwarevm`, `*.utm`), Xcode projects, and similar. Removing a "duplicate" resource inside one corrupts it.
- **Version control, dependencies, build output and caches**: `.git`, `node_modules`, `vendor`, `.venv`, `target`, `dist`, `build`, `__pycache__`, `.cache`, `.gradle`, `.cargo`, and friends.

Patterns are plain names or `*.ext` globs matched against each entry's name, so a matching directory is pruned with its whole subtree. Add your own with `-e PATTERN`, repeatable.

## Data locations

| What | Where |
|---|---|
| Scan database | `~/.local/share/dupscanner/scans.db` (macOS: `~/Library/Application Support/dupscanner/scans.db`) |
| Backups (`--delete-method backup`) | `<same directory>/backups/` plus `records.json` |

Nothing is written inside the scanned directory.

## Performance notes

- Files with a unique size are never opened. Files that share a size are hashed on their first 64 KiB; only files that also share that prefix are hashed in full.
- Hashing runs in parallel with rayon; the directory walk runs on its own thread behind a bounded queue, so memory does not grow with walk speed.
- Excluded directories are pruned during the walk, so a repository checkout skips `node_modules` and `target` instead of reading and discarding them.
- Symbolic links are not followed. Hard links to the same inode are currently reported as duplicates.

## Development

```sh
cargo build
cargo test
cargo run -- demo /tmp/dupscanner-demo && cargo run -- scan /tmp/dupscanner-demo
```

See `ARCHITECTURE.md` for the module layout.

## Roadmap

- Hard-link detection (skip files that already share an inode)
- Replace duplicates with hard links or reflinks instead of deleting
- Incremental rescans that reuse hashes from the database
- Near-duplicate detection for images

## License

MIT
