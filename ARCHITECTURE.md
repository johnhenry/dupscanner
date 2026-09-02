# Architecture

dupscanner is one binary with one engine and three front ends (terminal UI, web UI, non-interactive CLI modes). Every front end consumes the same event stream and goes through the same deletion layer.

```
scanner.rs ──walker thread──▶ engine.rs ──hasher thread──▶ EngineEvent ──▶ app.rs + tui.rs
                                 │                                     ├─▶ web.rs (+ assets/)
                                 └─ duplicates.rs                      └─▶ main.rs (--json, --yolo)
                                                                              │
deletion.rs ◀──── every removal ─────────────────────────────────────────────┘
   ├─ trash crate
   └─ backup.rs
database.rs  ◀── completed scans, later edits
suggestions.rs ── scoring used by all front ends and report.rs
```

## Modules

| Module | Responsibility |
|---|---|
| `scanner.rs` | `ScanConfig`, `FileInfo`, exclusion matching (`ExclusionMatcher`, compiled once, applied with `WalkDir::filter_entry` so excluded directories are never descended), the walker thread, and the two hash functions. Batches of `FileInfo` go out over a bounded `sync_channel(4)`. |
| `duplicates.rs` | `DuplicateFinder::process_batch`: register files by size; for sizes with more than one file, quick-hash (first 64 KiB) every unhashed member in parallel; for shared quick hashes, full-hash in parallel; rebuild the affected groups from the whole size bucket. `DuplicateGroup` keeps files sorted by path so output is deterministic. |
| `engine.rs` | `ScanSession` spawns the walker and a hasher thread that owns the `DuplicateFinder`. It emits `EngineEvent::Progress`, throttled `EngineEvent::Groups` snapshots (at most every 250 ms) and a final `EngineEvent::Complete { finder, progress, elapsed }`. `RemovedPaths` lets a front end report files it deleted mid-scan so the final result excludes them. |
| `suggestions.rs` | `SuggestionEngine::analyze` scores each file in a group and picks the keeper. Pure function of the `FileInfo`s; used by the TUI, web UI, yolo mode and JSON report. |
| `deletion.rs` | `plan_deletions` validates a set of paths against the current groups (must belong to a group; a group may never lose every member). `Deleter` performs removals via trash, the backup store, or permanently, re-checking file size first. `DeletionReport` carries per-file outcomes. |
| `backup.rs` | Backup store with collision-free filenames (timestamp + short digest), an atomically written `records.json` index, restore and clean-up. |
| `database.rs` | SQLite (WAL) with `scans` and `files` tables. Groups are written in one transaction; `modified` and `depth` are stored so reopened scans still get sensible suggestions. Old databases are migrated in place. |
| `paths.rs` | The data directory and default database path. |
| `report.rs` | Serializable `ScanReport` / `GroupReport` used by `scan --json`, `view --json` and the web API, including keeper flags and reasons. |
| `app.rs` | Front-end-agnostic review state: current groups, selection, marks keyed by path, pending confirmation, deletion through `Deleter`, persistence to the database. |
| `tui.rs` | ratatui rendering and key handling. Installs a panic hook that restores the terminal. All truncation is on character boundaries. |
| `web.rs` + `assets/` | axum server bound to 127.0.0.1 with embedded static assets, SSE progress, and JSON endpoints that mirror `report.rs`. Every file-touching endpoint canonicalizes the path, checks it is inside the scan root and belongs to a current group. |
| `main.rs` | clap CLI. `scan` (TUI, `--json`, `--yolo`), `serve`, `history`, `view`, `forget`, `restore`, `demo`. |
| `demo.rs` | Generates a directory tree with known duplicates for trying the tool. |

## Data flow for a scan

1. `main.rs` builds a `ScanConfig` (canonical root, size limits, exclusion patterns) and a `Deleter`.
2. `ScanSession::start` spawns the walker (in `scanner.rs`) and the hasher thread (in `engine.rs`).
3. The walker sends batches of stat-only `FileInfo`s. The hasher calls `DuplicateFinder::process_batch`, which hashes only what the size and quick-hash tiers require, using rayon.
4. Every 250 ms the hasher publishes a sorted snapshot of groups. Front ends render it immediately; the TUI and web UI allow marking and deleting while the scan is still running.
5. On completion the hasher hands over the `DuplicateFinder`. The front end records the scan in the database.
6. Deletions: front end collects marked paths, `plan_deletions` validates, the user confirms, `Deleter` removes, the groups are updated in memory and in the database. If the scan is still running, the removed paths are also pushed to the engine through `RemovedPaths`.

## Invariants

- A `DuplicateGroup` always has at least two files once exposed to a front end; empties are dropped on every mutation.
- Files are hashed at most once per tier per scan.
- No code path deletes a file that is not a member of a current group, and no group loses its last member through dupscanner.
- The web server never binds to a non-loopback address, and never renders HTML, SVG or scripts from the scanned tree inline.

## Testing

`cargo test` covers hashing tiers, exclusion matching and pruning, grouping (including the two-file case and cross-batch arrival), engine completion and mid-scan removals, deletion planning rules, backup collisions and restore, database round trips, suggestion scoring, path truncation, size parsing, and web path validation.
