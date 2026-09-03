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

The keeper is the lowest score. Ties go to the shallower path, then the older file, then the shorter path, so the choice is the same every run. Only the filename is inspected for copy patterns, so a folder called "Copy Editing" does not taint the files inside it.

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
r                   rename the selected file     e / Enter   open it with the default app
/                   filter by path substring     z / t       cycle size / type filter     x  clear filters
d / D               delete marked files (group / all matching groups), after confirmation
Tab                 Duplicates / Statistics / Help      ?  help      q  quit
```

Groups appear while the scan is still running; you can start reviewing immediately. Marks are tied to file paths, so they survive re-sorting as new groups arrive, and files you delete or rename mid-scan stay deleted or renamed in later snapshots.

Both UIs use the same rules for filtering (size buckets, file types, path substring), for auto-selecting (the six rules under `m`, which replace existing marks in the groups they touch and never mark every copy), and for renaming and deleting. What you can do in one you can do in the other, apart from image preview, which only the browser can draw; the TUI opens the file in its default application instead.

## Web UI

`dupscanner serve PATH` scans and serves a single-page app on `http://127.0.0.1:8080` (use `--port`, or `--port 0` for a free port; `--no-open` skips launching the browser). It offers live progress, filtering by path, size and type, pagination, previews for images, video, audio, PDF and text, inline and batch rename, the same auto-select rules as the TUI (applied to the current page or to all matching groups, and computed by the server so the two UIs can never disagree), per-group mark and delete buttons, a confirmation dialog that lists every file, a statistics panel, and a help dialog. `dupscanner serve --scan-id N` serves a recorded scan instead of scanning again.

The TUI's keyboard is available in the browser too: `j`/`k` and `n`/`p` move a cursor between files and groups, `Space` marks, `a`/`A`, `o`/`O` and `m` auto-select, `c`/`C` clear, `d`/`D` delete after confirmation, `r` renames, `Enter` previews, `/`, `z` and `t` drive the filters, `Tab` toggles statistics and `?` opens help.

HTML, SVG and script files are only ever offered as downloads, never rendered, so a malicious filename or file cannot run inside the page.

## Commands

```
dupscanner scan [PATH] [--json | --yolo] [scan options]
dupscanner serve [PATH] [--port N] [--no-open] [--scan-id ID] [scan options]
dupscanner history [-n COUNT] [--db FILE]
dupscanner view ID [--json | --plain] [--delete-method M] [--db FILE]
dupscanner forget ID [--db FILE]
dupscanner restore list | file PATH [--to DEST] | clean [--older-than DAYS]
dupscanner demo [PATH] [-n FILES] [-d DUPLICATES]
```

Scan options, shared by `scan` and `serve`:

```
-m, --min-size SIZE          ignore smaller files (1, 10KB, 5MB, 1.5GiB)   [default: 1]
-x, --max-size SIZE          ignore larger files
-e, --exclude PATTERN        extra glob to exclude; a bare name prunes whole directories
    --no-default-excludes    do not skip .git, node_modules, target, dist, build, caches, ...
    --delete-method METHOD   trash (default), backup, permanent
    --db FILE                scan database                                  [default: ~/.local/share/dupscanner/scans.db]
    --no-record              do not record this scan
```

Every completed scan is recorded in one SQLite database, whatever mode produced it. `history` lists them, `view` reopens one in the TUI (or prints it), and `serve --scan-id` reopens one in the browser. Files that have disappeared since the scan are dropped when a scan is reopened.

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
