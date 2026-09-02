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

use crate::backup::BackupManager;
use crate::database::ScanDatabase;
use crate::deletion::{plan_deletions, DeleteMethod, Deleter};
use crate::duplicates::DuplicateGroup;
use crate::engine::{EngineEvent, RemovedPaths, ScanSession};
use crate::report::{self, GroupReport};
use crate::scanner::{ScanConfig, ScanProgress};
use anyhow::{bail, Context, Result};
use axum::body::{Body, Bytes};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Query, Request, State as AxumState};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::stream::{Stream, StreamExt};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::broadcast;
use tokio_stream::wrappers::BroadcastStream;

const INDEX_HTML: &str = include_str!("../assets/index.html");
const APP_JS: &str = include_str!("../assets/app.js");
const STYLE_CSS: &str = include_str!("../assets/style.css");

const CSP: &str = "default-src 'self'; img-src 'self' blob: data:; media-src 'self' blob:; \
                   style-src 'self' 'unsafe-inline'; object-src 'self'; frame-src 'self'";

/// Files up to this size are read into memory; larger ones are streamed.
const READ_WHOLE_LIMIT: u64 = 50 * 1024 * 1024;
/// Files above this size are refused for preview/download.
const PREVIEW_LIMIT: u64 = 2 * 1024 * 1024 * 1024;
const STREAM_CHUNK: usize = 256 * 1024;
const MAX_PAGE: usize = 500;
const DEFAULT_PAGE: usize = 25;

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

// ---------------------------------------------------------------------------
// State

pub(crate) struct State {
    root: PathBuf,
    /// Canonical form of `root`, used for containment checks.
    canonical_root: PathBuf,
    groups: Vec<DuplicateGroup>,
    progress: ScanProgress,
    scan_complete: bool,
    elapsed: Option<Duration>,
    started: Instant,
    deleter: Deleter,
    /// Handle to the running engine, if any.
    removed: Option<RemovedPaths>,
    /// Paths deleted through the UI while the engine was still running.
    /// Snapshots from the engine are filtered against this set.
    deleted_during_scan: HashSet<PathBuf>,
    /// Renames performed while the engine was still running, applied to
    /// every snapshot the engine sends afterwards.
    renamed_during_scan: HashMap<PathBuf, PathBuf>,
    database: Option<ScanDatabase>,
    scan_id: Option<i64>,
    /// Bumped on every change to `groups`.
    version: u64,
}

impl State {
    fn is_scanning(&self) -> bool {
        !self.scan_complete
    }

    fn elapsed(&self) -> Duration {
        self.elapsed.unwrap_or_else(|| self.started.elapsed())
    }

    fn contains_path(&self, path: &Path) -> bool {
        self.groups
            .iter()
            .any(|g| g.files.iter().any(|f| f.path == path))
    }

    fn status_json(&self) -> Value {
        json!({
            "root": self.root,
            "scanning": self.is_scanning(),
            "complete": self.scan_complete,
            "progress": {
                "files_scanned": self.progress.scanned_count,
                "bytes_scanned": self.progress.total_size,
                "unreadable": self.progress.skipped,
            },
            "elapsed_seconds": self.elapsed().as_secs_f64(),
            "group_count": self.groups.len(),
            "duplicate_files": self.groups.iter().map(|g| g.file_count()).sum::<usize>(),
            "wasted_space": self.groups.iter().map(|g| g.wasted_space).sum::<u64>(),
            "delete_method": self.deleter.method().label(),
            "delete_method_description": self.deleter.method().description(),
            "scan_id": self.scan_id,
            "version": self.version,
        })
    }

    fn groups_summary_json(&self) -> Value {
        json!({
            "version": self.version,
            "group_count": self.groups.len(),
            "duplicate_files": self.groups.iter().map(|g| g.file_count()).sum::<usize>(),
            "wasted_space": self.groups.iter().map(|g| g.wasted_space).sum::<u64>(),
        })
    }

    /// Apply UI-side edits made during the scan to a snapshot from the engine.
    fn adopt_snapshot(&mut self, mut groups: Vec<DuplicateGroup>) {
        if !self.deleted_during_scan.is_empty() {
            for g in &mut groups {
                g.remove_paths(&self.deleted_during_scan);
            }
        }
        if !self.renamed_during_scan.is_empty() {
            for g in &mut groups {
                for f in &mut g.files {
                    if let Some(new) = self.renamed_during_scan.get(&f.path) {
                        f.path = new.clone();
                    }
                }
            }
        }
        groups.retain(|g| !g.is_empty());
        self.groups = groups;
        self.version += 1;
    }

    fn persist_groups(&mut self) {
        if let (Some(db), Some(id)) = (&mut self.database, self.scan_id) {
            let _ = db.save_groups(id, &self.groups);
            let _ = db.complete_scan(id, self.progress.scanned_count, self.groups.len());
        }
    }
}

type Shared = Arc<Mutex<State>>;

#[derive(Clone)]
struct SseMessage {
    event: &'static str,
    data: String,
}

#[derive(Clone)]
struct AppState {
    state: Shared,
    events: broadcast::Sender<SseMessage>,
}

impl AppState {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn broadcast(&self, event: &'static str, data: Value) {
        let _ = self.events.send(SseMessage {
            event,
            data: data.to_string(),
        });
    }
}

// ---------------------------------------------------------------------------
// Errors

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<(StatusCode, String)> for ApiError {
    fn from(v: (StatusCode, String)) -> Self {
        ApiError(v.0, v.1)
    }
}

impl From<JsonRejection> for ApiError {
    fn from(r: JsonRejection) -> Self {
        ApiError(StatusCode::BAD_REQUEST, format!("Invalid JSON body: {}", r.body_text()))
    }
}

fn bad_request(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}

// ---------------------------------------------------------------------------
// Validation

/// Resolve `requested` and check that it is inside the scan root and is one
/// of the files in a current duplicate group. Nothing else is ever read,
/// renamed or deleted.
pub(crate) fn validate_path(state: &State, requested: &Path) -> Result<PathBuf, (StatusCode, String)> {
    if requested.as_os_str().is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Missing path".into()));
    }
    if !requested.is_absolute() {
        return Err((StatusCode::BAD_REQUEST, "Path must be absolute".into()));
    }
    let canonical = fs::canonicalize(requested)
        .map_err(|_| (StatusCode::NOT_FOUND, format!("{} does not exist", requested.display())))?;
    if !canonical.starts_with(&state.canonical_root) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("{} is outside the scanned directory", requested.display()),
        ));
    }
    if !state.contains_path(&canonical) {
        return Err((
            StatusCode::FORBIDDEN,
            format!("{} is not part of any duplicate group in this scan", requested.display()),
        ));
    }
    Ok(canonical)
}

/// A new file name must be exactly one normal path component.
pub(crate) fn validate_new_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("New name must not be empty".into());
    }
    if name != name.trim() {
        return Err("New name must not start or end with whitespace".into());
    }
    if name.contains('\0') {
        return Err("New name must not contain NUL".into());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("New name must not contain path separators".into());
    }
    if name == "." || name == ".." {
        return Err("New name must be a file name, not . or ..".into());
    }
    if name.len() > 255 {
        return Err("New name is too long (255 bytes max)".into());
    }
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(()),
        _ => Err("New name must be a single file name".into()),
    }
}

/// Map a requested path onto the exact path stored in the groups, accepting
/// either the stored form or any spelling that canonicalizes to it. Paths
/// that match nothing are returned unchanged so `plan_deletions` can reject
/// them with its own message.
fn resolve_known_path(state: &State, requested: &Path) -> PathBuf {
    if state.contains_path(requested) {
        return requested.to_path_buf();
    }
    if let Ok(canonical) = fs::canonicalize(requested) {
        if canonical.starts_with(&state.canonical_root) && state.contains_path(&canonical) {
            return canonical;
        }
    }
    requested.to_path_buf()
}

// ---------------------------------------------------------------------------
// Filtering

fn extension_lower(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn file_type(path: &Path) -> &'static str {
    match extension_lower(path).as_str() {
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "webp" | "heic" | "heif" | "tif" | "tiff" | "svg" | "ico"
        | "avif" | "raw" | "cr2" | "nef" | "arw" | "dng" | "psd" => "image",
        "mp4" | "mov" | "avi" | "mkv" | "webm" | "wmv" | "flv" | "m4v" | "mpg" | "mpeg" | "3gp" | "ts" => "video",
        "mp3" | "wav" | "flac" | "ogg" | "oga" | "m4a" | "aac" | "aiff" | "aif" | "wma" | "opus" | "mid"
        | "midi" => "audio",
        "pdf" | "doc" | "docx" | "txt" | "md" | "rtf" | "odt" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp"
        | "csv" | "pages" | "numbers" | "key" | "epub" | "mobi" => "document",
        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar" | "dmg" | "iso" | "jar" | "war" => {
            "archive"
        }
        "rs" | "js" | "mjs" | "cjs" | "jsx" | "tsx" | "py" | "rb" | "go" | "java" | "c" | "h" | "cpp" | "hpp"
        | "cc" | "cs" | "swift" | "kt" | "kts" | "php" | "html" | "htm" | "css" | "scss" | "less" | "json"
        | "yaml" | "yml" | "toml" | "xml" | "sh" | "bash" | "zsh" | "sql" | "lua" | "pl" | "r" | "m" | "vue"
        | "svelte" | "ipynb" | "lock" => "code",
        _ => "other",
    }
}

fn size_bucket(size: u64) -> &'static str {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if size < 10 * KB {
        "tiny"
    } else if size < MB {
        "small"
    } else if size < 100 * MB {
        "medium"
    } else {
        "large"
    }
}

#[derive(Debug, Default, Deserialize)]
struct GroupsQuery {
    offset: Option<usize>,
    limit: Option<usize>,
    path: Option<String>,
    size: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

fn group_matches(group: &DuplicateGroup, path_filter: &str, size: &str, kind: &str) -> bool {
    if size != "all" && size_bucket(group.file_size()) != size {
        return false;
    }
    let path_filter = path_filter.trim().to_lowercase();
    group.files.iter().any(|f| {
        let path_ok = path_filter.is_empty() || f.path.to_string_lossy().to_lowercase().contains(&path_filter);
        let type_ok = kind == "all" || file_type(&f.path) == kind;
        path_ok && type_ok
    })
}

// ---------------------------------------------------------------------------
// Handlers: static assets

async fn nosniff(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    res.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    res
}

async fn index() -> Response {
    let mut res = Html(INDEX_HTML).into_response();
    res.headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    res
}

async fn app_js() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        APP_JS,
    )
        .into_response()
}

async fn style_css() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        STYLE_CSS,
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Handlers: API

async fn api_scan(AxumState(app): AxumState<AppState>) -> Json<Value> {
    Json(app.lock().status_json())
}

async fn api_groups(AxumState(app): AxumState<AppState>, Query(q): Query<GroupsQuery>) -> Json<Value> {
    let limit = q.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    let offset = q.offset.unwrap_or(0);
    let path_filter = q.path.unwrap_or_default();
    let size = q.size.unwrap_or_else(|| "all".into());
    let kind = q.kind.unwrap_or_else(|| "all".into());

    let state = app.lock();
    let matching: Vec<&DuplicateGroup> = state
        .groups
        .iter()
        .filter(|g| group_matches(g, &path_filter, &size, &kind))
        .collect();
    let total = matching.len();
    let page: Vec<GroupReport> = matching
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(report::group_report)
        .collect();
    Json(json!({
        "total": total,
        "offset": offset,
        "limit": limit,
        "version": state.version,
        "groups": page,
    }))
}

async fn api_events(
    AxumState(app): AxumState<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = app.events.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| async move {
        match item {
            Ok(msg) => Some(Ok(Event::default().event(msg.event).data(msg.data))),
            // A slow client missed some messages; the next one carries the
            // current version, which is all the browser needs.
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    )
}

#[derive(Debug, Deserialize)]
struct DeleteRequest {
    paths: Vec<String>,
}

async fn api_delete(
    AxumState(app): AxumState<AppState>,
    body: Result<Json<DeleteRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(req) = body?;
    if req.paths.is_empty() {
        return Err(bad_request("No paths given"));
    }

    let app2 = app.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<Value, ApiError> {
        let mut state = app2.lock();
        let wanted: HashSet<PathBuf> = req
            .paths
            .iter()
            .map(|p| resolve_known_path(&state, Path::new(p)))
            .collect();
        let plan = plan_deletions(&state.groups, &wanted).map_err(|e| bad_request(e.to_string()))?;
        let report = state.deleter.delete_planned(&plan);
        let deleted = report.deleted_paths();

        for group in &mut state.groups {
            group.remove_paths(&deleted);
        }
        state.groups.retain(|g| !g.is_empty());
        if state.is_scanning() {
            if let Some(removed) = &state.removed {
                removed.add_all(deleted.iter().cloned());
            }
            state.deleted_during_scan.extend(deleted.iter().cloned());
        }
        if state.scan_id.is_some() {
            state.persist_groups();
        }
        state.version += 1;

        let mut deleted_list: Vec<&PathBuf> = deleted.iter().collect();
        deleted_list.sort();
        let failed: Vec<Value> = report
            .failures()
            .map(|(p, e)| json!({ "path": p, "error": e }))
            .collect();
        Ok(json!({
            "deleted": deleted_list,
            "failed": failed,
            "bytes_freed": report.bytes_freed(),
            "version": state.version,
        }))
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("Deletion task failed: {e}")))??;

    let summary = app.lock().groups_summary_json();
    app.broadcast("groups", summary);
    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
struct RenameRequest {
    path: String,
    new_name: String,
}

async fn api_rename(
    AxumState(app): AxumState<AppState>,
    body: Result<Json<RenameRequest>, JsonRejection>,
) -> Result<Json<GroupReport>, ApiError> {
    let Json(req) = body?;
    validate_new_name(&req.new_name).map_err(bad_request)?;

    let app2 = app.clone();
    let report = tokio::task::spawn_blocking(move || -> Result<GroupReport, ApiError> {
        let mut state = app2.lock();
        let old = validate_path(&state, Path::new(&req.path))?;
        let parent = old
            .parent()
            .ok_or_else(|| bad_request("Cannot rename a path without a parent directory"))?;
        let new = parent.join(&req.new_name);
        if new == old {
            return Err(bad_request("The new name is the same as the current name"));
        }
        if fs::symlink_metadata(&new).is_ok() {
            return Err(ApiError(
                StatusCode::CONFLICT,
                format!("{} already exists", new.display()),
            ));
        }
        fs::rename(&old, &new).map_err(|e| {
            ApiError(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to rename {}: {e}", old.display()),
            )
        })?;

        let scanning = state.is_scanning();
        let mut updated: Option<GroupReport> = None;
        for group in &mut state.groups {
            if let Some(f) = group.files.iter_mut().find(|f| f.path == old) {
                f.path = new.clone();
                updated = Some(report::group_report(group));
                break;
            }
        }
        if scanning {
            // Later snapshots from the engine still carry the old path.
            let key = state
                .renamed_during_scan
                .iter()
                .find(|(_, v)| **v == old)
                .map(|(k, _)| k.clone())
                .unwrap_or_else(|| old.clone());
            state.renamed_during_scan.insert(key, new.clone());
        }
        if state.scan_id.is_some() {
            state.persist_groups();
        }
        state.version += 1;
        updated.ok_or_else(|| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Group vanished during rename".into()))
    })
    .await
    .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("Rename task failed: {e}")))??;

    let summary = app.lock().groups_summary_json();
    app.broadcast("groups", summary);
    Ok(Json(report))
}

#[derive(Debug, Deserialize)]
struct FileQuery {
    path: Option<String>,
    download: Option<String>,
}

fn inline_allowed(mime: &mime_guess::Mime) -> bool {
    use mime_guess::mime;
    match (mime.type_(), mime.subtype()) {
        (mime::IMAGE, sub) => sub != mime::SVG,
        (mime::VIDEO, _) | (mime::AUDIO, _) => true,
        (mime::APPLICATION, mime::PDF) => true,
        (mime::TEXT, mime::PLAIN) => true,
        _ => false,
    }
}

fn content_disposition(kind: &str, file_name: &str) -> HeaderValue {
    let ascii: String = file_name
        .chars()
        .map(|c| if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' { c } else { '_' })
        .collect();
    let encoded = utf8_percent_encode(file_name, NON_ALPHANUMERIC).to_string();
    let value = format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}");
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

async fn api_file(
    AxumState(app): AxumState<AppState>,
    Query(q): Query<FileQuery>,
) -> Result<Response, ApiError> {
    let requested = q.path.unwrap_or_default();
    let path = {
        let state = app.lock();
        validate_path(&state, Path::new(&requested))?
    };
    let force_download = matches!(q.download.as_deref(), Some("1") | Some("true"));

    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|e| ApiError(StatusCode::NOT_FOUND, format!("{}: {e}", path.display())))?;
    if !meta.is_file() {
        return Err(bad_request(format!("{} is not a regular file", path.display())));
    }
    let size = meta.len();
    if size > PREVIEW_LIMIT {
        return Err(ApiError(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Files over 2 GB cannot be previewed or downloaded through the web UI".into(),
        ));
    }

    let guessed = mime_guess::from_path(&path).first_or_octet_stream();
    let (content_type, disposition) = if !force_download && inline_allowed(&guessed) {
        let ct = if guessed.type_() == mime_guess::mime::TEXT {
            format!("{guessed}; charset=utf-8")
        } else {
            guessed.to_string()
        };
        (ct, "inline")
    } else {
        ("application/octet-stream".to_string(), "attachment")
    };
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".into());

    let body = if size <= READ_WHOLE_LIMIT {
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("Read failed: {e}")))?;
        Body::from(bytes)
    } else {
        let file = tokio::fs::File::open(&path)
            .await
            .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, format!("Open failed: {e}")))?;
        let stream = futures_util::stream::unfold(Some(file), |file| async move {
            let mut file = file?;
            let mut buf = vec![0u8; STREAM_CHUNK];
            match file.read(&mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    Some((Ok::<Bytes, std::io::Error>(Bytes::from(buf)), Some(file)))
                }
                Err(e) => Some((Err(e), None)),
            }
        });
        Body::from_stream(stream)
    };

    let mut res = Response::new(body);
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    headers.insert(header::CONTENT_DISPOSITION, content_disposition(disposition, &file_name));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

async fn api_shutdown() -> Json<Value> {
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(200)).await;
        std::process::exit(0);
    });
    Json(json!({ "ok": true }))
}

// ---------------------------------------------------------------------------
// Wiring

fn router(app: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/api/scan", get(api_scan))
        .route("/api/groups", get(api_groups))
        .route("/api/events", get(api_events))
        .route("/api/delete", post(api_delete))
        .route("/api/rename", post(api_rename))
        .route("/api/file", get(api_file))
        .route("/api/shutdown", post(api_shutdown))
        .layer(middleware::from_fn(nosniff))
        .layer(tower_http::limit::RequestBodyLimitLayer::new(1 << 20))
        .with_state(app)
}

fn build_deleter(method: DeleteMethod) -> Result<Deleter> {
    let backup = if method == DeleteMethod::Backup {
        Some(BackupManager::open_default()?)
    } else {
        None
    };
    Ok(Deleter::new(method, backup))
}

/// Consume engine events on a plain thread and push them into the shared
/// state, notifying SSE subscribers.
fn run_engine_thread(session: ScanSession, app: AppState) {
    while let Some(ev) = session.next() {
        match ev {
            EngineEvent::Progress(p) => {
                let status = {
                    let mut state = app.lock();
                    state.progress = p;
                    state.status_json()
                };
                app.broadcast("progress", status);
            }
            EngineEvent::Groups(groups) => {
                let summary = {
                    let mut state = app.lock();
                    state.adopt_snapshot(groups);
                    state.groups_summary_json()
                };
                app.broadcast("groups", summary);
            }
            EngineEvent::Complete {
                finder,
                progress,
                elapsed,
            } => {
                let status = {
                    let mut state = app.lock();
                    state.adopt_snapshot(finder.groups().to_vec());
                    state.progress = progress;
                    state.elapsed = Some(elapsed);
                    state.scan_complete = true;
                    state.removed = None;
                    state.deleted_during_scan.clear();
                    state.renamed_during_scan.clear();
                    let root = state.root.clone();
                    let scanned = state.progress.scanned_count;
                    let groups = state.groups.clone();
                    if let Some(db) = state.database.as_mut() {
                        match db.record_completed_scan(&root, scanned, &groups) {
                            Ok(id) => state.scan_id = Some(id),
                            Err(e) => eprintln!("warning: could not record scan: {e}"),
                        }
                    }
                    state.status_json()
                };
                app.broadcast("complete", status);
                return;
            }
        }
    }
}

async fn bind(port: u16) -> Result<tokio::net::TcpListener> {
    let candidates: Vec<u16> = if port == 0 {
        vec![0]
    } else {
        (0..=20u16).filter_map(|i| port.checked_add(i)).collect()
    };
    let mut last_err = None;
    for p in candidates {
        match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, p)).await {
            Ok(l) => return Ok(l),
            Err(e) => last_err = Some(e),
        }
    }
    Err(anyhow::anyhow!(
        "could not bind 127.0.0.1:{port} (or the next 20 ports): {}",
        last_err.map(|e| e.to_string()).unwrap_or_default()
    ))
}

pub fn serve(opts: ServeOptions) -> Result<()> {
    let deleter = build_deleter(opts.delete_method)?;
    let (events, _) = broadcast::channel::<SseMessage>(256);

    let mut database = match (&opts.db_path, opts.scan_id) {
        (Some(p), _) => Some(ScanDatabase::open(p)?),
        (None, Some(_)) => bail!("--scan-id needs a scan database (do not combine it with --no-record)"),
        (None, None) => None,
    };

    let (state, session) = match opts.scan_id {
        Some(id) => {
            let db = database.as_mut().context("scan database unavailable")?;
            let info = db.get_scan_info(id).with_context(|| format!("Scan {id} not found"))?;
            let mut groups = db.load_duplicate_groups(id)?;
            // Drop files that no longer exist so the view reflects the disk.
            let mut changed = false;
            for g in &mut groups {
                let gone: HashSet<PathBuf> =
                    g.files.iter().filter(|f| !f.path.exists()).map(|f| f.path.clone()).collect();
                if !gone.is_empty() {
                    g.remove_paths(&gone);
                    changed = true;
                }
            }
            groups.retain(|g| !g.is_empty());
            if changed {
                let _ = db.save_groups(id, &groups);
            }
            let root = info.root_path.clone();
            let canonical_root = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            let state = State {
                root,
                canonical_root,
                groups,
                progress: ScanProgress {
                    scanned_count: info.files_scanned.max(0) as usize,
                    ..Default::default()
                },
                scan_complete: true,
                elapsed: None,
                started: Instant::now(),
                deleter,
                removed: None,
                deleted_during_scan: HashSet::new(),
                renamed_during_scan: HashMap::new(),
                database,
                scan_id: Some(id),
                version: 1,
            };
            (state, None)
        }
        None => {
            let root = opts.config.root_path.clone();
            let canonical_root = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
            let session = ScanSession::start(opts.config.clone());
            let removed = session.removed_paths();
            let state = State {
                root,
                canonical_root,
                groups: Vec::new(),
                progress: ScanProgress::default(),
                scan_complete: false,
                elapsed: None,
                started: Instant::now(),
                deleter,
                removed: Some(removed),
                deleted_during_scan: HashSet::new(),
                renamed_during_scan: HashMap::new(),
                database,
                scan_id: None,
                version: 0,
            };
            (state, Some(session))
        }
    };

    let app = AppState {
        state: Arc::new(Mutex::new(state)),
        events,
    };

    if let Some(session) = session {
        let app_for_thread = app.clone();
        std::thread::spawn(move || run_engine_thread(session, app_for_thread));
    }

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let listener = bind(opts.port).await?;
        let addr = listener.local_addr()?;
        let url = format!("http://127.0.0.1:{}", addr.port());
        println!("Listening on {url}");
        if opts.open_browser {
            if let Err(e) = open::that(&url) {
                eprintln!("Could not open a browser: {e}. Open {url} manually.");
            }
        }
        axum::serve(listener, router(app))
            .await
            .context("web server failed")?;
        Ok(())
    })
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::FileInfo;
    use std::time::SystemTime;
    use tempfile::TempDir;

    fn info(path: &Path) -> FileInfo {
        FileInfo {
            path: path.to_path_buf(),
            size: 4,
            quick_hash: None,
            hash: Some("h".into()),
            modified: SystemTime::UNIX_EPOCH,
            depth: path.components().count(),
        }
    }

    fn state_for(root: &Path, groups: Vec<DuplicateGroup>) -> State {
        State {
            root: root.to_path_buf(),
            canonical_root: fs::canonicalize(root).unwrap(),
            groups,
            progress: ScanProgress::default(),
            scan_complete: true,
            elapsed: None,
            started: Instant::now(),
            deleter: Deleter::new(DeleteMethod::Permanent, None),
            removed: None,
            deleted_during_scan: HashSet::new(),
            renamed_during_scan: HashMap::new(),
            database: None,
            scan_id: None,
            version: 0,
        }
    }

    /// Root with a duplicate pair (a, b), a non-duplicate inside the root
    /// (solo), and a file outside the root.
    fn fixture() -> (TempDir, PathBuf, State) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("root");
        fs::create_dir_all(root.join("sub")).unwrap();
        let a = root.join("a.txt");
        let b = root.join("sub").join("b.txt");
        fs::write(&a, b"pair").unwrap();
        fs::write(&b, b"pair").unwrap();
        fs::write(root.join("solo.txt"), b"solo").unwrap();
        fs::write(dir.path().join("outside.txt"), b"pair").unwrap();

        let canonical_root = fs::canonicalize(&root).unwrap();
        let group = DuplicateGroup::new(
            "h".into(),
            vec![
                info(&canonical_root.join("a.txt")),
                info(&canonical_root.join("sub").join("b.txt")),
            ],
        );
        let state = state_for(&root, vec![group]);
        (dir, root, state)
    }

    #[test]
    fn validate_path_accepts_group_members() {
        let (_dir, root, state) = fixture();
        let ok = validate_path(&state, &root.join("a.txt")).unwrap();
        assert_eq!(ok, fs::canonicalize(root.join("a.txt")).unwrap());
        // A non-canonical spelling of the same file also resolves.
        let ok2 = validate_path(&state, &root.join("sub").join("..").join("a.txt")).unwrap();
        assert_eq!(ok, ok2);
    }

    #[test]
    fn validate_path_rejects_files_inside_root_but_not_in_a_group() {
        let (_dir, root, state) = fixture();
        let err = validate_path(&state, &root.join("solo.txt")).unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert!(err.1.contains("not part of any duplicate group"));
    }

    #[test]
    fn validate_path_rejects_files_outside_root() {
        let (dir, _root, state) = fixture();
        let err = validate_path(&state, &dir.path().join("outside.txt")).unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert!(err.1.contains("outside"));
        // Traversal that escapes the root is caught after canonicalization.
        let err = validate_path(&state, &_root.join("..").join("outside.txt")).unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    #[test]
    fn validate_path_rejects_missing_relative_and_empty_paths() {
        let (_dir, root, state) = fixture();
        assert_eq!(
            validate_path(&state, &root.join("missing.txt")).unwrap_err().0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            validate_path(&state, Path::new("relative/a.txt")).unwrap_err().0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(validate_path(&state, Path::new("")).unwrap_err().0, StatusCode::BAD_REQUEST);
        assert_eq!(validate_path(&state, Path::new("/etc/passwd")).unwrap_err().0, StatusCode::FORBIDDEN);
    }

    #[test]
    fn validate_path_rejects_symlink_pointing_outside_root() {
        let (dir, root, state) = fixture();
        let link = root.join("link.txt");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.path().join("outside.txt"), &link).unwrap();
            let err = validate_path(&state, &link).unwrap_err();
            assert_eq!(err.0, StatusCode::FORBIDDEN);
        }
    }

    #[test]
    fn new_name_validation() {
        assert!(validate_new_name("report.pdf").is_ok());
        assert!(validate_new_name("with spaces.txt").is_ok());
        assert!(validate_new_name("v1..2.txt").is_ok());
        assert!(validate_new_name(".hidden").is_ok());
        for bad in [
            "", " lead.txt", "trail.txt ", "a/b.txt", "a\\b.txt", "..", ".", "../x.txt", "nul\0.txt", "/abs.txt",
        ] {
            assert!(validate_new_name(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(validate_new_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn filters_classify_size_and_type() {
        assert_eq!(size_bucket(100), "tiny");
        assert_eq!(size_bucket(10 * 1024), "small");
        assert_eq!(size_bucket(5 * 1024 * 1024), "medium");
        assert_eq!(size_bucket(500 * 1024 * 1024), "large");
        assert_eq!(file_type(Path::new("/x/photo.JPG")), "image");
        assert_eq!(file_type(Path::new("/x/clip.mov")), "video");
        assert_eq!(file_type(Path::new("/x/song.flac")), "audio");
        assert_eq!(file_type(Path::new("/x/notes.txt")), "document");
        assert_eq!(file_type(Path::new("/x/site.tar.gz")), "archive");
        assert_eq!(file_type(Path::new("/x/main.rs")), "code");
        assert_eq!(file_type(Path::new("/x/blob")), "other");

        let (_dir, _root, state) = fixture();
        let g = &state.groups[0];
        assert!(group_matches(g, "", "all", "all"));
        assert!(group_matches(g, "sub/b", "all", "document"));
        assert!(group_matches(g, "SUB", "tiny", "all"));
        assert!(!group_matches(g, "nope", "all", "all"));
        assert!(!group_matches(g, "", "large", "all"));
        assert!(!group_matches(g, "", "all", "image"));
    }

    #[test]
    fn inline_only_for_safe_media_types() {
        let mime = |p: &str| mime_guess::from_path(p).first_or_octet_stream();
        assert!(inline_allowed(&mime("a.png")));
        assert!(inline_allowed(&mime("a.mp4")));
        assert!(inline_allowed(&mime("a.mp3")));
        assert!(inline_allowed(&mime("a.pdf")));
        assert!(inline_allowed(&mime("a.txt")));
        assert!(!inline_allowed(&mime("a.html")));
        assert!(!inline_allowed(&mime("a.svg")));
        assert!(!inline_allowed(&mime("a.xml")));
        assert!(!inline_allowed(&mime("a.js")));
        assert!(!inline_allowed(&mime("a.css")));
        assert!(!inline_allowed(&mime("a.bin")));
    }

    #[test]
    fn content_disposition_escapes_names() {
        let v = content_disposition("attachment", "we\"ird\\ name é.txt");
        let s = v.to_str().unwrap();
        assert!(s.starts_with("attachment; filename=\"we_ird_ name _.txt\""));
        assert!(s.contains("filename*=UTF-8''we%22ird%5C%20name%20%C3%A9%2Etxt"));
    }

    #[test]
    fn snapshot_adoption_applies_deletes_and_renames() {
        let (_dir, _root, mut state) = fixture();
        let a = state.groups[0].files[0].path.clone();
        let b = state.groups[0].files[1].path.clone();
        let c = state.canonical_root.join("c.txt");
        let renamed_b = state.canonical_root.join("sub").join("renamed.txt");
        state.scan_complete = false;
        state.deleted_during_scan.insert(a.clone());
        state.renamed_during_scan.insert(b.clone(), renamed_b.clone());

        let snapshot = vec![DuplicateGroup::new("h".into(), vec![info(&a), info(&b), info(&c)])];
        state.adopt_snapshot(snapshot);
        assert_eq!(state.groups.len(), 1);
        let paths: Vec<&PathBuf> = state.groups[0].files.iter().map(|f| &f.path).collect();
        assert!(!paths.contains(&&a));
        assert!(paths.contains(&&renamed_b));
        assert!(paths.contains(&&c));
        assert_eq!(state.version, 1);

        // A group that collapses to one file disappears.
        state.adopt_snapshot(vec![DuplicateGroup::new("h".into(), vec![info(&a), info(&c)])]);
        assert!(state.groups.is_empty());
    }
}
