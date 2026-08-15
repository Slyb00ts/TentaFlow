// ===== File: code_studio/index.rs — semantic index of a workspace repository (§14) =====
//
// One vector namespace per workspace: scope `cs-<workspace_id>`, namespace
// `code`, created AT the workspace directory (`<workspace>/vectors/`) so the
// index lives and dies with the workspace instead of leaking into the shared
// addon vector tree. Quotas per (org, addon) still apply — the scope IS the
// addon id as far as the namespace manager is concerned.
//
// Four decisions shape this module.
//
// **The index is built from a COMMIT, never from the working tree.** The walk
// reads `git ls-tree` through the broker, so `.gitignore` is honoured by
// construction (an ignored file is not in the tree) rather than by a second
// ignore parser of our own that could disagree with git. It also makes every
// chunk attributable: `{path, lang, start_line, end_line, commit, branch}` is
// exactly what was committed, not what a worktree happened to hold at the time.
//
// **Refresh is a diff, not a walk.** After an accepted patch set, a checkout, a
// pull or a merge the branch moves; `diff_name_status(indexed_commit, head)`
// names the files that changed and only those are re-embedded. The per-file
// ledger (`index_files`) carries the blob id, so even a full rebuild skips
// content it already embedded — which is what makes a run stopped by the time
// budget resumable instead of wasteful.
//
// **A stale index says so.** `index_state` is per branch and only ever advances
// `indexed_commit` after a COMPLETE pass. A partial pass keeps the old commit
// and records why it stopped, so the UI shows "behind" rather than a fresh
// index that silently misses half the repository. Divergence is a soft
// degradation: search still answers, and says it is degraded.
//
// **One run per workspace at a time.** Indexing is git + embeddings + vector
// writes over the same namespace; two concurrent passes over one workspace
// would duplicate work and race on the same `index_state` row. Requests
// therefore queue on a per-workspace lock, and repeated triggers within the
// debounce window collapse into one run.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use dashmap::DashMap;
use parking_lot::Mutex;
use rusqlite::{params, OptionalExtension};
use tokio::sync::broadcast;

use super::git_broker::{Broker, RepoHandle};
use super::models::WorkspaceRecord;
use super::{paths, repository, workspace_db};
use crate::db::DbPool;
use crate::routing::router::Router;
use crate::services::document::extract::{CHUNK_OVERLAP_CHARS, CHUNK_SIZE_CHARS};
use crate::services::vector::backend::{Field, FieldSpec, Metric, UpsertItem};
use crate::services::vector::error::VectorError;
use crate::services::vector::NamespaceManager;
use tentaflow_sdk_spec::{FieldType, FieldValue, Filter};

/// Namespace holding the code chunks of one workspace.
pub const VECTOR_NAMESPACE: &str = "code";

/// Vector scope of a workspace. Mirrors `ps-<project_id>` from Project Studio:
/// the owner is not an addon, but the quota accounting is the same.
pub fn vector_scope(workspace_id: &str) -> String {
    format!("cs-{workspace_id}")
}

/// Files above this never enter the index (§14). A generated bundle or a
/// checked-in binary costs embeddings and returns noise.
pub const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;

/// Directory names skipped wherever they appear in a path. They hold build
/// output and vendored dependencies, which a repository may well have
/// committed — `.gitignore` cannot be relied on to exclude them.
const EXCLUDED_DIR_SEGMENTS: &[&str] = &["node_modules", "target", "dist", ".git"];

/// How many chunks are embedded in one request.
const EMBED_BATCH: usize = 16;

/// Default ceiling on ONE indexing pass. A repository larger than the budget
/// finishes over several passes; the state says "incomplete" in between.
pub const DEFAULT_TIME_BUDGET: Duration = Duration::from_secs(300);

/// Triggers arriving within this window collapse into a single run — a review
/// that accepts eight files must not start eight passes.
pub const REFRESH_DEBOUNCE: Duration = Duration::from_millis(1500);

/// Bytes inspected when deciding whether a blob is binary.
const BINARY_PROBE_BYTES: usize = 8192;

/// Progress entries kept per workspace so a stream that subscribes late (or
/// reconnects) can replay from its cursor.
const PROGRESS_HISTORY: usize = 256;

/// Candidate multiplier for a filtered search: the backend cannot express a
/// path prefix, so the filter runs here over a wider candidate set.
const SEARCH_OVERSAMPLE: usize = 8;

// =============================================================================
// Embeddings
// =============================================================================

/// Source of embeddings for the index. A trait because the index layer must be
/// testable without a model: what it owns is chunking, deltas and state, and
/// those are exactly what a real embedding call would hide behind its cost.
#[async_trait]
pub trait CodeEmbedder: Send + Sync {
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>>;
}

/// Production embedder: the platform executor under the shared `rag-embeddings`
/// alias, through the same call Project Studio ingest uses — one embedding
/// space for project knowledge and code.
pub struct RouterEmbedder {
    router: Arc<Router>,
}

impl RouterEmbedder {
    pub fn new(router: Arc<Router>) -> Self {
        Self { router }
    }
}

#[async_trait]
impl CodeEmbedder for RouterEmbedder {
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
        crate::project_studio::ingest::embed_texts(&self.router, texts).await
    }
}

// =============================================================================
// Chunking
// =============================================================================

/// One chunk before it has an embedding: a line range of one file plus the
/// exact body that range covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedChunk {
    pub start_line: u32,
    pub end_line: u32,
    pub body: String,
}

impl PlannedChunk {
    /// What is actually embedded. The header carries file identity and line
    /// provenance into the vector, while the stored `text` field keeps the raw
    /// body so a search hit renders as source, not as source plus a banner.
    pub fn embed_text(&self, path: &str) -> String {
        format!(
            "// {path}:{}-{}\n{}",
            self.start_line, self.end_line, self.body
        )
    }
}

/// Language slug derived from the extension. Used as filterable metadata and
/// to pick the boundary rule, so a missing entry degrades to window chunking
/// rather than to no indexing.
pub fn lang_of(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    match ext.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" | "jsx" => "typescript",
        "go" => "go",
        "java" => "java",
        "cs" => "csharp",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" | "hh" => "cpp",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "sh" | "bash" | "fish" => "shell",
        "sql" => "sql",
        "html" | "htm" => "html",
        "css" | "scss" => "css",
        "md" | "markdown" => "markdown",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "xml" => "xml",
        _ => "text",
    }
}

/// Start of a top-level unit. Deliberately a shallow, language-agnostic rule:
/// a real parser per language is not cheap, and §14 asks for syntactic
/// boundaries only "where they are cheap". Anything this misses simply falls
/// back to a window, so a wrong guess costs nothing but a chunk boundary.
fn is_boundary(lang: &str, line: &str) -> bool {
    if lang == "markdown" {
        return line.starts_with('#');
    }
    if line.starts_with(char::is_whitespace) || line.trim().is_empty() {
        return false;
    }
    const OPENERS: &[&str] = &[
        "pub ",
        "fn ",
        "struct ",
        "enum ",
        "trait ",
        "impl ",
        "mod ",
        "macro_rules!",
        "class ",
        "def ",
        "func ",
        "function ",
        "interface ",
        "type ",
        "const ",
        "static ",
        "export ",
        "async ",
        "public ",
        "private ",
        "protected ",
        "internal ",
        "package ",
        "namespace ",
        "module ",
        "#[",
        "@",
    ];
    OPENERS.iter().any(|kw| line.starts_with(kw))
}

/// Splits a file into chunks: whole top-level units packed up to the chunk
/// budget, and overlapping line windows inside a unit that is larger than the
/// budget on its own. Line numbers are 1-based and inclusive on both ends.
pub fn chunk_file(lang: &str, content: &str) -> Vec<PlannedChunk> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    // Unit starts, always including line 1 so the first unit is complete.
    let mut starts: Vec<usize> = vec![0];
    for (idx, line) in lines.iter().enumerate().skip(1) {
        if is_boundary(lang, line) {
            starts.push(idx);
        }
    }

    let mut out = Vec::new();
    let mut pending_start: Option<usize> = None;
    let mut pending_end = 0usize;
    let mut pending_chars = 0usize;

    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(lines.len()) - 1;
        let unit_chars: usize = (start..=end).map(|l| lines[l].chars().count() + 1).sum();

        if unit_chars > CHUNK_SIZE_CHARS {
            if let Some(s) = pending_start.take() {
                out.push(make_chunk(&lines, s, pending_end));
                pending_chars = 0;
            }
            out.extend(window_chunks(&lines, start, end));
            continue;
        }
        match pending_start {
            Some(s) if pending_chars + unit_chars > CHUNK_SIZE_CHARS => {
                out.push(make_chunk(&lines, s, pending_end));
                pending_start = Some(start);
                pending_end = end;
                pending_chars = unit_chars;
            }
            Some(_) => {
                pending_end = end;
                pending_chars += unit_chars;
            }
            None => {
                pending_start = Some(start);
                pending_end = end;
                pending_chars = unit_chars;
            }
        }
    }
    if let Some(s) = pending_start {
        out.push(make_chunk(&lines, s, pending_end));
    }
    out.retain(|c| !c.body.trim().is_empty());
    out
}

fn make_chunk(lines: &[&str], start: usize, end: usize) -> PlannedChunk {
    let body = lines[start..=end].join("\n");
    // A single line can be longer than the whole budget (a minified bundle is
    // one line of a megabyte). Windowing cannot split it, so the cut happens
    // here — no chunk may exceed the embedding context, whatever its shape.
    let body = if body.chars().count() > CHUNK_SIZE_CHARS {
        body.chars().take(CHUNK_SIZE_CHARS).collect()
    } else {
        body
    };
    PlannedChunk {
        start_line: start as u32 + 1,
        end_line: end as u32 + 1,
        body,
    }
}

/// Overlapping windows over one oversized unit. The overlap is measured in
/// characters but cut on line boundaries, and every window advances by at
/// least one line so the walk always terminates.
fn window_chunks(lines: &[&str], start: usize, end: usize) -> Vec<PlannedChunk> {
    let mut out = Vec::new();
    let mut cursor = start;
    while cursor <= end {
        let mut chars = 0usize;
        let mut last = cursor;
        while last <= end {
            let len = lines[last].chars().count() + 1;
            if chars + len > CHUNK_SIZE_CHARS && last > cursor {
                last -= 1;
                break;
            }
            chars += len;
            last += 1;
        }
        let last = last.min(end);
        out.push(make_chunk(lines, cursor, last));
        if last >= end {
            break;
        }
        // Step back far enough to cover the overlap budget, never past the
        // window start.
        let mut back = last;
        let mut overlap = 0usize;
        while back > cursor + 1 {
            let len = lines[back].chars().count() + 1;
            if overlap + len > CHUNK_OVERLAP_CHARS {
                break;
            }
            overlap += len;
            back -= 1;
        }
        cursor = back.max(cursor + 1);
    }
    out
}

// =============================================================================
// Walk
// =============================================================================

/// One indexable file of a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFile {
    pub path: String,
    pub blob_oid: String,
}

/// True when a path is inside a directory the index never enters. Only
/// DIRECTORY components are matched, so a source file literally called
/// `dist.rs` is still indexed.
pub fn is_excluded_path(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts
        .iter()
        .any(|segment| EXCLUDED_DIR_SEGMENTS.contains(segment))
}

/// True for content the index refuses: too large, or not text. The NUL probe is
/// what git itself uses to call a blob binary, and the UTF-8 check keeps
/// non-UTF-8 encodings out of an embedding that would only see replacement
/// characters.
pub fn is_indexable_content(bytes: &[u8]) -> bool {
    if bytes.is_empty() || bytes.len() > MAX_FILE_BYTES {
        return false;
    }
    let probe = &bytes[..bytes.len().min(BINARY_PROBE_BYTES)];
    if probe.contains(&0) {
        return false;
    }
    std::str::from_utf8(bytes).is_ok()
}

/// Files of a commit that are candidates for indexing: regular blobs outside
/// the excluded directories. Symlinks (mode 120000) and submodules (160000)
/// are dropped — neither has content of its own in this repository.
fn walk_commit(broker: &Broker, handle: &RepoHandle, commit: &str) -> Result<Vec<RepoFile>> {
    let mut out: Vec<RepoFile> = broker
        .list_tree(handle, commit)?
        .into_iter()
        .filter(|entry| entry.mode.starts_with("100"))
        .filter(|entry| !is_excluded_path(&entry.path))
        .map(|entry| RepoFile {
            path: entry.path,
            blob_oid: entry.oid,
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

// =============================================================================
// Progress
// =============================================================================

/// One progress frame of an indexing job. `seq` is monotonic per workspace, so
/// a stream resumes from a cursor instead of replaying or skipping.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexProgress {
    pub seq: u64,
    pub job_id: String,
    pub workspace_id: String,
    pub branch: String,
    /// `queued` | `walk` | `index` | `done` | `partial` | `failed` | `cancelled`
    pub phase: String,
    pub files_done: u32,
    pub files_total: u32,
    pub chunks: u32,
    pub message: String,
    /// Last frame of this job. The stream may close after it.
    pub terminal: bool,
}

struct ProgressChannel {
    tx: broadcast::Sender<IndexProgress>,
    seq: AtomicU64,
    history: Mutex<std::collections::VecDeque<IndexProgress>>,
}

fn channels() -> &'static DashMap<String, Arc<ProgressChannel>> {
    static MAP: OnceLock<DashMap<String, Arc<ProgressChannel>>> = OnceLock::new();
    MAP.get_or_init(DashMap::new)
}

fn channel_for(workspace_id: &str) -> Arc<ProgressChannel> {
    if let Some(existing) = channels().get(workspace_id) {
        return existing.clone();
    }
    channels()
        .entry(workspace_id.to_string())
        .or_insert_with(|| {
            Arc::new(ProgressChannel {
                tx: broadcast::channel(256).0,
                seq: AtomicU64::new(0),
                history: Mutex::new(std::collections::VecDeque::new()),
            })
        })
        .clone()
}

/// Live progress of a workspace. The channel exists whether or not a job is
/// running, so a subscriber never has to race the job's start.
pub fn subscribe_progress(workspace_id: &str) -> broadcast::Receiver<IndexProgress> {
    channel_for(workspace_id).tx.subscribe()
}

/// Frames after `after_seq`, newest last. Bounded by `PROGRESS_HISTORY`: a
/// client whose cursor fell off the end simply receives what is still known.
pub fn progress_since(workspace_id: &str, after_seq: u64) -> Vec<IndexProgress> {
    let Some(channel) = channels().get(workspace_id) else {
        return Vec::new();
    };
    // Bound to a local: as a tail expression the map guard would be dropped
    // before the temporary it lends the lock to, which does not compile.
    let frames = channel
        .history
        .lock()
        .iter()
        .filter(|frame| frame.seq > after_seq)
        .cloned()
        .collect();
    frames
}

#[allow(clippy::too_many_arguments)]
fn emit(
    workspace_id: &str,
    job_id: &str,
    branch: &str,
    phase: &str,
    files_done: u32,
    files_total: u32,
    chunks: u32,
    message: &str,
    terminal: bool,
) {
    let channel = channel_for(workspace_id);
    let frame = IndexProgress {
        seq: channel.seq.fetch_add(1, Ordering::SeqCst) + 1,
        job_id: job_id.to_string(),
        workspace_id: workspace_id.to_string(),
        branch: branch.to_string(),
        phase: phase.to_string(),
        files_done,
        files_total,
        chunks,
        message: message.to_string(),
        terminal,
    };
    {
        let mut history = channel.history.lock();
        history.push_back(frame.clone());
        while history.len() > PROGRESS_HISTORY {
            history.pop_front();
        }
    }
    let _ = channel.tx.send(frame);
}

// =============================================================================
// Queue and cancellation
// =============================================================================

fn queues() -> &'static DashMap<String, Arc<tokio::sync::Mutex<()>>> {
    static MAP: OnceLock<DashMap<String, Arc<tokio::sync::Mutex<()>>>> = OnceLock::new();
    MAP.get_or_init(DashMap::new)
}

fn queue_for(workspace_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    queues()
        .entry(workspace_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

fn cancels() -> &'static DashMap<String, Arc<AtomicBool>> {
    static MAP: OnceLock<DashMap<String, Arc<AtomicBool>>> = OnceLock::new();
    MAP.get_or_init(DashMap::new)
}

/// Asks a running job to stop at the next file boundary. Returns false when no
/// such job is registered — a finished job is not an error.
pub fn cancel_job(job_id: &str) -> bool {
    match cancels().get(job_id) {
        Some(flag) => {
            flag.store(true, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

/// Debounce generations per (workspace, branch). A newer request supersedes an
/// older sleeping one instead of queueing behind it.
fn debounce() -> &'static DashMap<String, u64> {
    static MAP: OnceLock<DashMap<String, u64>> = OnceLock::new();
    MAP.get_or_init(DashMap::new)
}

// =============================================================================
// Runtime state (workspace.db)
// =============================================================================

/// `index_state` of one branch, joined with the branch's live head so the
/// caller can see the divergence rather than infer it.
#[derive(Debug, Clone, PartialEq)]
pub struct BranchIndexState {
    pub branch: String,
    pub indexed_commit: Option<String>,
    pub head_commit: Option<String>,
    pub files: u32,
    pub chunks: u32,
    pub updated_at: Option<String>,
    pub last_error: Option<String>,
    /// The index does not describe the current head. Not an error — search
    /// still answers and reports itself degraded.
    pub stale: bool,
}

fn read_state(pool: &DbPool, branch: &str) -> Result<Option<(Option<String>, Option<String>)>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace.db read: {e}"))?;
    conn.query_row(
        "SELECT indexed_commit, last_error FROM index_state WHERE branch = ?1",
        params![branch],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|e| anyhow!("index_state read: {e}"))
}

fn write_state(
    pool: &DbPool,
    branch: &str,
    indexed_commit: Option<&str>,
    files: u32,
    chunks: u32,
    last_error: Option<&str>,
) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace.db write: {e}"))?;
    conn.execute(
        "INSERT INTO index_state (branch, indexed_commit, files, chunks, updated_at, last_error) \
         VALUES (?1, ?2, ?3, ?4, datetime('now'), ?5) \
         ON CONFLICT(branch) DO UPDATE SET indexed_commit = excluded.indexed_commit, \
            files = excluded.files, chunks = excluded.chunks, \
            updated_at = excluded.updated_at, last_error = excluded.last_error",
        params![branch, indexed_commit, files, chunks, last_error],
    )
    .map_err(|e| anyhow!("index_state write: {e}"))?;
    Ok(())
}

fn file_ledger(pool: &DbPool, branch: &str) -> Result<HashMap<String, (String, u32)>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace.db read: {e}"))?;
    let mut stmt =
        conn.prepare("SELECT path, blob_oid, chunks FROM index_files WHERE branch = ?1")?;
    let rows = stmt.query_map(params![branch], |row| {
        Ok((
            row.get::<_, String>(0)?,
            (row.get::<_, String>(1)?, row.get::<_, i64>(2)? as u32),
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (path, entry) = row?;
        out.insert(path, entry);
    }
    Ok(out)
}

fn upsert_file(pool: &DbPool, branch: &str, path: &str, oid: &str, chunks: u32) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace.db write: {e}"))?;
    conn.execute(
        "INSERT INTO index_files (branch, path, blob_oid, chunks, indexed_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(branch, path) DO UPDATE SET blob_oid = excluded.blob_oid, \
            chunks = excluded.chunks, indexed_at = excluded.indexed_at",
        params![branch, path, oid, chunks as i64],
    )?;
    Ok(())
}

fn forget_file(pool: &DbPool, branch: &str, path: &str) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace.db write: {e}"))?;
    conn.execute(
        "DELETE FROM index_files WHERE branch = ?1 AND path = ?2",
        params![branch, path],
    )?;
    Ok(())
}

fn branch_totals(pool: &DbPool, branch: &str) -> Result<(u32, u32)> {
    let conn = pool.read().map_err(|e| anyhow!("workspace.db read: {e}"))?;
    let row: (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(chunks), 0) FROM index_files WHERE branch = ?1",
        params![branch],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((row.0 as u32, row.1 as u32))
}

/// Deterministic ref id from (branch, path, chunk index) — FNV-1a 64-bit, the
/// same construction Project Studio uses, so re-indexing a file REPLACES its
/// vectors instead of duplicating them. ref_id 0 is reserved by zvec.
fn ref_id_for(branch: &str, path: &str, chunk_index: u32) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in branch.as_bytes().iter().chain(b"\0").chain(path.as_bytes()) {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash ^= (chunk_index as u64).wrapping_add(1);
    hash = hash.wrapping_mul(0x100000001b3);
    if hash == 0 {
        1
    } else {
        hash
    }
}

/// Metadata schema of the `code` namespace — §14's chunk metadata, plus the
/// body so a hit can be rendered without going back to git.
pub fn chunk_field_specs() -> Vec<FieldSpec> {
    vec![
        FieldSpec {
            name: "path".to_string(),
            field_type: FieldType::Str,
            indexed: true,
        },
        FieldSpec {
            name: "lang".to_string(),
            field_type: FieldType::Str,
            indexed: true,
        },
        FieldSpec {
            name: "start_line".to_string(),
            field_type: FieldType::Int,
            indexed: false,
        },
        FieldSpec {
            name: "end_line".to_string(),
            field_type: FieldType::Int,
            indexed: false,
        },
        FieldSpec {
            name: "commit".to_string(),
            field_type: FieldType::Str,
            indexed: true,
        },
        FieldSpec {
            name: "branch".to_string(),
            field_type: FieldType::Str,
            indexed: true,
        },
        FieldSpec {
            name: "text".to_string(),
            field_type: FieldType::Str,
            indexed: false,
        },
    ]
}

// =============================================================================
// Reports and hits
// =============================================================================

/// What one pass did. `complete = false` means the branch state was NOT moved
/// to the new head — the caller is looking at a partial index that says so.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IndexRunReport {
    pub job_id: String,
    pub branch: String,
    pub head_commit: String,
    pub files_indexed: u32,
    pub files_skipped: u32,
    pub files_removed: u32,
    pub chunks_written: u32,
    pub complete: bool,
    /// `time_budget_exceeded` | `cancelled` — set only when `complete` is false.
    pub stopped_reason: Option<String>,
}

/// One semantic hit, shaped for `CodeSearchHit` on the wire.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeHit {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    pub snippet: String,
    pub lang: String,
    pub commit: String,
    pub branch: String,
}

/// Result of a semantic search plus the honesty flag the wire needs: `degraded`
/// says the answer came from an index that does not describe the current head
/// (or from no index at all), which is when the caller falls back to grep.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CodeSearchOutcome {
    pub hits: Vec<CodeHit>,
    pub degraded: bool,
    pub reason: Option<String>,
}

// =============================================================================
// The index
// =============================================================================

/// Semantic index of ONE workspace. Holds its dependencies explicitly so
/// production (namespace manager singleton, router embedder, path-derived
/// broker) and tests (temporary root, counting embedder) differ only in how the
/// value is built.
pub struct CodeIndex {
    core_db: DbPool,
    runtime: DbPool,
    org_id: String,
    workspace_id: String,
    root: std::path::PathBuf,
    namespaces: Arc<NamespaceManager>,
    embedder: Arc<dyn CodeEmbedder>,
    budget: Duration,
}

impl CodeIndex {
    /// Index of a provisioned workspace on its owner node.
    pub fn for_workspace(
        core_db: &DbPool,
        workspace: &WorkspaceRecord,
        router: Arc<Router>,
    ) -> Result<Self> {
        Ok(Self {
            core_db: core_db.clone(),
            runtime: workspace_db::open(&workspace.id)?,
            org_id: workspace.org_id.clone(),
            workspace_id: workspace.id.clone(),
            root: paths::workspace_dir(&workspace.id)?,
            namespaces: crate::services::vector_namespace_manager(core_db).clone(),
            embedder: Arc::new(RouterEmbedder::new(router)),
            budget: DEFAULT_TIME_BUDGET,
        })
    }

    /// Index over an explicit workspace root and explicit dependencies.
    #[allow(clippy::too_many_arguments)]
    pub fn with_parts(
        core_db: DbPool,
        runtime: DbPool,
        org_id: impl Into<String>,
        workspace_id: impl Into<String>,
        root: impl Into<std::path::PathBuf>,
        namespaces: Arc<NamespaceManager>,
        embedder: Arc<dyn CodeEmbedder>,
    ) -> Self {
        Self {
            core_db,
            runtime,
            org_id: org_id.into(),
            workspace_id: workspace_id.into(),
            root: root.into(),
            namespaces,
            embedder,
            budget: DEFAULT_TIME_BUDGET,
        }
    }

    /// Git access of this workspace. A broker is a workspace root plus the
    /// hardened invocation rules, so building one per call costs a path clone
    /// and keeps the blocking git work movable onto a blocking thread.
    fn broker(&self) -> Broker {
        Broker::at(self.root.clone())
    }

    fn vectors_dir(&self) -> std::path::PathBuf {
        self.root.join("vectors")
    }

    /// Ceiling on one pass. Exceeding it ends the pass as INCOMPLETE, never as
    /// a quiet success.
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    fn scope(&self) -> String {
        vector_scope(&self.workspace_id)
    }

    // ----- state ------------------------------------------------------------

    /// Index state of every branch that was ever indexed, with the branch's
    /// current head so divergence is visible instead of implied.
    pub fn status(&self) -> Result<Vec<BranchIndexState>> {
        let rows: Vec<(
            String,
            Option<String>,
            i64,
            i64,
            Option<String>,
            Option<String>,
        )> = {
            let conn = self
                .runtime
                .read()
                .map_err(|e| anyhow!("workspace.db read: {e}"))?;
            let mut stmt = conn.prepare(
                "SELECT branch, indexed_commit, files, chunks, updated_at, last_error \
                 FROM index_state ORDER BY branch",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let broker = self.broker();
        let handle = broker.reference();
        let mut out = Vec::with_capacity(rows.len());
        for (branch, indexed_commit, files, chunks, updated_at, last_error) in rows {
            let head = broker.rev_parse(&handle, &branch).ok().flatten();
            let stale = match (&indexed_commit, &head) {
                (Some(indexed), Some(head)) => indexed != head,
                _ => true,
            };
            out.push(BranchIndexState {
                branch,
                indexed_commit,
                head_commit: head,
                files: files as u32,
                chunks: chunks as u32,
                updated_at,
                last_error,
                stale,
            });
        }
        Ok(out)
    }

    // ----- runs -------------------------------------------------------------

    /// Full pass over `branch`: every file of the head commit is considered.
    /// Content already embedded under the same blob id is not embedded again,
    /// so a rebuild after a partial pass resumes rather than restarts.
    pub async fn rebuild(&self, branch: &str) -> Result<IndexRunReport> {
        let job_id = uuid::Uuid::new_v4().to_string();
        self.run_queued(branch, RunMode::Full, &job_id).await
    }

    /// Incremental pass: only what changed between `index_state.indexed_commit`
    /// and the branch head. Falls back to a full pass when there is no usable
    /// starting point (first run, or a base commit the repository no longer
    /// has after a reset).
    pub async fn refresh(&self, branch: &str) -> Result<IndexRunReport> {
        let job_id = uuid::Uuid::new_v4().to_string();
        self.run_queued(branch, RunMode::Incremental, &job_id).await
    }

    async fn run_queued(
        &self,
        branch: &str,
        mode: RunMode,
        job_id: &str,
    ) -> Result<IndexRunReport> {
        let queue = queue_for(&self.workspace_id);
        emit(
            &self.workspace_id,
            job_id,
            branch,
            "queued",
            0,
            0,
            0,
            "",
            false,
        );
        let _lock = queue.lock().await;
        let cancel = Arc::new(AtomicBool::new(false));
        cancels().insert(job_id.to_string(), cancel.clone());
        let result = self.run(branch, mode, job_id, &cancel).await;
        cancels().remove(job_id);
        if let Err(e) = &result {
            let message = e.to_string();
            let _ = self.record_failure(branch, &message);
            emit(
                &self.workspace_id,
                job_id,
                branch,
                "failed",
                0,
                0,
                0,
                &message,
                true,
            );
        }
        result
    }

    fn record_failure(&self, branch: &str, message: &str) -> Result<()> {
        let (files, chunks) = branch_totals(&self.runtime, branch).unwrap_or((0, 0));
        let existing = read_state(&self.runtime, branch)?.and_then(|(commit, _)| commit);
        write_state(
            &self.runtime,
            branch,
            existing.as_deref(),
            files,
            chunks,
            Some(message),
        )
    }

    async fn run(
        &self,
        branch: &str,
        mode: RunMode,
        job_id: &str,
        cancel: &Arc<AtomicBool>,
    ) -> Result<IndexRunReport> {
        let workspace = repository::get_workspace(&self.core_db, &self.workspace_id)?
            .ok_or_else(|| anyhow!("workspace not found"))?;
        if !workspace.index_enabled {
            return Err(anyhow!("the semantic index is disabled for this workspace"));
        }
        let broker = self.broker();
        let handle = broker.reference();
        let head = broker
            .rev_parse(&handle, branch)?
            .ok_or_else(|| anyhow!("branch '{branch}' has no commit"))?;

        let previous = read_state(&self.runtime, branch)?;
        // An incomplete previous pass invalidates the diff shortcut: files that
        // did not change since the base commit may still be missing from the
        // index, and a diff would never name them. The full walk is cheap here
        // anyway — the per-file ledger skips whatever was already embedded.
        let incomplete = previous
            .as_ref()
            .map(|(_, error)| error.is_some())
            .unwrap_or(false);
        let base = match mode {
            RunMode::Full => None,
            RunMode::Incremental if incomplete => None,
            RunMode::Incremental => previous
                .as_ref()
                .and_then(|(commit, _)| commit.clone())
                .filter(|commit| broker.object_exists(&handle, commit).unwrap_or(false)),
        };
        if mode == RunMode::Incremental
            && base.as_deref() == Some(head.as_str())
            && previous
                .as_ref()
                .map(|(_, error)| error.is_none())
                .unwrap_or(false)
        {
            let (files, chunks) = branch_totals(&self.runtime, branch)?;
            emit(
                &self.workspace_id,
                job_id,
                branch,
                "done",
                files,
                files,
                chunks,
                "already up to date",
                true,
            );
            return Ok(IndexRunReport {
                job_id: job_id.to_string(),
                branch: branch.to_string(),
                head_commit: head,
                complete: true,
                ..Default::default()
            });
        }
        emit(
            &self.workspace_id,
            job_id,
            branch,
            "walk",
            0,
            0,
            0,
            "",
            false,
        );
        let files = walk_commit(&broker, &handle, &head)?;
        let ledger = file_ledger(&self.runtime, branch)?;
        let (work, removed_paths) = match &base {
            Some(base) => self.incremental_work(&broker, &handle, base, &head, &files)?,
            None => {
                // A walk-based pass owns the branch's ledger: a path the ledger
                // still knows but the commit no longer has must leave the index.
                let present: std::collections::HashSet<&str> =
                    files.iter().map(|file| file.path.as_str()).collect();
                let removed = ledger
                    .keys()
                    .filter(|path| !present.contains(path.as_str()))
                    .cloned()
                    .collect();
                (files.clone(), removed)
            }
        };

        let mut report = IndexRunReport {
            job_id: job_id.to_string(),
            branch: branch.to_string(),
            head_commit: head.clone(),
            complete: true,
            ..Default::default()
        };

        for path in &removed_paths {
            if let Some((_, chunks)) = ledger.get(path) {
                self.delete_chunks(branch, path, *chunks)?;
            }
            forget_file(&self.runtime, branch, path)?;
            report.files_removed += 1;
        }

        let started = Instant::now();
        let total = work.len() as u32;
        for (position, file) in work.iter().enumerate() {
            if cancel.load(Ordering::SeqCst) {
                report.complete = false;
                report.stopped_reason = Some("cancelled".to_string());
                break;
            }
            if started.elapsed() >= self.budget {
                report.complete = false;
                report.stopped_reason = Some("time_budget_exceeded".to_string());
                break;
            }
            if ledger
                .get(&file.path)
                .is_some_and(|(oid, _)| oid == &file.blob_oid)
            {
                report.files_skipped += 1;
                continue;
            }
            match self
                .index_one(branch, &head, file, ledger.get(&file.path))
                .await?
            {
                Some(chunks) => {
                    report.files_indexed += 1;
                    report.chunks_written += chunks;
                }
                None => report.files_skipped += 1,
            }
            emit(
                &self.workspace_id,
                job_id,
                branch,
                "index",
                position as u32 + 1,
                total,
                report.chunks_written,
                &file.path,
                false,
            );
        }

        let (files_total, chunks_total) = branch_totals(&self.runtime, branch)?;
        // Only a COMPLETE pass may claim the new head. A partial one leaves the
        // previous commit in place and records why, so the state reads as
        // "behind and incomplete" rather than as a fresh index.
        let (indexed_commit, last_error) = if report.complete {
            (Some(head.clone()), None)
        } else {
            let reason = report.stopped_reason.clone().unwrap_or_default();
            (
                previous.as_ref().and_then(|(commit, _)| commit.clone()),
                Some(format!(
                    "{reason}: indexed {} of {total} file(s) at {head}",
                    report.files_indexed
                )),
            )
        };
        write_state(
            &self.runtime,
            branch,
            indexed_commit.as_deref(),
            files_total,
            chunks_total,
            last_error.as_deref(),
        )?;
        emit(
            &self.workspace_id,
            job_id,
            branch,
            if report.complete { "done" } else { "partial" },
            report.files_indexed,
            total,
            chunks_total,
            last_error.as_deref().unwrap_or_default(),
            true,
        );
        Ok(report)
    }

    /// Changed and deleted paths between two commits. Rename detection stays
    /// off (as in the broker): a rename is a delete plus an add, and both sides
    /// have to be re-embedded anyway because the path is part of the chunk.
    fn incremental_work(
        &self,
        broker: &Broker,
        handle: &RepoHandle,
        base: &str,
        head: &str,
        head_files: &[RepoFile],
    ) -> Result<(Vec<RepoFile>, Vec<String>)> {
        let by_path: HashMap<&str, &RepoFile> = head_files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        let mut work = Vec::new();
        let mut removed = Vec::new();
        for entry in broker.diff_name_status(handle, base, head)? {
            match by_path.get(entry.path.as_str()) {
                Some(file) => work.push((*file).clone()),
                // Deleted, or moved into a directory the index excludes — both
                // mean "no longer in the index".
                None => removed.push(entry.path),
            }
        }
        work.sort_by(|a, b| a.path.cmp(&b.path));
        work.dedup_by(|a, b| a.path == b.path);
        Ok((work, removed))
    }

    /// Embeds and stores one file. `Ok(None)` means the content was refused
    /// (too large, binary, or empty after chunking) — a skip, not a failure.
    async fn index_one(
        &self,
        branch: &str,
        commit: &str,
        file: &RepoFile,
        previous: Option<&(String, u32)>,
    ) -> Result<Option<u32>> {
        let root = self.root.clone();
        let oid = file.blob_oid.clone();
        let bytes = tokio::task::spawn_blocking(move || {
            let broker = Broker::at(root);
            let handle = broker.reference();
            broker.cat_file(&handle, &oid)
        })
        .await
        .map_err(|e| anyhow!("blob read task: {e}"))??;
        if !is_indexable_content(&bytes) {
            return Ok(None);
        }
        let content = String::from_utf8(bytes).map_err(|_| anyhow!("blob is not UTF-8"))?;
        let lang = lang_of(&file.path);
        let chunks = chunk_file(lang, &content);
        if chunks.is_empty() {
            return Ok(None);
        }

        let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());
        for batch in chunks.chunks(EMBED_BATCH) {
            let texts: Vec<String> = batch
                .iter()
                .map(|chunk| chunk.embed_text(&file.path))
                .collect();
            let embedded = self.embedder.embed(texts).await?;
            if embedded.len() != batch.len() {
                return Err(anyhow!(
                    "embedder returned {} vectors for {} chunks",
                    embedded.len(),
                    batch.len()
                ));
            }
            vectors.extend(embedded);
        }
        let dim = vectors
            .first()
            .map(|v| v.len() as u32)
            .ok_or_else(|| anyhow!("no vectors to store"))?;

        // Old chunks first: a file that shrank must not leave orphans behind,
        // and the recorded count makes the removal exact.
        if let Some((_, previous_chunks)) = previous {
            self.delete_chunks(branch, &file.path, *previous_chunks)?;
        }
        self.store_chunks(branch, commit, &file.path, lang, &chunks, &vectors, dim)?;
        upsert_file(
            &self.runtime,
            branch,
            &file.path,
            &file.blob_oid,
            chunks.len() as u32,
        )?;
        Ok(Some(chunks.len() as u32))
    }

    #[allow(clippy::too_many_arguments)]
    fn store_chunks(
        &self,
        branch: &str,
        commit: &str,
        path: &str,
        lang: &str,
        chunks: &[PlannedChunk],
        vectors: &[Vec<f32>],
        dim: u32,
    ) -> Result<()> {
        let specs = chunk_field_specs();
        let scope = self.scope();
        // Create the namespace AT the workspace directory before the quota path
        // (`upsert_batch_with_quota` → `get_or_create`) would place it in the
        // shared addon tree.
        self.namespaces
            .get_or_create_at(
                &self.org_id,
                &scope,
                VECTOR_NAMESPACE,
                dim,
                Metric::Cosine,
                &specs,
                false,
                &self.vectors_dir(),
            )
            .map_err(|e| anyhow!("code index namespace: {e}"))?;

        let fields: Vec<Vec<Field>> = chunks
            .iter()
            .map(|chunk| {
                vec![
                    Field {
                        name: "path".to_string(),
                        value: FieldValue::Str(path.to_string()),
                    },
                    Field {
                        name: "lang".to_string(),
                        value: FieldValue::Str(lang.to_string()),
                    },
                    Field {
                        name: "start_line".to_string(),
                        value: FieldValue::Int(chunk.start_line as i64),
                    },
                    Field {
                        name: "end_line".to_string(),
                        value: FieldValue::Int(chunk.end_line as i64),
                    },
                    Field {
                        name: "commit".to_string(),
                        value: FieldValue::Str(commit.to_string()),
                    },
                    Field {
                        name: "branch".to_string(),
                        value: FieldValue::Str(branch.to_string()),
                    },
                    Field {
                        name: "text".to_string(),
                        value: FieldValue::Str(chunk.body.clone()),
                    },
                ]
            })
            .collect();
        let items: Vec<UpsertItem<'_>> = chunks
            .iter()
            .enumerate()
            .zip(vectors.iter())
            .zip(fields.iter())
            .map(|(((index, _), vector), fields)| UpsertItem {
                ref_id: ref_id_for(branch, path, index as u32),
                vector,
                fields: fields.as_slice(),
                sparse: None,
            })
            .collect();

        if let Err(e) = self.namespaces.upsert_batch_with_quota(
            &self.org_id,
            &scope,
            VECTOR_NAMESPACE,
            dim,
            Metric::Cosine,
            &specs,
            false,
            &items,
        ) {
            // No partial file in the index: roll back every ref of this file so
            // the ledger and the store cannot disagree.
            if let Ok(backend) = self.namespaces.get(&self.org_id, &scope, VECTOR_NAMESPACE) {
                for item in &items {
                    let _ = backend.delete(item.ref_id);
                }
            }
            return Err(anyhow!("code index upsert: {e}"));
        }
        Ok(())
    }

    /// Removes the vectors of one file. Exact, because the ref ids are derived
    /// from (branch, path, index) and the count is recorded — no filtered
    /// search whose recall would decide how much of a file survives.
    fn delete_chunks(&self, branch: &str, path: &str, chunks: u32) -> Result<()> {
        if chunks == 0 {
            return Ok(());
        }
        let backend = match self
            .namespaces
            .get(&self.org_id, &self.scope(), VECTOR_NAMESPACE)
        {
            Ok(backend) => backend,
            Err(VectorError::NamespaceNotFound { .. }) => return Ok(()),
            Err(e) => return Err(anyhow!("code index namespace open: {e}")),
        };
        for index in 0..chunks {
            let _ = backend.delete(ref_id_for(branch, path, index));
        }
        Ok(())
    }

    // ----- search -----------------------------------------------------------

    /// Semantic search over the workspace index. `prefix` narrows to a path
    /// prefix (empty = whole repository) and is applied here because the
    /// backend filter AST has no prefix operator.
    ///
    /// The outcome carries `degraded`: hits from a branch whose index is behind
    /// its head, or an index that does not exist yet, are still returned but
    /// must not be presented as authoritative — grep is (§14).
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
        prefix: &str,
    ) -> Result<CodeSearchOutcome> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(CodeSearchOutcome::default());
        }
        let backend = match self
            .namespaces
            .get(&self.org_id, &self.scope(), VECTOR_NAMESPACE)
        {
            Ok(backend) => backend,
            Err(VectorError::NamespaceNotFound { .. }) => {
                return Ok(CodeSearchOutcome {
                    hits: Vec::new(),
                    degraded: true,
                    reason: Some("index_missing".to_string()),
                })
            }
            Err(e) => return Err(anyhow!("code index namespace open: {e}")),
        };
        let mut embedded = self.embedder.embed(vec![query.to_string()]).await?;
        let vector = embedded
            .pop()
            .ok_or_else(|| anyhow!("embedder returned no vector for the query"))?;
        if vector.len() as u32 != backend.dim() {
            return Err(anyhow!(
                "query embedding has dim {} but the index has {}",
                vector.len(),
                backend.dim()
            ));
        }
        let k = limit.saturating_mul(SEARCH_OVERSAMPLE).min(10_000);
        let filter: Option<Filter> = None;
        let outputs: Vec<String> = chunk_field_specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        let raw = backend
            .search(&vector, k, filter.as_ref(), &outputs)
            .map_err(|e| anyhow!("code index search: {e}"))?;

        let mut hits = Vec::new();
        for hit in raw {
            let get_str = |name: &str| -> String {
                hit.fields
                    .iter()
                    .find(|f| f.name == name)
                    .and_then(|f| match &f.value {
                        FieldValue::Str(v) => Some(v.clone()),
                        _ => None,
                    })
                    .unwrap_or_default()
            };
            let get_int = |name: &str| -> u32 {
                hit.fields
                    .iter()
                    .find(|f| f.name == name)
                    .and_then(|f| match &f.value {
                        FieldValue::Int(v) => Some(*v as u32),
                        _ => None,
                    })
                    .unwrap_or_default()
            };
            let path = get_str("path");
            if !prefix.is_empty() && !path.starts_with(prefix) {
                continue;
            }
            hits.push(CodeHit {
                path,
                start_line: get_int("start_line"),
                end_line: get_int("end_line"),
                score: hit.score,
                snippet: get_str("text"),
                lang: get_str("lang"),
                commit: get_str("commit"),
                branch: get_str("branch"),
            });
            if hits.len() >= limit {
                break;
            }
        }

        let states = self.status()?;
        let mut degraded = false;
        let mut reason = None;
        for hit in &hits {
            match states.iter().find(|state| state.branch == hit.branch) {
                Some(state) if state.stale => {
                    degraded = true;
                    reason = Some("index_behind_head".to_string());
                }
                Some(state) if state.last_error.is_some() => {
                    degraded = true;
                    reason = Some("index_incomplete".to_string());
                }
                Some(_) => {}
                None => {
                    degraded = true;
                    reason = Some("index_state_missing".to_string());
                }
            }
        }
        Ok(CodeSearchOutcome {
            hits,
            degraded,
            reason,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunMode {
    Full,
    Incremental,
}

// =============================================================================
// Triggers
// =============================================================================

/// Starts a full rebuild in the background and returns its job id. The job
/// queues behind any pass already running for this workspace.
pub fn start_rebuild(index: Arc<CodeIndex>, branch: &str) -> String {
    let job_id = uuid::Uuid::new_v4().to_string();
    let branch = branch.to_string();
    let spawned = job_id.clone();
    tokio::spawn(async move {
        if let Err(e) = index.run_queued(&branch, RunMode::Full, &spawned).await {
            tracing::warn!(
                workspace_id = %index.workspace_id,
                branch = %branch,
                "code index rebuild failed: {e}"
            );
        }
    });
    job_id
}

/// Debounced incremental refresh — the hook for an accepted patch set, a
/// checkout, a pull and a merge. Repeated calls inside `REFRESH_DEBOUNCE`
/// collapse into one pass, and the pass itself queues per workspace.
pub fn schedule_refresh(index: Arc<CodeIndex>, branch: &str) {
    let key = format!("{}:{branch}", index.workspace_id);
    let generation = {
        let mut entry = debounce().entry(key.clone()).or_insert(0);
        *entry += 1;
        *entry
    };
    let branch = branch.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(REFRESH_DEBOUNCE).await;
        // A later trigger already claimed this window; it will run the pass.
        if debounce().get(&key).map(|g| *g) != Some(generation) {
            return;
        }
        let job_id = uuid::Uuid::new_v4().to_string();
        if let Err(e) = index
            .run_queued(&branch, RunMode::Incremental, &job_id)
            .await
        {
            tracing::warn!(
                workspace_id = %index.workspace_id,
                branch = %branch,
                "code index refresh failed: {e}"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_studio::models::{
        AutonomyMode, EgressEnforcement, ExecMode, NewWorkspace, WorkspaceStatus,
    };
    use std::sync::atomic::AtomicUsize;

    const WORKSPACE_ID: &str = "1f2a1c4b-0e5d-4a77-9c31-8a2b6d4e1f11";
    const ORG: &str = "org-1";
    const DIM: usize = 8;

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Counts calls and texts, and returns a deterministic vector derived from
    /// the text so a search still ranks meaningfully without a model.
    struct CountingEmbedder {
        calls: AtomicUsize,
        texts: AtomicUsize,
        delay: Duration,
        concurrent: AtomicUsize,
        peak: AtomicUsize,
    }

    impl CountingEmbedder {
        fn new() -> Arc<Self> {
            Self::slow(Duration::ZERO)
        }

        fn slow(delay: Duration) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                texts: AtomicUsize::new(0),
                delay,
                concurrent: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
            })
        }
    }

    #[async_trait]
    impl CodeEmbedder for CountingEmbedder {
        async fn embed(&self, texts: Vec<String>) -> Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.texts.fetch_add(texts.len(), Ordering::SeqCst);
            let now = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            self.concurrent.fetch_sub(1, Ordering::SeqCst);
            let out = texts
                .into_iter()
                .map(|text| {
                    let mut v = vec![0.0f32; DIM];
                    for (i, byte) in text.bytes().enumerate() {
                        v[i % DIM] += byte as f32 / 255.0;
                    }
                    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1.0);
                    v.iter_mut().for_each(|x| *x /= norm);
                    v
                })
                .collect();
            Ok(out)
        }
    }

    struct Fixture {
        _data: tempfile::TempDir,
        _registry: tempfile::TempDir,
        _vectors: tempfile::TempDir,
        _guard: std::sync::MutexGuard<'static, ()>,
        core_db: DbPool,
        runtime: DbPool,
        root: std::path::PathBuf,
        namespaces: Arc<NamespaceManager>,
    }

    impl Fixture {
        fn index(&self, embedder: Arc<dyn CodeEmbedder>) -> CodeIndex {
            CodeIndex::with_parts(
                self.core_db.clone(),
                self.runtime.clone(),
                ORG,
                WORKSPACE_ID,
                self.root.clone(),
                self.namespaces.clone(),
                embedder,
            )
        }

        /// Writes files into the reference worktree, stages and commits them
        /// with the system git — the broker's own commit path builds trees from
        /// accepted blobs, which is not what a fixture needs.
        fn commit(&self, files: &[(&str, &str)]) -> String {
            let work = self.root.join("repo");
            for (path, content) in files {
                let full = work.join(path);
                if let Some(parent) = full.parent() {
                    std::fs::create_dir_all(parent).unwrap();
                }
                std::fs::write(&full, content).unwrap();
            }
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "-m", "fixture", "--allow-empty"]);
            self.head()
        }

        fn remove(&self, path: &str) -> String {
            let work = self.root.join("repo");
            std::fs::remove_file(work.join(path)).unwrap();
            git(&work, &["add", "-A"]);
            git(&work, &["commit", "-m", "remove"]);
            self.head()
        }

        fn head(&self) -> String {
            let broker = Broker::at(self.root.clone());
            let handle = broker.reference();
            broker.head_commit(&handle).unwrap()
        }
    }

    /// Raw git for fixture setup. Signing is disabled explicitly: the developer
    /// running the suite may have `commit.gpgsign` on globally, and a fixture
    /// commit must not depend on their key.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn fixture(index_enabled: bool) -> Fixture {
        let guard = paths::test_data_dir_guard();
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let registry = tempfile::tempdir().expect("registry dir");
        let core_db = crate::db::init(&registry.path().join("tentaflow.db")).expect("core db");
        repository::create_workspace(
            &core_db,
            &NewWorkspace {
                id: WORKSPACE_ID.to_string(),
                org_id: ORG.into(),
                owner_user_id: "u-1".into(),
                name: "Workspace".into(),
                slug: "workspace".into(),
                node_id: "node-1".into(),
                exec_mode: ExecMode::TrustedNative,
                container_image: None,
                egress_enforcement: EgressEnforcement::Unrestricted,
                repo_kind: "empty".into(),
                repo_url: None,
                repo_auth_kind: Some("none".into()),
                secret_ref: None,
                ssh_host_fingerprint: None,
                default_branch: Some("main".into()),
                target_branch: None,
                autonomy_ceiling: AutonomyMode::Normal,
                egress_policy: "org_approved".into(),
                index_enabled,
                quota_disk_bytes: None,
                quota_sessions: None,
            },
        )
        .expect("create workspace");
        repository::set_status(&core_db, WORKSPACE_ID, WorkspaceStatus::Active, None).unwrap();

        let root = paths::create_workspace_layout(WORKSPACE_ID).expect("layout");
        let (runtime, _) = workspace_db::open_pool_at(&root).expect("workspace.db");
        Broker::at(root.clone())
            .init_repository("main")
            .expect("init repo");
        let vectors = tempfile::tempdir().expect("vector root");
        let namespaces = Arc::new(NamespaceManager::with_root(
            core_db.clone(),
            vectors.path().to_path_buf(),
        ));
        Fixture {
            _data: data,
            _registry: registry,
            _vectors: vectors,
            _guard: guard,
            core_db,
            runtime,
            root,
            namespaces,
        }
    }

    fn release() {
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    // ----- walk and chunking -------------------------------------------------

    #[test]
    fn the_walk_skips_ignored_files_build_directories_and_oversized_blobs() {
        if !git_available() {
            return;
        }
        let fx = fixture(true);
        let big = "x".repeat(MAX_FILE_BYTES + 1024);
        fx.commit(&[
            (".gitignore", "secret.env\n"),
            ("src/main.rs", "fn main() {}\n"),
            ("node_modules/left-pad/index.js", "module.exports = 1;\n"),
            ("target/debug/build.rs", "fn generated() {}\n"),
            ("dist/bundle.js", "var a = 1;\n"),
            ("huge.txt", &big),
        ]);
        std::fs::write(fx.root.join("repo").join("secret.env"), "TOKEN=1\n").unwrap();

        let broker = Broker::at(fx.root.clone());
        let handle = broker.reference();
        let head = fx.head();
        let files = walk_commit(&broker, &handle, &head).unwrap();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();

        assert!(paths.contains(&"src/main.rs"));
        assert!(
            !paths.iter().any(|p| p.starts_with("node_modules/")),
            "node_modules entered the walk: {paths:?}"
        );
        assert!(!paths.iter().any(|p| p.starts_with("target/")));
        assert!(!paths.iter().any(|p| p.starts_with("dist/")));
        assert!(
            !paths.contains(&"secret.env"),
            "a .gitignore'd file reached the walk: {paths:?}"
        );

        // The size gate is content-level: the blob is in the tree, and the
        // content check is what refuses it.
        assert!(paths.contains(&"huge.txt"));
        let oid = &files
            .iter()
            .find(|f| f.path == "huge.txt")
            .unwrap()
            .blob_oid;
        let bytes = broker.cat_file(&handle, oid).unwrap();
        assert!(
            !is_indexable_content(&bytes),
            "a blob over 2 MiB was accepted"
        );
        assert!(!is_indexable_content(b"binary\0content"));
        release();
    }

    #[test]
    fn a_chunk_reports_the_line_range_it_actually_covers() {
        let content = (1..=40)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = chunk_file("rust", &content);
        assert!(!chunks.is_empty());
        for chunk in &chunks {
            assert!(chunk.start_line >= 1);
            assert!(chunk.end_line >= chunk.start_line);
            let first = chunk.body.lines().next().unwrap();
            assert_eq!(first, format!("line {}", chunk.start_line));
            let last = chunk.body.lines().last().unwrap();
            assert_eq!(last, format!("line {}", chunk.end_line));
        }
        assert_eq!(chunks.first().unwrap().start_line, 1);
        assert_eq!(chunks.last().unwrap().end_line, 40);
    }

    #[test]
    fn a_file_larger_than_the_chunk_budget_is_windowed_with_overlap() {
        let line = "y".repeat(200);
        let content = (0..60).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
        let chunks = chunk_file("text", &content);
        assert!(chunks.len() > 1, "one chunk for {} chars", content.len());
        for pair in chunks.windows(2) {
            assert!(
                pair[1].start_line > pair[0].start_line,
                "windowing did not advance"
            );
            assert!(
                pair[1].start_line <= pair[0].end_line + 1,
                "windows left a gap between {:?} and {:?}",
                pair[0].end_line,
                pair[1].start_line
            );
        }
    }

    #[test]
    fn top_level_definitions_start_a_new_chunk_when_the_budget_allows() {
        let body = "z".repeat(CHUNK_SIZE_CHARS - 30);
        let content = format!("fn first() {{\n{body}\n}}\nfn second() {{\n    ok();\n}}\n");
        let chunks = chunk_file("rust", &content);
        assert_eq!(chunks.len(), 2, "the two functions shared a chunk");
        assert!(chunks[1].body.starts_with("fn second()"));
    }

    // ----- runs --------------------------------------------------------------

    #[tokio::test]
    async fn a_refresh_after_a_patch_set_embeds_only_the_files_that_changed() {
        if !git_available() {
            return;
        }
        let fx = fixture(true);
        fx.commit(&[
            ("a.rs", "fn a() { one(); }\n"),
            ("b.rs", "fn b() { two(); }\n"),
            ("c.rs", "fn c() { three(); }\n"),
        ]);
        let embedder = CountingEmbedder::new();
        let index = fx.index(embedder.clone());

        let first = index.rebuild("main").await.unwrap();
        assert_eq!(first.files_indexed, 3);
        assert!(first.complete);
        let after_full = embedder.calls.load(Ordering::SeqCst);
        assert_eq!(after_full, 3, "one embed call per file was expected");
        assert_eq!(
            embedder.texts.load(Ordering::SeqCst),
            3,
            "one chunk per one-line file was expected"
        );

        fx.commit(&[("b.rs", "fn b() { two(); three(); }\n")]);
        let second = index.refresh("main").await.unwrap();
        assert_eq!(second.files_indexed, 1, "the whole tree was re-indexed");
        assert_eq!(
            embedder.calls.load(Ordering::SeqCst) - after_full,
            1,
            "a refresh embedded more than the changed file"
        );
        assert!(second.complete);

        let state = index.status().unwrap();
        let main = state.iter().find(|s| s.branch == "main").unwrap();
        assert!(!main.stale);
        assert_eq!(main.files, 3);
        release();
    }

    #[tokio::test]
    async fn a_deleted_file_takes_its_chunks_out_of_the_index() {
        if !git_available() {
            return;
        }
        let fx = fixture(true);
        fx.commit(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        let index = fx.index(CountingEmbedder::new());
        index.rebuild("main").await.unwrap();

        fx.remove("b.rs");
        let report = index.refresh("main").await.unwrap();
        assert_eq!(report.files_removed, 1);
        assert_eq!(branch_totals(&fx.runtime, "main").unwrap().0, 1);

        let hits = index.search("fn b", 10, "").await.unwrap();
        assert!(
            hits.hits.iter().all(|hit| hit.path != "b.rs"),
            "a removed file still answers searches"
        );
        release();
    }

    #[tokio::test]
    async fn moving_to_another_branch_shows_divergence_instead_of_freshness() {
        if !git_available() {
            return;
        }
        let fx = fixture(true);
        fx.commit(&[("a.rs", "fn a() {}\n")]);
        let index = fx.index(CountingEmbedder::new());
        index.rebuild("main").await.unwrap();
        assert!(!index.status().unwrap()[0].stale);

        // The branch moves under the index — exactly what a checkout, a pull or
        // an accepted patch set does.
        fx.commit(&[("a.rs", "fn a() { changed(); }\n")]);
        let state = index.status().unwrap();
        let main = state.iter().find(|s| s.branch == "main").unwrap();
        assert!(main.stale, "the index claimed to be current after a commit");
        assert_ne!(main.indexed_commit.as_deref(), main.head_commit.as_deref());

        // A branch that was never indexed has no row at all — it cannot look
        // fresh either.
        let work = fx.root.join("repo");
        git(&work, &["checkout", "-b", "feature"]);
        assert!(index
            .status()
            .unwrap()
            .iter()
            .all(|s| s.branch != "feature"));
        release();
    }

    #[tokio::test]
    async fn two_passes_over_one_workspace_never_overlap() {
        if !git_available() {
            return;
        }
        let fx = fixture(true);
        fx.commit(&[("a.rs", "fn a() {}\n"), ("b.rs", "fn b() {}\n")]);
        // Two branches at the same commit: each pass has real work of its own,
        // so a peak concurrency of one proves serialisation rather than an
        // empty second run.
        git(&fx.root.join("repo"), &["branch", "second"]);
        let embedder = CountingEmbedder::slow(Duration::from_millis(40));
        let index = Arc::new(fx.index(embedder.clone()));

        let one = {
            let index = index.clone();
            tokio::spawn(async move { index.rebuild("main").await })
        };
        let two = {
            let index = index.clone();
            tokio::spawn(async move { index.rebuild("second").await })
        };
        one.await.unwrap().unwrap();
        two.await.unwrap().unwrap();
        assert_eq!(
            embedder.peak.load(Ordering::SeqCst),
            1,
            "two indexing passes ran at the same time"
        );
        release();
    }

    #[tokio::test]
    async fn an_exhausted_time_budget_ends_as_an_incomplete_index_not_a_success() {
        if !git_available() {
            return;
        }
        let fx = fixture(true);
        fx.commit(&[
            ("a.rs", "fn a() {}\n"),
            ("b.rs", "fn b() {}\n"),
            ("c.rs", "fn c() {}\n"),
            ("d.rs", "fn d() {}\n"),
        ]);
        let embedder = CountingEmbedder::slow(Duration::from_millis(50));
        let index = fx
            .index(embedder.clone())
            .with_budget(Duration::from_millis(75));

        let report = index.rebuild("main").await.unwrap();
        assert!(!report.complete, "a truncated pass reported success");
        assert_eq!(
            report.stopped_reason.as_deref(),
            Some("time_budget_exceeded")
        );
        assert!(report.files_indexed >= 1 && report.files_indexed < 4);

        let state = index.status().unwrap();
        let main = state.iter().find(|s| s.branch == "main").unwrap();
        assert!(
            main.indexed_commit.is_none(),
            "a partial pass claimed a commit"
        );
        assert!(main.stale);
        assert!(main
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("time_budget_exceeded"));

        // The next pass resumes: files already embedded are not embedded again.
        let before = embedder.calls.load(Ordering::SeqCst);
        let resumed = index.with_budget(DEFAULT_TIME_BUDGET).rebuild("main").await;
        let resumed = resumed.unwrap();
        assert!(resumed.complete);
        assert_eq!(
            embedder.calls.load(Ordering::SeqCst) - before,
            4 - report.files_indexed as usize,
            "the resumed pass re-embedded work it had already paid for"
        );
        release();
    }

    #[tokio::test]
    async fn indexing_a_workspace_with_the_index_switched_off_is_refused() {
        if !git_available() {
            return;
        }
        let fx = fixture(false);
        fx.commit(&[("a.rs", "fn a() {}\n")]);
        let index = fx.index(CountingEmbedder::new());
        assert!(index.rebuild("main").await.is_err());
        release();
    }

    // ----- search ------------------------------------------------------------

    #[tokio::test]
    async fn a_search_hit_carries_the_path_and_the_line_range() {
        if !git_available() {
            return;
        }
        let fx = fixture(true);
        fx.commit(&[
            ("src/parser.rs", "fn parse_header(input: &str) {}\n"),
            ("src/other.rs", "fn unrelated() {}\n"),
            ("docs/readme.md", "# Parser\n"),
        ]);
        let index = fx.index(CountingEmbedder::new());
        index.rebuild("main").await.unwrap();

        let outcome = index
            .search("fn parse_header(input: &str) {}", 5, "")
            .await
            .unwrap();
        assert!(!outcome.hits.is_empty(), "the index answered nothing");
        assert!(
            !outcome.degraded,
            "a current index reported itself degraded"
        );
        let hit = &outcome.hits[0];
        assert!(!hit.path.is_empty());
        assert!(hit.start_line >= 1);
        assert!(hit.end_line >= hit.start_line);
        assert!(!hit.commit.is_empty());
        assert_eq!(hit.branch, "main");

        let scoped = index.search("# Parser", 5, "docs/").await.unwrap();
        assert!(
            !scoped.hits.is_empty(),
            "the prefix filter dropped everything"
        );
        assert!(
            scoped.hits.iter().all(|hit| hit.path.starts_with("docs/")),
            "the path prefix was ignored: {:?}",
            scoped.hits
        );

        // Once the branch moves, the same search must confess it is behind.
        fx.commit(&[(
            "src/parser.rs",
            "fn parse_header(input: &str) { done(); }\n",
        )]);
        let stale = index.search("fn parse_header", 5, "").await.unwrap();
        assert!(!stale.hits.is_empty());
        assert!(stale.degraded);
        assert_eq!(stale.reason.as_deref(), Some("index_behind_head"));
        release();
    }

    #[tokio::test]
    async fn searching_a_workspace_that_was_never_indexed_degrades_instead_of_failing() {
        if !git_available() {
            return;
        }
        let fx = fixture(true);
        fx.commit(&[("a.rs", "fn a() {}\n")]);
        let index = fx.index(CountingEmbedder::new());
        let outcome = index.search("anything", 5, "").await.unwrap();
        assert!(outcome.hits.is_empty());
        assert!(outcome.degraded);
        assert_eq!(outcome.reason.as_deref(), Some("index_missing"));
        release();
    }

    #[tokio::test]
    async fn progress_frames_are_replayable_from_a_cursor() {
        if !git_available() {
            return;
        }
        let fx = fixture(true);
        fx.commit(&[("a.rs", "fn a() {}\n")]);
        let index = fx.index(CountingEmbedder::new());
        index.rebuild("main").await.unwrap();

        let frames = progress_since(WORKSPACE_ID, 0);
        assert!(frames.len() >= 2, "no progress was published");
        assert!(frames.last().unwrap().terminal);
        let cursor = frames[frames.len() - 2].seq;
        let tail = progress_since(WORKSPACE_ID, cursor);
        assert_eq!(tail.len(), 1);
        assert!(tail[0].terminal);
        release();
    }
}
