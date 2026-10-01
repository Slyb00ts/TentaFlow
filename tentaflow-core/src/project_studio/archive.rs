// ===== File: project_studio/archive.rs — project export / import archives (F4) =====
//
// One project becomes one self-contained zip:
//
//   manifest.json          census + embedding fingerprint + per-file sha256
//   db/project.db          SANITIZED snapshot (see below)
//   db/registry.json       name / description / template / modules — no org, no owner
//   db/user_names.json     historical display names (opt-in, personal data)
//   files/<sha256>         knowledge blobs
//   runs/<run_id>/…        run artifacts (opt-in)
//   vectors/…              the passage index (opt-in, only reused on a match)
//
// Three invariants decide the shape of this module:
//
//  * The database snapshot is written with `VACUUM INTO` AFTER a TRUNCATE
//    checkpoint. Copying `project.db` while a `-wal` exists would archive a
//    file that is missing its own most recent commits.
//  * Secrets never travel. `environments.secret_enc` / `sources.secret_enc` are
//    encrypted with a PER-NODE key, so a copy is useless on the target and
//    dangerous in transit; both are blanked and every environment goes back to
//    'pending' — an import must never resurrect an approved private target.
//  * A hostile archive is assumed. Extraction enforces `enclosed_name`,
//    refuses symlink entries, whitelists the entry prefixes, caps entries and
//    bytes, and verifies every declared sha256.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::db::DbPool;
use crate::deploy::log_bus::{self, BusMessage, LogLine};
use crate::routing::router::Router;

#[derive(Serialize, Deserialize)]
struct DeletionJournal {
    project_id: String,
    org_id: String,
    directory: PathBuf,
    quarantine: PathBuf,
    operation_id: String,
}

fn deletion_journal_dir() -> Result<PathBuf> {
    let root = crate::paths::data_dir().join("projects");
    std::fs::create_dir_all(&root)?;
    super::media::safe_directory(&root, ".deletions")
}

pub fn finish_project_delete(
    core_db: &DbPool,
    project: &super::models::ProjectRecord,
    operation_id: &str,
) -> Result<()> {
    uuid::Uuid::parse_str(operation_id)?;
    let directory = PathBuf::from(&project.dir_path);
    let parent = directory
        .parent()
        .ok_or_else(|| anyhow!("project directory has no parent"))?;
    let quarantine = parent.join(format!(".deleted-{operation_id}"));
    let path = deletion_journal_dir()?.join(format!("{operation_id}.json"));
    let journal = DeletionJournal {
        project_id: project.project_id.clone(),
        org_id: project.org_id.clone(),
        directory: directory.clone(),
        quarantine: quarantine.clone(),
        operation_id: operation_id.into(),
    };
    super::media::write_metadata(&path, &journal)?;
    super::media::cancel_project(core_db, &project.project_id)?;
    for path in [&directory, &quarantine] {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => bail!("project deletion storage is not a regular directory"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if directory.exists() {
        if !std::fs::symlink_metadata(&directory)?.file_type().is_dir() || quarantine.exists() {
            bail!("project deletion directory identity is invalid");
        }
        let (pool, _) = super::project_db::open_pool_at(&directory)?;
        for link in super::ml_link::list(&pool)? {
            super::ml_link::detach(&pool, &project.project_id, &link, true)?;
        }
        drop(pool);
        super::project_db::close(&project.project_id);
        super::media::with_publication_gate(|| {
            std::fs::rename(&directory, &quarantine)?;
            #[cfg(unix)]
            std::fs::File::open(parent)?.sync_all()?;
            Ok(())
        })?;
    } else if !quarantine.exists() {
        let registry = super::db::pool()?;
        let mirrored: i64 = registry.read()?.query_row(
            "SELECT COUNT(*) FROM project_ml_grants WHERE project_id=?1",
            [&project.project_id],
            |row| row.get(0),
        )?;
        if mirrored != 0 {
            bail!("missing project storage prevents grant revocation");
        }
    }
    super::ingest::drop_project_namespaces(core_db, &project.org_id, &project.project_id)?;
    super::ingest::drop_project_graph(core_db, &project.org_id, &project.project_id)?;
    super::repository::delete_project_rows(&project.project_id, operation_id)?;
    super::schedules::delete_hint(&project.project_id);
    cleanup_deleted_project(&journal, &path)
}

fn cleanup_deleted_project(journal: &DeletionJournal, path: &Path) -> Result<()> {
    uuid::Uuid::parse_str(&journal.operation_id)?;
    let parent = journal
        .directory
        .parent()
        .ok_or_else(|| anyhow!("invalid deletion directory"))?;
    if journal.quarantine != parent.join(format!(".deleted-{}", journal.operation_id)) {
        bail!("invalid deletion quarantine path");
    }
    if journal.quarantine.exists() {
        if !std::fs::symlink_metadata(&journal.quarantine)?
            .file_type()
            .is_dir()
        {
            bail!("deletion quarantine is not a directory");
        }
        std::fs::remove_dir_all(&journal.quarantine)?;
    }
    std::fs::remove_file(path)?;
    Ok(())
}

pub fn recover_operations(core_db: &DbPool) -> Result<()> {
    crate::services::ingest_jobs::init(&crate::paths::data_dir().join("jobs.db"))?;
    for pending in super::repository::pending_project_deletions()? {
        finish_project_delete(core_db, &pending.project, &pending.operation_id)?;
    }
    for entry in std::fs::read_dir(deletion_journal_dir()?)? {
        let path = entry?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let journal: DeletionJournal = serde_json::from_reader(super::media::open_regular(&path)?)?;
        if super::repository::get_project(&journal.org_id, &journal.project_id)?.is_some() {
            bail!("project deletion journal is missing its durable admission");
        }
        cleanup_deleted_project(&journal, &path)?;
    }
    for pending in super::repository::pending_project_tree_imports()? {
        for node in pending.nodes {
            let dir = PathBuf::from(&node.dir_path);
            if super::repository::get_project(&pending.org_id, &node.project_id)?.is_some() {
                bail!("prepared import contains a published project");
            }
            if dir.exists() {
                std::fs::remove_dir_all(dir)?;
            }
        }
        let staging = crate::paths::data_dir()
            .join("projects/.imports")
            .join(&pending.operation_id);
        if staging.exists() {
            std::fs::remove_dir_all(staging)?;
        }
        super::repository::abort_project_tree_import(&pending.operation_id)?;
    }
    let staging_root = crate::paths::data_dir().join("projects/.imports");
    if staging_root.exists() {
        for entry in std::fs::read_dir(&staging_root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir()
                || uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()).is_err()
            {
                bail!("import staging contains an invalid operation directory");
            }
            std::fs::remove_dir_all(entry.path())?;
        }
    }
    Ok(())
}

/// Archive layout version. An import refuses anything it does not understand
/// rather than guessing.
pub const ARCHIVE_VERSION: u32 = 3;

const MANIFEST_ENTRY: &str = "manifest.json";
/// Entry prefixes an import will unpack. Anything else is a foreign archive (or
/// an attempt to smuggle a file past the layout) and aborts the import.
const ALLOWED_PREFIXES: [&str; 5] = ["db/", "files/", "runs/", "vectors/", "nodes/"];

/// Import guards. Generous for a real project (thousands of documents plus run
/// artifacts), still bounded against a zip bomb.
const MAX_IMPORT_ENTRIES: usize = 400_000;
const MAX_IMPORT_BYTES: u64 = 32 * 1024 * 1024 * 1024;
/// The manifest is parsed before any other cap is known, so it gets its own.
const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;
const COPY_BUF: usize = 256 * 1024;

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// =============================================================================
// Manifest
// =============================================================================

/// One archived file with its size and content hash. The import verifies each
/// hash after extraction, so a truncated copy fails loudly instead of producing
/// a project with silently corrupt documents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// Project identity as it existed on the exporting node. `owner_user_id` /
/// `org_id` are informational: the import always re-owns to the importer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub parent_id: Option<String>,
    pub path: String,
    pub depth: u32,
    pub is_private: bool,
    pub inherit_modules: bool,
    pub inherit_task_types: bool,
    pub lifecycle: String,
    pub ended_at: Option<String>,
    pub project_id: String,
    #[serde(default)]
    pub key_prefix: String,
    pub name: String,
    pub description: String,
    pub template: String,
    pub modules: Vec<String>,
    pub owner_user_id: String,
    pub org_id: String,
}

/// Fingerprint of the embedding space the archived vectors live in. The vector
/// file is reused ONLY when every field matches on the importing node: the same
/// dimension produced by a DIFFERENT model is the worst possible failure mode
/// for a knowledge base — silently wrong retrieval, with no error anywhere.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingMeta {
    pub alias: String,
    /// Model the alias resolved to at export time.
    pub target_model: String,
    pub dim: u32,
    pub metric: String,
    /// Metadata schema of the namespace (name + type + indexed), serialized.
    pub fields: String,
    pub vector_count: u64,
}

/// Content census, shown before an import so the decision is made with real
/// numbers in hand.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    pub cases: u32,
    pub suites: u32,
    pub runs: u32,
    pub tasks: u32,
    pub documents: u32,
    pub sources: u32,
    pub files: u32,
    pub bytes_files: u64,
    pub bytes_runs: u64,
    pub vectors: u64,
    pub vector_dim: u32,
    pub embedding_alias: String,
    pub embedding_model: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ExportOptions {
    pub include_runs: bool,
    pub include_vectors: bool,
    /// Copies display names into the archive so historical authorship stays
    /// readable on the target node. Personal data — opt-in and audited.
    pub include_user_names: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveManifest {
    pub nodes: Vec<TreeArchiveNode>,
    pub version: u32,
    /// project.db schema version of the snapshot. A NEWER schema than this
    /// binary understands is refused before anything is unpacked.
    pub schema_version: i64,
    pub exported_at: String,
    pub source_node_id: String,
    pub project: ProjectMeta,
    pub options: ExportOptions,
    pub embedding: Option<EmbeddingMeta>,
    pub inventory: Inventory,
    pub files: Vec<FileEntry>,
    #[serde(default)]
    pub functions: Vec<tentaflow_protocol::project_studio::access::ProjectFunctionWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeArchiveNode {
    pub entry: FileEntry,
    pub manifest: Box<ArchiveManifest>,
}

pub fn project_meta(record: &super::models::ProjectRecord) -> Result<ProjectMeta> {
    Ok(ProjectMeta {
        project_id: record.project_id.clone(),
        key_prefix: record.key_prefix.clone(),
        name: record.name.clone(),
        description: record.description.clone(),
        template: record.template.clone(),
        modules: super::repository::effective_project_settings(&record.project_id)?.0,
        owner_user_id: record.owner_user_id.clone(),
        org_id: record.org_id.clone(),
        parent_id: record.parent_id.clone(),
        path: record.path.clone(),
        depth: record.depth,
        is_private: record.is_private,
        inherit_modules: record.inherit_modules,
        inherit_task_types: record.inherit_task_types,
        lifecycle: record.lifecycle.clone(),
        ended_at: record.ended_at.clone(),
    })
}

fn require_export_access(task: &ExportTask) -> Result<()> {
    use tentaflow_protocol::project_studio::ProjectTreeScope;
    let actor = crate::db::repository::get_user_account_by_id(&task.core_db, &task.user_id)?
        .ok_or_else(|| anyhow!("export actor account is unavailable"))?;
    if !actor.is_active {
        bail!("export actor account is disabled");
    }
    let mut current = if task.scope == ProjectTreeScope::Subtree {
        super::repository::list_descendants(&task.org_id, &task.project_id, true)?
    } else {
        vec![
            super::repository::get_project(&task.org_id, &task.project_id)?
                .ok_or_else(|| anyhow!("export project is unavailable"))?,
        ]
    };
    current.sort_by(|a, b| a.project_id.cmp(&b.project_id));
    let mut expected = task
        .nodes
        .iter()
        .map(|node| node.project_id.clone())
        .collect::<Vec<_>>();
    if expected.is_empty() {
        bail!("export scope is empty");
    }
    expected.sort();
    if current
        .iter()
        .map(|node| &node.project_id)
        .collect::<Vec<_>>()
        != expected.iter().collect::<Vec<_>>()
    {
        bail!("export scope changed; prepare the scope again");
    }
    for node in current {
        let access = super::repository::project_access(&node, &task.user_id, false)?;
        if !access.has_access || !access.project_admin {
            bail!("export scope access was revoked");
        }
    }
    Ok(())
}

// =============================================================================
// Job progress
// =============================================================================

/// Live progress of one export/import job. `owner_user_id` is stored so the
/// status handler authorizes the caller — a bare job id must never expose
/// progress to an unrelated user.
#[derive(Debug, Clone, Default)]
pub struct ArchiveJob {
    /// 'running' | 'success' | 'failed'.
    pub status: String,
    pub phase: String,
    pub progress_pct: u32,
    pub error: String,
    pub owner_user_id: String,
    pub project_id: String,
    pub export_ref: String,
    pub archive_bytes: u64,
    pub inventory: Option<Inventory>,
    pub scope_project_ids: Vec<String>,
    pub vectors_imported: bool,
    pub reindex_job_ids: Vec<String>,
}

static JOBS: OnceLock<Mutex<HashMap<String, ArchiveJob>>> = OnceLock::new();

fn jobs() -> &'static Mutex<HashMap<String, ArchiveJob>> {
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn job(job_id: &str) -> Option<ArchiveJob> {
    jobs().lock().ok()?.get(job_id).cloned()
}

fn set_job(job_id: &str, job: ArchiveJob) {
    if let Ok(mut map) = jobs().lock() {
        map.insert(job_id.to_string(), job);
    }
}

fn update_job(job_id: &str, f: impl FnOnce(&mut ArchiveJob)) {
    if let Ok(mut map) = jobs().lock() {
        if let Some(entry) = map.get_mut(job_id) {
            f(entry);
        }
    }
}

// One export per project, one import per staged archive.
static ACTIVE: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn try_claim(key: &str) -> bool {
    match ACTIVE.get_or_init(|| Mutex::new(HashSet::new())).lock() {
        Ok(mut set) => set.insert(key.to_string()),
        Err(_) => false,
    }
}

fn release(key: &str) {
    if let Some(active) = ACTIVE.get() {
        if let Ok(mut set) = active.lock() {
            set.remove(key);
        }
    }
}

/// Emits one progress line on the job's log_bus channel (the ArchiveStream
/// subscription) and mirrors it into the polled job record.
fn emit(
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    job_id: &str,
    phase: &str,
    line: &str,
    pct: u32,
) {
    update_job(job_id, |j| {
        j.phase = phase.to_string();
        j.progress_pct = pct;
    });
    let _ = tx.send(BusMessage::Line(LogLine {
        deploy_id: job_id.to_string(),
        kind: "log".to_string(),
        line: line.to_string(),
        phase: phase.to_string(),
        progress_pct: pct,
        ts_ms: log_bus::now_ms(),
    }));
}

fn finish(
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    job_id: &str,
    status: &str,
    error: &str,
) {
    update_job(job_id, |j| {
        j.status = status.to_string();
        j.error = error.to_string();
        if status == "success" {
            j.progress_pct = 100;
        }
    });
    let _ = tx.send(BusMessage::End {
        deploy_id: job_id.to_string(),
        final_status: status.to_string(),
        image_tag: String::new(),
        container_name: String::new(),
        error_message: error.to_string(),
        duration_ms: 0,
    });
}

// =============================================================================
// Export
// =============================================================================

/// Everything the export task needs, captured before the spawn.
#[derive(Clone)]
pub struct ExportTask {
    pub nodes: Vec<super::models::ProjectRecord>,
    pub scope: tentaflow_protocol::project_studio::ProjectTreeScope,
    pub core_db: DbPool,
    pub org_id: String,
    pub user_id: String,
    pub node_id: String,
    pub project_id: String,
    pub dir_path: PathBuf,
    pub project: ProjectMeta,
    pub options: ExportOptions,
    pub export_ref: String,
    pub dest_zip: PathBuf,
}

/// Starts an export. The log_bus channel is opened BEFORE the spawn so a client
/// that subscribes the moment it gets the job id still finds it.
pub fn spawn_export(task: ExportTask) -> Result<String> {
    let key = format!("psexport:{}", task.project_id);
    if !try_claim(&key) {
        bail!("eksport tego projektu juz trwa");
    }
    let job_id = uuid::Uuid::new_v4().to_string();
    set_job(
        &job_id,
        ArchiveJob {
            status: "running".to_string(),
            phase: "collecting".to_string(),
            owner_user_id: task.user_id.clone(),
            project_id: task.project_id.clone(),
            export_ref: task.export_ref.clone(),
            scope_project_ids: task
                .nodes
                .iter()
                .map(|node| node.project_id.clone())
                .collect(),
            ..ArchiveJob::default()
        },
    );
    let tx = log_bus::sender_for(&job_id);
    let job_task = job_id.clone();
    tokio::spawn(async move {
        let tx_task = tx.clone();
        let jid = job_task.clone();
        // Hashing and copying gigabytes is blocking work — keep it off the async
        // worker; a panic must still release the claim and end the stream.
        let result =
            tokio::task::spawn_blocking(move || build_export(&job_task, &tx_task, &task)).await;
        match result {
            Ok(Ok(summary)) => {
                update_job(&jid, |j| {
                    j.archive_bytes = summary.0;
                    j.inventory = Some(summary.1);
                });
                finish(&tx, &jid, "success", "");
            }
            Ok(Err(e)) => {
                tracing::warn!(job_id = %jid, "project studio export failed: {e:#}");
                finish(&tx, &jid, "failed", &e.to_string());
            }
            Err(e) => {
                tracing::warn!(job_id = %jid, "project studio export panicked: {e}");
                finish(&tx, &jid, "failed", "export task panicked");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        log_bus::close(&jid);
        release(&key);
    });
    Ok(job_id)
}

type ZipOut = zip::ZipWriter<std::io::BufWriter<std::fs::File>>;

fn stored_options() -> zip::write::FileOptions<'static, ()> {
    // Documents and run artifacts are mostly already-compressed payloads:
    // deflating them burns CPU for almost nothing on a multi-GB archive.
    // `large_file` emits zip64 headers so a >4 GiB archive stays readable.
    zip::write::FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(true)
}

/// Streams `src` into the archive through a fixed buffer, hashing as it goes so
/// the file is read exactly once regardless of size.
fn write_file(
    zip: &mut ZipOut,
    arch_path: &str,
    src: &Path,
    allowed: &dyn Fn() -> Result<()>,
) -> Result<FileEntry> {
    zip.start_file(arch_path.to_string(), stored_options())?;
    let mut f =
        super::media::open_regular(src).with_context(|| format!("open {}", src.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; COPY_BUF];
    let mut size = 0u64;
    loop {
        allowed()?;
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        zip.write_all(&buf[..n])?;
        size += n as u64;
    }
    Ok(FileEntry {
        path: arch_path.to_string(),
        size,
        sha256: hex(&hasher.finalize()),
    })
}

fn write_bytes(zip: &mut ZipOut, arch_path: &str, bytes: &[u8]) -> Result<FileEntry> {
    zip.start_file(
        arch_path.to_string(),
        zip::write::FileOptions::<()>::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )?;
    zip.write_all(bytes)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(FileEntry {
        path: arch_path.to_string(),
        size: bytes.len() as u64,
        sha256: hex(&hasher.finalize()),
    })
}

/// Every regular file under `root`, relative path first, sorted for a stable
/// archive. Symlinks are NOT followed: the archive must describe real bytes
/// under the project directory, never wander outside it.
fn walk_sorted(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    fn rec(root: &Path, cur: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(cur)
            .with_context(|| format!("odczyt katalogu {}", cur.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        entries.sort();
        for path in entries {
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                rec(root, &path, out)?;
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/")
                    .to_string();
                out.push((rel, path));
            }
        }
        Ok(())
    }
    if !root.is_dir() {
        return Ok(out);
    }
    rec(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Writes a CONSISTENT, sanitized snapshot of project.db next to the original
/// and returns its path. `VACUUM INTO` is the only copy that is guaranteed to
/// contain every committed transaction; the TRUNCATE checkpoint before it keeps
/// the source's own `-wal` from growing unbounded during a long export.
fn snapshot_database(pool: &DbPool, dest: &Path, options: &ExportOptions) -> Result<()> {
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    {
        let conn = pool
            .write()
            .map_err(|e| anyhow!("project pool write: {e}"))?;
        conn.pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        conn.execute("VACUUM INTO ?1", params![dest.to_string_lossy()])?;
    }

    let copy = rusqlite::Connection::open(dest)?;
    // Secrets are encrypted with a key that exists only on THIS node, and an
    // approval decision belongs to the node that made it.
    copy.execute("UPDATE environments SET secret_enc = ''", [])?;
    copy.execute(
        "UPDATE environments SET approval_status = 'pending', approval_reason = '', \
            decided_by = '', decided_at = NULL",
        [],
    )?;
    copy.execute("UPDATE sources SET secret_enc = ''", [])?;
    // A schedule must not fire on the importing node until a human enables it
    // there: its `next_run_at` is in the past by the time the archive lands, and
    // its environment / runner ids belong to the exporting node. Left enabled,
    // the loop would create REAL runs (manual) or a stream of 'blocked'
    // notifications (auto/perf) the moment the import finishes — the same reason
    // environment approvals are reset above.
    copy.execute(
        "UPDATE schedules SET enabled = 0, auto_disabled = 0, next_run_at = NULL, \
            last_run_id = '', last_status = '', last_reason = '', last_trigger_at = '', \
            consecutive_failures = 0",
        [],
    )?;
    if !options.include_runs {
        copy.execute(
            "DELETE FROM test_run_steps WHERE item_id IN (SELECT item_id FROM test_run_items)",
            [],
        )?;
        copy.execute("DELETE FROM test_run_items", [])?;
        copy.execute("DELETE FROM run_artifacts", [])?;
        copy.execute("DELETE FROM auto_run_meta", [])?;
        copy.execute("DELETE FROM test_runs", [])?;
        copy.execute("DELETE FROM schedule_runs", [])?;
    }
    copy.execute("VACUUM", [])?;
    Ok(())
}

/// Row counts of the archived content.
fn read_inventory(pool: &DbPool, options: &ExportOptions) -> Result<Inventory> {
    let conn = pool.read().map_err(|e| anyhow!("project pool read: {e}"))?;
    let one = |sql: &str| -> Result<u32> {
        let n: i64 = conn.query_row(sql, [], |row| row.get(0))?;
        Ok(n as u32)
    };
    Ok(Inventory {
        cases: one("SELECT COUNT(*) FROM test_cases")?,
        suites: one("SELECT COUNT(*) FROM test_suites")?,
        runs: if options.include_runs {
            one("SELECT COUNT(*) FROM test_runs")?
        } else {
            0
        },
        tasks: one("SELECT COUNT(*) FROM tasks")?,
        documents: one("SELECT COUNT(*) FROM source_files")?,
        sources: one("SELECT COUNT(*) FROM sources")?,
        ..Inventory::default()
    })
}

/// Reads the embedding fingerprint of the project's passage namespace from the
/// CORE vector registry, plus the model its alias currently resolves to.
fn read_embedding_meta(core_db: &DbPool, org_id: &str, project_id: &str) -> Option<EmbeddingMeta> {
    let scope = super::ingest::vector_scope(project_id);
    let conn = core_db.read().ok()?;
    let (dim, metric, count, fields): (i64, String, i64, String) = conn
        .query_row(
            "SELECT dim, metric, count, fields_json FROM addon_vector_namespaces \
             WHERE org_id = ?1 AND addon_id = ?2 AND namespace = ?3",
            params![org_id, scope, super::ingest::VECTOR_NAMESPACE],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .ok()??;
    let target_model: String = conn
        .query_row(
            "SELECT target_model FROM model_aliases WHERE alias = ?1",
            params![super::ingest::EMBEDDINGS_ALIAS],
            |row| row.get(0),
        )
        .optional()
        .ok()?
        .unwrap_or_default();
    Some(EmbeddingMeta {
        alias: super::ingest::EMBEDDINGS_ALIAS.to_string(),
        target_model,
        dim: dim as u32,
        metric,
        fields,
        vector_count: count.max(0) as u64,
    })
}

/// Builds the archive into `<dest>.tmp` and renames it into place only after
/// the central directory is flushed, so an interrupted export never leaves a
/// truncated file that still looks like a valid zip.
fn build_export(
    job_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    task: &ExportTask,
) -> Result<(u64, Inventory)> {
    require_export_access(task)?;
    if task.nodes.len() > 1 {
        return build_tree_export(job_id, tx, task);
    }
    let functions = super::repository::list_functions(&task.project_id)?;
    super::repository::validate_function_catalogue(&functions)?;
    let pool = super::project_db::open(&task.project_id)?;
    emit(tx, job_id, "collecting", "zbieranie zawartosci projektu", 5);

    let mut inventory = read_inventory(&pool, &task.options)?;
    let embedding = if task.options.include_vectors {
        read_embedding_meta(&task.core_db, &task.org_id, &task.project_id)
    } else {
        None
    };
    if let Some(meta) = embedding.as_ref() {
        inventory.vectors = meta.vector_count;
        inventory.vector_dim = meta.dim;
        inventory.embedding_alias = meta.alias.clone();
        inventory.embedding_model = meta.target_model.clone();
    }

    if let Some(parent) = task.dest_zip.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = task.dest_zip.with_extension("zip.tmp");
    let snapshot = tmp.with_extension("db");
    let type_source = super::repository::effective_project_settings(&task.project_id)?.1;
    let types = super::tasks::list_task_types(&super::project_db::open(&type_source)?)?;
    snapshot_database(&pool, &snapshot, &task.options)?;
    let copy = rusqlite::Connection::open(&snapshot)?;
    let transaction = copy.unchecked_transaction()?;
    for kind in types {
        transaction.execute("INSERT INTO task_types(type_id,name,description,sort_order,built_in,active) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(type_id) DO UPDATE SET name=excluded.name,description=excluded.description,sort_order=excluded.sort_order,built_in=excluded.built_in,active=excluded.active",
            params![kind.type_id,kind.name,kind.description,kind.sort_order,kind.built_in,kind.active])?;
    }
    let transfers = {
        let mut query = transaction
            .prepare("SELECT event_id,before_json FROM task_events WHERE kind='transferred'")?;
        let rows = query.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (event, before) in transfers {
        let before: serde_json::Value = serde_json::from_str(&before)?;
        if before.is_null() {
            continue;
        }
        let source = before
            .get("project_id")
            .and_then(|value| value.as_str())
            .ok_or_else(|| anyhow!("transfer event source missing"))?;
        let visible = super::repository::get_project(&task.org_id, source)?
            .map(|record| super::repository::project_access(&record, &task.user_id, false))
            .transpose()?
            .is_some_and(|access| {
                access.allows(
                    tentaflow_protocol::project_studio::access::ProjectArea::Tasks,
                    tentaflow_protocol::project_studio::access::ProjectPermissionLevel::Read,
                ) || access.allows(
                    tentaflow_protocol::project_studio::access::ProjectArea::Board,
                    tentaflow_protocol::project_studio::access::ProjectPermissionLevel::Read,
                )
            });
        if !visible {
            transaction.execute(
                "UPDATE task_events SET before_json='null' WHERE event_id=?1",
                [event],
            )?;
        }
    }
    transaction.commit()?;
    drop(copy);
    emit(tx, job_id, "writing", "zapis migawki bazy", 20);

    let mut files: Vec<FileEntry> = Vec::new();
    let result = (|| -> Result<u64> {
        let file = std::fs::File::create(&tmp)?;
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));

        files.push(write_file(&mut zip, "db/project.db", &snapshot, &|| {
            require_export_access(task)
        })?);

        let registry = serde_json::json!({
            "name": task.project.name,
            "key_prefix": task.project.key_prefix,
            "description": task.project.description,
            "template": task.project.template,
            "modules": task.project.modules,
            "functions": functions,
        });
        files.push(write_bytes(
            &mut zip,
            "db/registry.json",
            &serde_json::to_vec(&registry)?,
        )?);

        if task.options.include_user_names {
            let names = collect_user_names(&pool, &task.project_id);
            files.push(write_bytes(
                &mut zip,
                "db/user_names.json",
                &serde_json::to_vec(&names)?,
            )?);
        }

        emit(tx, job_id, "writing", "pakowanie plikow zrodel", 35);
        let referenced = super::repository::referenced_blob_sha256s(&pool)?;
        for (rel, abs) in walk_sorted(&task.dir_path.join("files"))? {
            if rel.starts_with(".uploads/") || rel.starts_with(".previews/") {
                continue;
            }
            if super::media::is_sha256(&rel) && !referenced.contains(&rel) {
                continue;
            }
            let entry = write_file(&mut zip, &format!("files/{rel}"), &abs, &|| {
                require_export_access(task)
            })?;
            inventory.bytes_files += entry.size;
            inventory.files += 1;
            files.push(entry);
        }

        for sha in &referenced {
            if let Ok(path) = super::media::preview_path(&task.dir_path, sha) {
                let state = super::media::read_preview_state(&task.dir_path, sha)?
                    .ok_or_else(|| anyhow!("completed preview metadata disappeared"))?;
                let entry = write_file(
                    &mut zip,
                    &format!("files/.previews/{sha}.mp4"),
                    &path,
                    &|| require_export_access(task),
                )?;
                if entry.size != state.total_size {
                    bail!("completed preview changed during export");
                }
                inventory.bytes_files += entry.size;
                files.push(entry);
                files.push(write_bytes(
                    &mut zip,
                    &format!("files/.previews/{sha}.json"),
                    &serde_json::to_vec(&state)?,
                )?);
            }
        }

        if task.options.include_runs {
            emit(tx, job_id, "writing", "pakowanie artefaktow przebiegow", 60);
            for (rel, abs) in walk_sorted(&task.dir_path.join("runs"))? {
                let entry = write_file(&mut zip, &format!("runs/{rel}"), &abs, &|| {
                    require_export_access(task)
                })?;
                inventory.bytes_runs += entry.size;
                files.push(entry);
            }
        }

        if task.options.include_vectors {
            emit(tx, job_id, "writing", "pakowanie indeksu wektorow", 80);
            for (rel, abs) in walk_sorted(&task.dir_path.join("vectors"))? {
                files.push(write_file(
                    &mut zip,
                    &format!("vectors/{rel}"),
                    &abs,
                    &|| require_export_access(task),
                )?);
            }
        }

        let manifest = ArchiveManifest {
            nodes: Vec::new(),
            version: ARCHIVE_VERSION,
            schema_version: super::project_db::LATEST_SCHEMA_VERSION,
            exported_at: chrono::Utc::now().to_rfc3339(),
            source_node_id: task.node_id.clone(),
            project: task.project.clone(),
            options: task.options,
            embedding: embedding.clone(),
            inventory: inventory.clone(),
            files: files.clone(),
            functions: functions.clone(),
        };
        write_bytes(&mut zip, MANIFEST_ENTRY, &serde_json::to_vec(&manifest)?)?;
        let mut out = zip.finish()?;
        out.flush()?;
        drop(out);
        Ok(std::fs::metadata(&tmp)?.len())
    })();

    let _ = std::fs::remove_file(&snapshot);
    let bytes = match result {
        Ok(bytes) => bytes,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    };
    require_export_access(task)?;
    std::fs::rename(&tmp, &task.dest_zip)?;
    emit(tx, job_id, "done", "archiwum gotowe", 100);
    Ok((bytes, inventory))
}

fn add_inventory(total: &mut Inventory, next: &Inventory) -> Result<()> {
    total.cases = total
        .cases
        .checked_add(next.cases)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    total.suites = total
        .suites
        .checked_add(next.suites)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    total.runs = total
        .runs
        .checked_add(next.runs)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    total.tasks = total
        .tasks
        .checked_add(next.tasks)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    total.documents = total
        .documents
        .checked_add(next.documents)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    total.sources = total
        .sources
        .checked_add(next.sources)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    total.files = total
        .files
        .checked_add(next.files)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    total.bytes_files = total
        .bytes_files
        .checked_add(next.bytes_files)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    total.bytes_runs = total
        .bytes_runs
        .checked_add(next.bytes_runs)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    total.vectors = total
        .vectors
        .checked_add(next.vectors)
        .ok_or_else(|| anyhow!("archive inventory overflow"))?;
    Ok(())
}

fn build_tree_export(
    job_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    task: &ExportTask,
) -> Result<(u64, Inventory)> {
    let staging = task.dest_zip.with_extension("nodes");
    std::fs::create_dir_all(&staging)?;
    let temp = task.dest_zip.with_extension("zip.tmp");
    let result = (|| -> Result<(u64, Inventory)> {
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(std::fs::File::create(&temp)?));
        let mut nodes = Vec::new();
        let mut inventory = Inventory::default();
        let mut records = task.nodes.clone();
        records.sort_by(|a, b| a.path.cmp(&b.path));
        for node in records {
            require_export_access(task)?;
            let archive = staging.join(format!("{}.zip", node.project_id));
            let inner = ExportTask {
                nodes: vec![node.clone()],
                scope: tentaflow_protocol::project_studio::ProjectTreeScope::Node,
                project_id: node.project_id.clone(),
                dir_path: node.dir_path.clone().into(),
                project: project_meta(&node)?,
                dest_zip: archive.clone(),
                ..task.clone()
            };
            build_export(job_id, tx, &inner)?;
            let manifest = read_manifest(&archive)?;
            add_inventory(&mut inventory, &manifest.inventory)?;
            let entry = write_file(
                &mut zip,
                &format!("nodes/{}.zip", node.project_id),
                &archive,
                &|| require_export_access(task),
            )?;
            nodes.push(TreeArchiveNode {
                entry,
                manifest: Box::new(manifest),
            });
            std::fs::remove_file(archive)?;
        }
        require_export_access(task)?;
        let manifest = ArchiveManifest {
            version: ARCHIVE_VERSION,
            schema_version: super::project_db::LATEST_SCHEMA_VERSION,
            exported_at: chrono::Utc::now().to_rfc3339(),
            source_node_id: task.node_id.clone(),
            project: task.project.clone(),
            options: task.options,
            embedding: None,
            inventory: inventory.clone(),
            files: nodes.iter().map(|node| node.entry.clone()).collect(),
            functions: super::repository::list_functions(&task.project_id)?,
            nodes,
        };
        write_bytes(&mut zip, MANIFEST_ENTRY, &serde_json::to_vec(&manifest)?)?;
        let mut file = zip.finish()?;
        file.flush()?;
        file.get_ref().sync_all()?;
        require_export_access(task)?;
        std::fs::rename(&temp, &task.dest_zip)?;
        Ok((std::fs::metadata(&task.dest_zip)?.len(), inventory))
    })();
    let _ = std::fs::remove_dir_all(&staging);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Display names of every identity referenced by the project's own rows.
fn collect_user_names(pool: &DbPool, project_id: &str) -> HashMap<String, String> {
    let mut ids: HashSet<String> = HashSet::new();
    if let Ok(conn) = pool.read() {
        for sql in [
            "SELECT DISTINCT created_by FROM test_cases",
            "SELECT DISTINCT created_by FROM tasks",
            "SELECT DISTINCT assigned_to FROM tasks",
            "SELECT DISTINCT created_by FROM test_runs",
            "SELECT DISTINCT assigned_to FROM test_run_items",
            "SELECT DISTINCT created_by FROM sources",
            "SELECT DISTINCT actor_user_id FROM activity_log",
        ] {
            if let Ok(mut stmt) = conn.prepare(sql) {
                if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0)) {
                    for id in rows.flatten() {
                        if !id.is_empty() {
                            ids.insert(id);
                        }
                    }
                }
            }
        }
    }
    for member in super::repository::list_members(project_id).unwrap_or_default() {
        ids.insert(member.user_id);
    }
    let list: Vec<String> = ids.into_iter().collect();
    super::repository::resolve_user_refs(&list)
        .into_iter()
        .map(|(id, (name, _email))| (id, name))
        .collect()
}

// =============================================================================
// Import: manifest + preview
// =============================================================================

/// Reads ONLY `manifest.json`. Nothing is unpacked before the user confirms,
/// and an archive from a NEWER schema is refused here — importing it would run
/// this binary's migrations over a future layout.
pub fn read_manifest(zip_path: &Path) -> Result<ArchiveManifest> {
    let file = std::fs::File::open(zip_path)
        .with_context(|| format!("otwarcie archiwum {}", zip_path.display()))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| anyhow!("plik nie jest poprawnym archiwum ZIP: {e}"))?;
    let manifest = {
        let mut entry = archive
            .by_name(MANIFEST_ENTRY)
            .map_err(|_| anyhow!("archiwum bez {MANIFEST_ENTRY} — to nie jest eksport projektu"))?;
        if entry.size() > MAX_MANIFEST_BYTES {
            bail!("manifest przekracza dopuszczalny rozmiar {MAX_MANIFEST_BYTES} B");
        }
        let mut buf = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut buf)?;
        serde_json::from_slice::<ArchiveManifest>(&buf).context("parsowanie manifest.json")?
    };
    if manifest.version != ARCHIVE_VERSION {
        bail!(
            "nieobslugiwana wersja archiwum {} (ta instancja obsluguje {ARCHIVE_VERSION})",
            manifest.version
        );
    }
    if manifest.schema_version > super::project_db::LATEST_SCHEMA_VERSION {
        bail!(
            "archiwum pochodzi z nowszej wersji TentaFlow (schemat {} > {}) — zaktualizuj wezel",
            manifest.schema_version,
            super::project_db::LATEST_SCHEMA_VERSION
        );
    }
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &ArchiveManifest) -> Result<()> {
    super::repository::validate_function_catalogue(&manifest.functions)?;
    validated_registry_meta(&manifest.project)?;
    if manifest.nodes.is_empty() {
        if manifest
            .files
            .iter()
            .any(|file| file.path.starts_with("nodes/"))
        {
            bail!("single-node archive contains tree entries");
        }
        return Ok(());
    }
    let mut ids = HashSet::new();
    let mut entries = HashSet::new();
    let mut total = 0u64;
    for node in &manifest.nodes {
        if !node.manifest.nodes.is_empty()
            || node.manifest.version != ARCHIVE_VERSION
            || node.manifest.schema_version > super::project_db::LATEST_SCHEMA_VERSION
        {
            bail!("invalid nested node archive");
        }
        if !ids.insert(node.manifest.project.project_id.clone())
            || !entries.insert(node.entry.path.clone())
        {
            bail!("tree archive contains duplicate node identities");
        }
        if node.entry.path != format!("nodes/{}.zip", node.manifest.project.project_id)
            || node.entry.path.contains("..")
            || node.entry.path.contains('\\')
        {
            bail!("tree archive node path is invalid");
        }
        validate_manifest(&node.manifest)?;
        total = node.manifest.files.iter().try_fold(total, |n, file| {
            n.checked_add(file.size)
                .ok_or_else(|| anyhow!("archive size overflow"))
        })?;
    }
    if total > MAX_IMPORT_BYTES || !ids.contains(&manifest.project.project_id) {
        bail!("tree archive inventory is invalid");
    }
    if manifest.files.len() != manifest.nodes.len()
        || manifest.files.iter().any(|entry| {
            !manifest.nodes.iter().any(|node| {
                node.entry.path == entry.path
                    && node.entry.sha256 == entry.sha256
                    && node.entry.size == entry.size
            })
        })
    {
        bail!("tree archive entries differ from manifest");
    }
    for node in &manifest.nodes {
        let mut seen = HashSet::new();
        let mut current = &node.manifest.project;
        while current.project_id != manifest.project.project_id {
            if !seen.insert(current.project_id.clone()) || seen.len() >= 4 {
                bail!("tree archive exceeds project depth or contains a cycle");
            }
            let parent = current
                .parent_id
                .as_ref()
                .ok_or_else(|| anyhow!("tree archive is disconnected"))?;
            current = &manifest
                .nodes
                .iter()
                .find(|node| &node.manifest.project.project_id == parent)
                .ok_or_else(|| anyhow!("tree archive parent is missing"))?
                .manifest
                .project;
        }
    }
    Ok(())
}

/// Metadata schema of the passage namespace in the SAME shape the vector
/// registry stores in `fields_json`, so the archived value and the local one are
/// directly comparable.
fn local_field_fingerprint() -> String {
    #[derive(Serialize)]
    struct Stored<'a> {
        name: &'a str,
        #[serde(rename = "type")]
        ty: &'a str,
        indexed: bool,
    }
    let specs = super::ingest::passage_field_specs();
    let stored: Vec<Stored<'_>> = specs
        .iter()
        .map(|f| Stored {
            name: &f.name,
            ty: match f.field_type {
                tentaflow_sdk_spec::FieldType::Str => "str",
                tentaflow_sdk_spec::FieldType::Int => "int",
                tentaflow_sdk_spec::FieldType::Float => "float",
                tentaflow_sdk_spec::FieldType::Bool => "bool",
            },
            indexed: f.indexed,
        })
        .collect();
    serde_json::to_string(&stored).unwrap_or_else(|_| "[]".to_string())
}

/// Whether the archived vector index may be moved verbatim onto this node, and
/// why not when it may not.
pub fn vectors_reusable(
    core_db: &DbPool,
    org_id: &str,
    manifest: &ArchiveManifest,
) -> (bool, String) {
    let Some(embedding) = manifest.embedding.as_ref() else {
        return (false, "archiwum nie zawiera indeksu wektorow".to_string());
    };
    if embedding.vector_count == 0 {
        return (false, "archiwum ma pusty indeks".to_string());
    }
    let Ok(conn) = core_db.read() else {
        return (false, "rejestr modeli niedostepny".to_string());
    };
    let local_model: String = conn
        .query_row(
            "SELECT target_model FROM model_aliases WHERE alias = ?1",
            params![embedding.alias],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or_default();
    if local_model.is_empty() {
        return (
            false,
            format!("alias '{}' nie jest skonfigurowany", embedding.alias),
        );
    }
    if local_model != embedding.target_model {
        // Same dimension + different model = silent garbage in RAG. Re-indexing
        // costs time; wrong retrieval costs trust.
        return (
            false,
            format!(
                "alias '{}' wskazuje tu model '{}', a archiwum zbudowano na '{}'",
                embedding.alias, local_model, embedding.target_model
            ),
        );
    }
    if local_field_fingerprint() != embedding.fields {
        return (false, "schemat metadanych wektorow sie roznii".to_string());
    }
    if embedding.metric != "cosine" {
        return (
            false,
            format!("miara '{}' nie odpowiada lokalnej", embedding.metric),
        );
    }
    // The dimension is derived from a real embedding at ingest time, never
    // configured, so the authoritative local value is the one this org's other
    // project namespaces were built with. A restored index of a different width
    // would be opened under the archived dimension and every query against it
    // would be answered from a different space.
    let local_dim: Option<i64> = conn
        .query_row(
            "SELECT dim FROM addon_vector_namespaces \
             WHERE org_id = ?1 AND namespace = ?2 AND addon_id LIKE 'ps-%' \
             GROUP BY dim ORDER BY COUNT(*) DESC, dim LIMIT 1",
            params![org_id, super::ingest::VECTOR_NAMESPACE],
            |row| row.get(0),
        )
        .optional()
        .ok()
        .flatten();
    if let Some(local_dim) = local_dim {
        if local_dim != i64::from(embedding.dim) {
            return (
                false,
                format!(
                    "wymiar wektorow archiwum to {}, a lokalne projekty uzywaja {local_dim}",
                    embedding.dim
                ),
            );
        }
    }
    (true, String::new())
}

// =============================================================================
// Import: extraction
// =============================================================================

/// Validated, contained relative path of an archive entry. Rejects absolute
/// paths, `..` traversal, symlink entries and anything outside the layout — a
/// hostile archive must not write a single byte outside the staging directory.
fn safe_entry_path<R: Read>(entry: &zip::read::ZipFile<'_, R>) -> Result<Option<PathBuf>> {
    // Unix mode 0xA000 is S_IFLNK: a symlink entry's "content" is a target path,
    // and materializing it would let a LATER entry write through it.
    if let Some(mode) = entry.unix_mode() {
        if mode & 0xF000 == 0xA000 {
            bail!("archiwum zawiera dowiazanie symboliczne: {}", entry.name());
        }
    }
    let name = entry.name().to_string();
    if name.is_empty() {
        return Ok(None);
    }
    let rel = entry
        .enclosed_name()
        .ok_or_else(|| anyhow!("niebezpieczna sciezka w archiwum: {name}"))?;
    if rel.components().count() == 0 {
        return Ok(None);
    }
    let normalized = rel.to_string_lossy().replace('\\', "/");
    if normalized != MANIFEST_ENTRY
        && !ALLOWED_PREFIXES
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
    {
        bail!("wpis spoza dozwolonego ukladu archiwum: {normalized}");
    }
    Ok(Some(rel))
}

/// Extracts the whole archive into `staging`, enforcing the entry-count and
/// byte caps and verifying every file's sha256 against the manifest.
fn extract_all(zip_path: &Path, staging: &Path, manifest: &ArchiveManifest) -> Result<()> {
    let expected: HashMap<&str, &FileEntry> = manifest
        .files
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    if expected.len() != manifest.files.len() {
        bail!("archive declares duplicate file paths");
    }
    let total = manifest.files.iter().try_fold(0u64, |n, file| {
        n.checked_add(file.size)
            .ok_or_else(|| anyhow!("archive size overflow"))
    })?;
    if total > MAX_IMPORT_BYTES {
        bail!("archiwum deklaruje {total} B po rozpakowaniu — limit to {MAX_IMPORT_BYTES} B");
    }

    let file = std::fs::File::open(zip_path)?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| anyhow!("plik nie jest poprawnym archiwum ZIP: {e}"))?;
    if archive.len() > MAX_IMPORT_ENTRIES {
        bail!(
            "archiwum ma {} wpisow — limit to {MAX_IMPORT_ENTRIES}",
            archive.len()
        );
    }

    let mut seen = HashSet::new();
    let mut written: u64 = 0;
    let mut buf = vec![0u8; COPY_BUF];
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i)?;
        let Some(rel) = safe_entry_path(&entry)? else {
            continue;
        };
        if !seen.insert(rel.clone()) {
            bail!("archive contains duplicate entries");
        }
        let out_path = staging.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let mut out = std::io::BufWriter::new(std::fs::File::create(&out_path)?);
        let mut hasher = Sha256::new();
        let mut size = 0u64;
        loop {
            let n = entry.read(&mut buf)?;
            if n == 0 {
                break;
            }
            written += n as u64;
            // The declared total is checked up front, but a LYING manifest must
            // not be able to make us write past the cap either.
            if written > MAX_IMPORT_BYTES {
                bail!("rozpakowane dane przekroczyly limit {MAX_IMPORT_BYTES} B");
            }
            hasher.update(&buf[..n]);
            out.write_all(&buf[..n])?;
            size += n as u64;
        }
        out.flush()?;
        drop(out);

        if rel_str != MANIFEST_ENTRY {
            let Some(want) = expected.get(rel_str.as_str()) else {
                bail!("wpis {rel_str} nie wystepuje w manifescie");
            };
            if hex(&hasher.finalize()) != want.sha256 || size != want.size {
                bail!("uszkodzony wpis {rel_str}: niezgodna suma kontrolna");
            }
        }
    }
    // Every declared entry must have been present: a stripped archive would
    // otherwise import as a project with silently missing documents.
    for f in &manifest.files {
        if !staging.join(&f.path).is_file() {
            bail!("archiwum niekompletne — brak {}", f.path);
        }
    }
    Ok(())
}

// =============================================================================
// Import: apply
// =============================================================================

pub struct ImportTask {
    pub context: crate::dispatch::HandlerContext,
    pub core_db: DbPool,
    pub router: Arc<Router>,
    pub org_id: String,
    pub user_id: String,
    pub archive_path: PathBuf,
    pub manifest: ArchiveManifest,
    pub name_override: String,
    pub import_vectors: bool,
    pub import_runs: bool,
}

fn require_import_access(task: &ImportTask) -> Result<()> {
    let actor = crate::db::repository::get_user_account_by_id(&task.core_db, &task.user_id)?
        .filter(|account| account.is_active)
        .ok_or_else(|| anyhow!("import actor account is unavailable"))?;
    let org =
        crate::services::rbac::resolve_org_context(&task.core_db, &actor.id, Some(&task.org_id))
            .map_err(|error| anyhow!(error))?;
    let mut context = task.context.clone();
    context.org_context = Some(org);
    crate::dispatch::project_studio::require_import_grant(&context)
        .map_err(|error| anyhow!(error.message))?;
    Ok(())
}

/// Starts an import. All-or-nothing: any failure removes the freshly created
/// directory AND the registry row, so a half-imported project never appears in
/// the list.
pub fn spawn_import(task: ImportTask) -> Result<String> {
    let key = format!("psimport:{}", task.archive_path.to_string_lossy());
    if !try_claim(&key) {
        bail!("import tego archiwum juz trwa");
    }
    let job_id = uuid::Uuid::new_v4().to_string();
    set_job(
        &job_id,
        ArchiveJob {
            status: "running".to_string(),
            phase: "extracting".to_string(),
            owner_user_id: task.user_id.clone(),
            ..ArchiveJob::default()
        },
    );
    let tx = log_bus::sender_for(&job_id);
    let job_task = job_id.clone();
    tokio::spawn(async move {
        let tx_task = tx.clone();
        let jid = job_task.clone();
        let result =
            tokio::task::spawn_blocking(move || run_import(&job_task, &tx_task, &task)).await;
        match result {
            Ok(Ok(())) => finish(&tx, &jid, "success", ""),
            Ok(Err(e)) => {
                tracing::warn!(job_id = %jid, "project studio import failed: {e:#}");
                finish(&tx, &jid, "failed", &e.to_string());
            }
            Err(e) => {
                tracing::warn!(job_id = %jid, "project studio import panicked: {e}");
                finish(&tx, &jid, "failed", "import task panicked");
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        log_bus::close(&jid);
        release(&key);
    });
    Ok(job_id)
}

fn run_import(
    job_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    task: &ImportTask,
) -> Result<()> {
    require_import_access(task)?;
    validate_manifest(&task.manifest)?;
    let root = crate::paths::data_dir().join("projects");
    std::fs::create_dir_all(&root)?;
    let imports = super::media::safe_directory(&root, ".imports")?;
    let staging = imports.join(job_id);
    std::fs::create_dir(&staging)?;
    let operation_id = job_id.to_string();
    let mut reserved = false;
    let mut published = false;
    let mut destinations = Vec::new();
    let result = (|| -> Result<()> {
        let hash = super::media::hash_file(&task.archive_path, &|| require_import_access(task))?;
        extract_all(&task.archive_path, &staging.join("archive"), &task.manifest)?;
        let manifests = if task.manifest.nodes.is_empty() {
            vec![(task.manifest.clone(), staging.join("archive"))]
        } else {
            let mut nodes = Vec::new();
            for node in &task.manifest.nodes {
                let archive = staging.join("archive").join(&node.entry.path);
                let actual = read_manifest(&archive)?;
                if serde_json::to_vec(&actual)? != serde_json::to_vec(&*node.manifest)? {
                    bail!("nested node manifest differs from tree manifest");
                }
                let directory = staging.join(&actual.project.project_id);
                extract_all(&archive, &directory, &actual)?;
                nodes.push((actual, directory));
            }
            nodes
        };
        let mut project_map = HashMap::new();
        let mut task_map = HashMap::new();
        for (manifest, directory) in &manifests {
            require_import_access(task)?;
            let id = uuid::Uuid::new_v4().to_string();
            if project_map
                .insert(manifest.project.project_id.clone(), id)
                .is_some()
            {
                bail!("duplicate source project identity");
            }
            let (pool, version) = super::project_db::open_pool_at(&directory.join("db"))?;
            if version > super::project_db::LATEST_SCHEMA_VERSION {
                bail!("archive contains a future project schema");
            }
            let conn = pool.read()?;
            let mut statement = conn.prepare("SELECT task_id FROM tasks")?;
            for row in statement.query_map([], |row| row.get::<_, String>(0))? {
                if task_map
                    .insert(row?, uuid::Uuid::new_v4().to_string())
                    .is_some()
                {
                    bail!("tree archive contains duplicate task identities");
                }
            }
        }
        let source_root = &task.manifest.project.project_id;
        let mut nodes = Vec::new();
        for (manifest, _) in &manifests {
            let meta = &manifest.project;
            let (description, template, modules) = validated_registry_meta(meta)?;
            let raw_name =
                if &meta.project_id == source_root && !task.name_override.trim().is_empty() {
                    task.name_override.trim()
                } else {
                    meta.name.trim()
                };
            if raw_name.is_empty() || raw_name.len() > 200 {
                bail!("archive project name is invalid");
            }
            let id = project_map
                .get(&meta.project_id)
                .ok_or_else(|| anyhow!("import identity missing"))?;
            let parent = meta
                .parent_id
                .as_ref()
                .and_then(|id| project_map.get(id))
                .cloned();
            nodes.push(super::models::ProjectImportNode {
                project_id: id.clone(),
                parent_id: parent.clone(),
                name: raw_name.into(),
                description,
                template,
                modules_json: serde_json::to_string(&modules)?,
                owner_user_id: task.user_id.clone(),
                dir_path: super::project_dir(id).to_string_lossy().into_owned(),
                key_prefix: meta.key_prefix.clone(),
                is_private: meta.is_private,
                inherit_modules: parent.is_some() && meta.inherit_modules,
                inherit_task_types: parent.is_some() && meta.inherit_task_types,
                status: "active".into(),
                lifecycle: meta.lifecycle.clone(),
                members: Vec::new(),
                functions: manifest.functions.clone(),
            });
        }
        let resolved = super::repository::prepare_project_tree_import(
            &operation_id,
            &task.org_id,
            &hash,
            &nodes,
        )?;
        reserved = true;
        destinations = resolved.clone();
        for (manifest, directory) in &manifests {
            let id = project_map
                .get(&manifest.project.project_id)
                .ok_or_else(|| anyhow!("import identity missing"))?;
            let node = resolved
                .iter()
                .find(|node| &node.project_id == id)
                .ok_or_else(|| anyhow!("reserved import identity missing"))?;
            let child = ImportTask {
                manifest: manifest.clone(),
                archive_path: task.archive_path.clone(),
                name_override: node.name.clone(),
                context: task.context.clone(),
                core_db: task.core_db.clone(),
                router: task.router.clone(),
                org_id: task.org_id.clone(),
                user_id: task.user_id.clone(),
                import_runs: task.import_runs,
                import_vectors: task.import_vectors,
            };
            apply_import(job_id, tx, &child, node, directory, &task_map, &project_map)?;
        }
        require_import_access(task)?;
        super::repository::publish_project_tree_import(
            &operation_id,
            &task.org_id,
            &hash,
            &resolved,
        )?;
        published = true;
        let root_id = project_map
            .get(source_root)
            .ok_or_else(|| anyhow!("import root identity missing"))?;
        update_job(job_id, |job| {
            job.project_id = root_id.clone();
            job.scope_project_ids = resolved
                .iter()
                .map(|node| node.project_id.clone())
                .collect();
        });
        let mut published_nodes = Vec::new();
        for node in &resolved {
            let record = super::repository::get_project(&task.org_id, &node.project_id)?
                .ok_or_else(|| anyhow!("published import registry is missing"))?;
            let pool = super::project_db::open(&node.project_id)?;
            while super::task_index::sync_project(&record, &pool, 256)?.lag != 0 {}
            published_nodes.push((record, pool));
        }
        for (record, pool) in published_nodes {
            super::task_transfer::reconcile_project_relations(&record, &pool)?;
        }
        for (manifest, directory) in &manifests {
            let id = &project_map[&manifest.project.project_id];
            let node = resolved
                .iter()
                .find(|node| &node.project_id == id)
                .ok_or_else(|| anyhow!("published import identity missing"))?;
            let child = ImportTask {
                manifest: manifest.clone(),
                archive_path: task.archive_path.clone(),
                name_override: node.name.clone(),
                context: task.context.clone(),
                core_db: task.core_db.clone(),
                router: task.router.clone(),
                org_id: task.org_id.clone(),
                user_id: task.user_id.clone(),
                import_runs: task.import_runs,
                import_vectors: task.import_vectors,
            };
            finish_import(job_id, tx, &child, node, directory)?;
        }
        Ok(())
    })();
    if result.is_err() && reserved && !published {
        let any_published = destinations.iter().try_fold(false, |found, node| {
            Ok::<_, anyhow::Error>(
                found || super::repository::get_project(&task.org_id, &node.project_id)?.is_some(),
            )
        })?;
        if !any_published {
            for node in &destinations {
                super::project_db::close(&node.project_id);
                let dir = Path::new(&node.dir_path);
                if dir.exists() {
                    std::fs::remove_dir_all(dir)?;
                }
            }
            super::repository::abort_project_tree_import(&operation_id)?;
        }
    }
    std::fs::remove_dir_all(&staging)?;
    result
}

/// Registry fields carried by a manifest, validated like the wire input of
/// `project_create` — the manifest comes from a foreign node, and both paths
/// write the same registry row that every project list then renders. The
/// description is bounded here rather than trusted: unlike the wire, nobody
/// typed it into a form on this node.
fn validated_registry_meta(project: &ProjectMeta) -> Result<(String, String, Vec<String>)> {
    let description: String = project.description.trim().chars().take(2_000).collect();
    if !matches!(project.lifecycle.as_str(), "active" | "ended") {
        bail!("archive lifecycle is invalid");
    }
    if super::db::normalize_key_prefix(&project.key_prefix).is_none() {
        bail!("archive project prefix is invalid");
    }
    if !super::VALID_TEMPLATES.contains(&project.template.as_str()) {
        bail!("archiwum deklaruje nieznany szablon '{}'", project.template);
    }
    let mut modules = Vec::with_capacity(project.modules.len());
    for module in &project.modules {
        if !super::VALID_MODULES.contains(&module.as_str()) {
            bail!("archiwum deklaruje nieznany modul '{module}'");
        }
        if !modules.contains(module) {
            modules.push(module.clone());
        }
    }
    Ok((description, project.template.clone(), modules))
}

fn apply_import(
    job_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    task: &ImportTask,
    node: &super::models::ProjectImportNode,
    staging: &Path,
    task_map: &HashMap<String, String>,
    project_map: &HashMap<String, String>,
) -> Result<()> {
    let dir = Path::new(&node.dir_path);
    if dir.exists() {
        bail!("reserved import directory already exists");
    }
    std::fs::create_dir_all(dir.join("files"))?;
    std::fs::rename(staging.join("db/project.db"), dir.join("project.db"))?;
    move_tree(&staging.join("files"), &dir.join("files"))?;
    if task.import_runs && task.manifest.options.include_runs {
        move_tree(&staging.join("runs"), &dir.join("runs"))?;
    }
    let (pool, version) = super::project_db::open_pool_at(dir)?;
    if version > super::project_db::LATEST_SCHEMA_VERSION {
        bail!("imported schema is newer than this binary");
    }
    rekey_imported_tasks(
        &pool,
        &node.key_prefix,
        &node.project_id,
        task_map,
        project_map,
    )?;
    remap_identities(&pool, &task.user_id)?;
    if !task.import_runs {
        drop_run_history(&pool)?;
    }
    if task.manifest.options.include_user_names {
        let names = std::fs::read_to_string(staging.join("db/user_names.json"))?;
        let _: HashMap<String, String> = serde_json::from_str(&names)?;
        super::repository::set_setting(&pool, "imported_user_display_names", &names)?;
    }
    super::repository::set_setting(&pool,"imported_from",&serde_json::json!({
        "source_node_id":task.manifest.source_node_id,"source_project_id":task.manifest.project.project_id,
        "exported_at":task.manifest.exported_at,"imported_at":chrono::Utc::now().to_rfc3339()}).to_string())?;
    emit(tx, job_id, "staging", "validated node content staged", 50);
    Ok(())
}

fn finish_import(
    job_id: &str,
    tx: &tokio::sync::broadcast::Sender<BusMessage>,
    task: &ImportTask,
    node: &super::models::ProjectImportNode,
    staging: &Path,
) -> Result<()> {
    let dir = Path::new(&node.dir_path);
    let pool = super::project_db::open(&node.project_id)?;
    super::schedules::refresh_hint(&node.project_id, &task.org_id)?;
    let (reused, reason) = restore_vectors(task, &node.project_id, dir, staging)?;
    let reindex = if !reused && node.lifecycle == "active" {
        reindex_sources(task, &node.project_id, &pool, dir)
    } else {
        Vec::new()
    };
    update_job(job_id, |job| {
        job.vectors_imported |= reused;
        job.reindex_job_ids.extend(reindex.clone());
    });
    super::repository::set_setting(
        &pool,
        "imported_vectors",
        &serde_json::json!({"reused":reused,"reason":reason}).to_string(),
    )?;
    super::activity::record(&pool,&task.user_id,"user","project.imported","project",&node.project_id,
        &serde_json::json!({"name":node.name,"source_node_id":task.manifest.source_node_id,"vectors_reused":reused}).to_string());
    super::notifications::notify(
        &task.org_id,
        &task.user_id,
        &node.project_id,
        "project_imported",
        "Project imported",
        &node.name,
        &serde_json::json!({"project_id":node.project_id}).to_string(),
    );
    emit(tx, job_id, "done", "import completed", 100);
    Ok(())
}

/// Import staging shares the destination filesystem so content publication is a rename.
fn move_tree(src: &Path, dest: &Path) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    for (rel, abs) in walk_sorted(src)? {
        let target = dest.join(&rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::rename(&abs, &target)?;
    }
    Ok(())
}

/// Rewrites every authorship column to the importing identity. Historical names
/// stay readable through `settings.imported_user_display_names`; the ids
/// themselves must not survive, because a user id from another node means
/// nothing here (and could accidentally collide with a real local account).
/// Assignments on rows that are still open are CLEARED — an imported project
/// must not look like work is assigned to people who cannot see it.
fn remap_identities(pool: &DbPool, importer: &str) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("imported project write: {e}"))?;
    let tx = conn.unchecked_transaction()?;
    for sql in [
        "UPDATE test_cases SET created_by = ?1",
        "UPDATE test_case_versions SET created_by = ?1",
        "UPDATE test_suites SET created_by = ?1",
        "UPDATE test_runs SET created_by = ?1, closed_by = ''",
        "UPDATE tasks SET created_by = ?1",
        "UPDATE task_links SET created_by = ?1",
        "UPDATE task_events SET actor_id = ?1 WHERE actor_kind = 'user'",
        "UPDATE task_comments SET author_user_id = ?1",
        "UPDATE sources SET created_by = ?1",
        "UPDATE ingest_jobs SET started_by = ?1",
        "UPDATE generation_runs SET started_by = ?1",
        "UPDATE environments SET requested_by = ?1, decided_by = ''",
        "UPDATE schedules SET created_by = ?1",
        "UPDATE ml_links SET created_by = ?1",
        "UPDATE tags SET created_by = ?1",
        "UPDATE activity_log SET actor_user_id = ?1",
    ] {
        tx.execute(sql, params![importer])?;
    }
    tx.execute(
        "UPDATE tasks SET assigned_to = '' WHERE status <> 'done'",
        [],
    )?;
    tx.execute("UPDATE task_comments SET mention_user_ids_json = '[]'", [])?;
    tx.execute(
        "UPDATE test_run_items SET assigned_to = '' \
         WHERE status IN ('pending', 'in_progress', 'running')",
        [],
    )?;
    // ML links cannot follow a project across nodes: the ML project id belongs
    // to the exporting node's ML Studio registry.
    tx.execute("DELETE FROM ml_links", [])?;
    tx.commit()?;
    Ok(())
}

fn rekey_imported_tasks(
    pool: &DbPool,
    prefix: &str,
    destination_project_id: &str,
    task_uuid_map: &HashMap<String, String>,
    project_uuid_map: &HashMap<String, String>,
) -> Result<()> {
    let conn = pool.write()?;
    let tx = conn.unchecked_transaction()?;
    tx.pragma_update(None, "defer_foreign_keys", true)?;
    tx.execute("DELETE FROM task_index_outbox", [])?;
    tx.execute(
        "DELETE FROM sqlite_sequence WHERE name='task_index_outbox'",
        [],
    )?;
    let rows = {
        let mut query = tx.prepare(
            "SELECT task_id,task_no,task_key,parent_task_id FROM tasks ORDER BY task_no",
        )?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let relations = {
        let mut query=tx.prepare("SELECT link_id,source_task_id,target_task_id,source_project_id,target_project_id FROM task_links")?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (old_id, number, old_key, parent) in rows {
        let id = task_uuid_map
            .get(&old_id)
            .ok_or_else(|| anyhow!("import task identity map is incomplete"))?;
        let parent = parent
            .as_ref()
            .map(|parent| {
                task_uuid_map
                    .get(parent)
                    .cloned()
                    .ok_or_else(|| anyhow!("import parent task identity is missing"))
            })
            .transpose()?;
        let key = format!("{prefix}-{number}");
        tx.execute(
            "UPDATE tasks SET task_id=?1,task_key=?2,parent_task_id=?3 WHERE task_id=?4",
            params![id, key, parent, old_id],
        )?;
        tx.execute(
            "UPDATE task_comments SET task_id=?1 WHERE task_id=?2",
            params![id, old_id],
        )?;
        tx.execute(
            "UPDATE task_events SET task_id=?1 WHERE task_id=?2",
            params![id, old_id],
        )?;
        tx.execute("INSERT INTO task_events(task_id,actor_kind,actor_id,kind,before_json,after_json) VALUES(?1,'system','','imported',?2,?3)",
            params![id,serde_json::json!({"task_id":old_id,"task_key":old_key}).to_string(),
                serde_json::json!({"task_id":id,"task_key":key,"project_id":destination_project_id}).to_string()])?;
    }
    for (link, source, target, source_project, target_project) in relations {
        let endpoints = task_uuid_map.get(&source).zip(task_uuid_map.get(&target));
        let projects = project_uuid_map
            .get(&source_project)
            .zip(project_uuid_map.get(&target_project));
        if let (Some((source, target)), Some((source_project, target_project))) =
            (endpoints, projects)
        {
            tx.execute("UPDATE task_links SET relation_id=?1,source_task_id=?2,target_task_id=?3,source_project_id=?4,target_project_id=?5 WHERE link_id=?6",
                params![uuid::Uuid::new_v4().to_string(),source,target,source_project,target_project,link])?;
        } else {
            tx.execute("DELETE FROM task_links WHERE link_id=?1", [link])?;
        }
    }
    tx.execute("INSERT INTO settings(key,value) VALUES('project_key_prefix',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[prefix])?;
    tx.execute("UPDATE settings SET value=CASE WHEN EXISTS(SELECT 1 FROM tasks) THEN '1' ELSE value END WHERE key='project_key_prefix_locked'",[])?;
    tx.execute("UPDATE settings SET value=CAST(MAX(CAST(value AS INTEGER),(SELECT COALESCE(MAX(task_no),0)+1 FROM tasks)) AS TEXT) WHERE key='task_next_no'",[])?;
    tx.commit()?;
    Ok(())
}

fn drop_run_history(pool: &DbPool) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("imported project write: {e}"))?;
    let tx = conn.unchecked_transaction()?;
    for sql in [
        "DELETE FROM test_run_steps",
        "DELETE FROM test_run_items",
        "DELETE FROM run_artifacts",
        "DELETE FROM auto_run_meta",
        "DELETE FROM test_runs",
        "DELETE FROM schedule_runs",
    ] {
        tx.execute(sql, [])?;
    }
    tx.commit()?;
    Ok(())
}

/// Appends a numeric suffix until the name is free in the org.

fn restore_vectors(
    task: &ImportTask,
    project_id: &str,
    dir: &Path,
    staging: &Path,
) -> Result<(bool, String)> {
    if !task.import_vectors {
        return Ok((false, "uzytkownik wybral przebudowe".to_string()));
    }
    let (reusable, reason) = vectors_reusable(&task.core_db, &task.org_id, &task.manifest);
    if !reusable {
        return Ok((false, reason));
    }
    let Some(embedding) = task.manifest.embedding.as_ref() else {
        return Ok((false, "brak odcisku modelu".to_string()));
    };
    let vectors_dir = dir.join("vectors");
    std::fs::create_dir_all(&vectors_dir)?;
    move_tree(&staging.join("vectors"), &vectors_dir)?;

    // The namespace row is created AT the project directory, so opening it picks
    // up the files just restored instead of building an empty index elsewhere.
    let manager = crate::services::vector_namespace_manager(&task.core_db);
    let backend = manager
        .get_or_create_at(
            &task.org_id,
            &super::ingest::vector_scope(project_id),
            super::ingest::VECTOR_NAMESPACE,
            embedding.dim,
            crate::services::vector::backend::Metric::Cosine,
            &super::ingest::passage_field_specs(),
            false,
            &vectors_dir,
        )
        .map_err(|e| anyhow!("rejestracja namespace wektorow: {e}"))?;
    // The cached count comes from the fresh row (zero); the restored index knows
    // the truth, and the per-tenant quota reads that column.
    if let Ok(conn) = task.core_db.write() {
        let _ = conn.execute(
            "UPDATE addon_vector_namespaces SET count = ?1 WHERE org_id = ?2 AND addon_id = ?3 \
             AND namespace = ?4",
            params![
                backend.count() as i64,
                task.org_id,
                super::ingest::vector_scope(project_id),
                super::ingest::VECTOR_NAMESPACE
            ],
        );
    }
    Ok((true, String::new()))
}

/// Re-indexes every source from the blobs that travelled in the archive — no
/// network access is needed, the bytes are already on disk.
fn reindex_sources(task: &ImportTask, project_id: &str, pool: &DbPool, dir: &Path) -> Vec<String> {
    let Ok(sources) = super::repository::list_sources(pool) else {
        return Vec::new();
    };
    let mut jobs = Vec::new();
    for source in sources {
        let source_id = source.record.source_id.clone();
        let Ok(files) = super::repository::files_for_ingest(pool, &source_id, None) else {
            continue;
        };
        let work: Vec<super::ingest::FileWork> = files
            .into_iter()
            .filter(|f| dir.join("files").join(&f.sha256).is_file())
            .map(|f| super::ingest::FileWork {
                file_id: f.file_id,
                path: f.path,
                sha256: f.sha256,
                mime: f.mime,
                payload: super::ingest::WorkPayload::Blob,
            })
            .collect();
        if work.is_empty() {
            continue;
        }
        let job_id = uuid::Uuid::new_v4().to_string();
        if super::repository::create_ingest_job(
            pool,
            &job_id,
            &source_id,
            work.len() as u32,
            &task.user_id,
        )
        .is_err()
        {
            continue;
        }
        super::ingest::start_job(super::ingest::IngestTask {
            core_db: task.core_db.clone(),
            router: task.router.clone(),
            project_pool: pool.clone(),
            org_id: task.org_id.clone(),
            project_id: project_id.to_string(),
            dir_path: dir.to_path_buf(),
            source_id,
            job_id: job_id.clone(),
            files: work,
        });
        jobs.push(job_id);
    }
    jobs
}

// =============================================================================
// Retention
// =============================================================================

/// Export archives are deleted after this age (== the max signed-URL TTL), so a
/// link never outlives the file it points at.
pub const EXPORT_RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

/// Deletes export archives older than the retention window. Run on export start
/// so finished-but-abandoned archives never accumulate on the cache disk.
pub fn reap_export_archives() {
    let now = std::time::SystemTime::now();
    let Ok(entries) = std::fs::read_dir(crate::paths::project_studio_exports_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("zip") {
            continue;
        }
        if let Ok(age) = entry
            .metadata()
            .and_then(|m| m.modified())
            .and_then(|modified| now.duration_since(modified).map_err(std::io::Error::other))
        {
            if age >= EXPORT_RETENTION {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[tokio::test]
    async fn recovery_finishes_quarantined_delete_and_aborts_unpublished_tree() {
        let tmp = tempfile::tempdir().expect("operation storage");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        let state = crate::dispatch::state::AppState::for_test();
        let org = format!("recovery-{}", uuid::Uuid::new_v4());
        let id = uuid::Uuid::new_v4().to_string();
        let dir = tmp.path().join(&id);
        std::fs::create_dir_all(&dir).expect("actual project directory");
        super::super::repository::create_project(
            &id,
            &org,
            "Recover deletion",
            "",
            "custom",
            "[\"tasks\"]",
            "owner",
            &dir.to_string_lossy(),
            "",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("actual registry row");
        let content = super::super::project_db::open(&id).expect("actual content database");
        drop(content);
        super::super::project_db::close(&id);
        let operation = super::super::repository::prepare_project_delete(&org, &id, "owner")
            .expect("durable delete admission");
        let quarantine = tmp.path().join(format!(".deleted-{operation}"));
        let journal_path = deletion_journal_dir()
            .expect("journal storage")
            .join(format!("{operation}.json"));
        super::super::media::write_metadata(
            &journal_path,
            &DeletionJournal {
                project_id: id.clone(),
                org_id: org.clone(),
                directory: dir.clone(),
                quarantine: quarantine.clone(),
                operation_id: operation.clone(),
            },
        )
        .expect("durable deletion journal");
        std::fs::rename(&dir, &quarantine).expect("destructive cut point before registry commit");
        assert!(
            super::super::project_db::open(&id).is_err(),
            "an admitted deletion must not recreate content"
        );
        recover_operations(&state.db).expect("restart completes quarantined deletion");
        assert!(super::super::repository::get_project(&org, &id)
            .expect("registry")
            .is_none());
        assert!(!dir.exists() && !quarantine.exists() && !journal_path.exists());

        let operation = uuid::Uuid::new_v4().to_string();
        let root = uuid::Uuid::new_v4().to_string();
        let child = uuid::Uuid::new_v4().to_string();
        let node = |project_id: String, parent_id: Option<String>, name: &str| {
            super::super::models::ProjectImportNode {
                dir_path: tmp.path().join(&project_id).to_string_lossy().into_owned(),
                project_id,
                parent_id,
                name: name.into(),
                description: String::new(),
                template: "custom".into(),
                modules_json: "[\"tasks\"]".into(),
                owner_user_id: "owner".into(),
                key_prefix: String::new(),
                is_private: false,
                inherit_modules: false,
                inherit_task_types: false,
                status: "active".into(),
                lifecycle: "active".into(),
                functions: super::super::models::default_project_functions(),
                members: Vec::new(),
            }
        };
        let nodes = super::super::repository::prepare_project_tree_import(
            &operation,
            &org,
            &"a".repeat(64),
            &[
                node(root.clone(), None, "Recovered import root"),
                node(child.clone(), Some(root.clone()), "Recovered import child"),
            ],
        )
        .expect("whole-tree durable reservations");
        std::fs::create_dir_all(&nodes[0].dir_path).expect("first node staging directory");
        let (content, _) = super::super::project_db::open_pool_at(Path::new(&nodes[0].dir_path))
            .expect("first node staged");
        drop(content);
        std::fs::create_dir_all(&nodes[1].dir_path).expect("second node staging directory");
        std::fs::write(
            Path::new(&nodes[1].dir_path).join("project.db"),
            b"invalid sqlite content",
        )
        .expect("interrupted invalid second node");
        assert!(super::super::project_db::open_pool_at(Path::new(&nodes[1].dir_path)).is_err());
        assert!(super::super::repository::get_project(&org, &root)
            .expect("root")
            .is_none());
        assert!(super::super::repository::get_project(&org, &child)
            .expect("child")
            .is_none());
        recover_operations(&state.db).expect("restart aborts unpublished tree as one operation");
        assert!(nodes.iter().all(|node| !Path::new(&node.dir_path).exists()));
        assert!(!super::super::repository::pending_project_tree_imports()
            .expect("pending journals")
            .iter()
            .any(|pending| pending.operation_id == operation));
        assert!(super::super::repository::get_project(&org, &root)
            .expect("root")
            .is_none());
        assert!(super::super::repository::get_project(&org, &child)
            .expect("child")
            .is_none());
    }

    /// Builds a minimal archive on disk WITHOUT the async job machinery, so the
    /// extraction guards can be exercised directly.
    fn zip_with(entries: &[(&str, &[u8])], symlink: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("archive.zip");
        let file = std::fs::File::create(&path).expect("create");
        let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
        let mut files = Vec::new();
        for (name, bytes) in entries {
            zip.start_file(name.to_string(), stored_options())
                .expect("start");
            zip.write_all(bytes).expect("write");
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            files.push(FileEntry {
                path: name.to_string(),
                size: bytes.len() as u64,
                sha256: hex(&hasher.finalize()),
            });
        }
        if let Some(target) = symlink {
            let opts = zip::write::FileOptions::<()>::default().unix_permissions(0o120_777);
            zip.start_file("db/evil".to_string(), opts).expect("start");
            zip.write_all(target.as_bytes()).expect("write");
            files.push(FileEntry {
                path: "db/evil".to_string(),
                size: target.len() as u64,
                sha256: String::new(),
            });
        }
        let manifest = ArchiveManifest {
            nodes: Vec::new(),
            version: ARCHIVE_VERSION,
            schema_version: 1,
            exported_at: "t".to_string(),
            source_node_id: "n1".to_string(),
            project: ProjectMeta {
                project_id: "archive-node".into(),
                name: "Archive project".into(),
                key_prefix: "ARC".into(),
                template: "custom".into(),
                lifecycle: "active".into(),
                depth: 1,
                path: "/archive-node".into(),
                ..ProjectMeta::default()
            },
            options: ExportOptions::default(),
            embedding: None,
            inventory: Inventory::default(),
            files,
            functions: super::super::models::default_project_functions(),
        };
        zip.start_file(MANIFEST_ENTRY.to_string(), stored_options())
            .expect("start manifest");
        zip.write_all(&serde_json::to_vec(&manifest).expect("json"))
            .expect("write manifest");
        zip.finish().expect("finish");
        (tmp, path)
    }

    /// Core-side registry stub carrying just the two tables the embedding
    /// fingerprint is read from.
    fn core_db_with_alias(alias_model: &str, ns: Option<(u32, &str, &str, i64)>) -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "CREATE TABLE model_aliases (alias TEXT PRIMARY KEY, target_model TEXT NOT NULL);
             CREATE TABLE addon_vector_namespaces (
                org_id TEXT NOT NULL, addon_id TEXT NOT NULL, namespace TEXT NOT NULL,
                dim INTEGER NOT NULL, metric TEXT NOT NULL, count INTEGER NOT NULL DEFAULT 0,
                fields_json TEXT NOT NULL DEFAULT '[]');",
        )
        .expect("schema");
        conn.execute(
            "INSERT INTO model_aliases (alias, target_model) VALUES (?1, ?2)",
            params![super::super::ingest::EMBEDDINGS_ALIAS, alias_model],
        )
        .expect("alias");
        if let Some((dim, metric, fields, count)) = ns {
            conn.execute(
                "INSERT INTO addon_vector_namespaces (org_id, addon_id, namespace, dim, metric, \
                    count, fields_json) VALUES ('org-t', ?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    super::super::ingest::vector_scope("p-vec"),
                    super::super::ingest::VECTOR_NAMESPACE,
                    dim,
                    metric,
                    count,
                    fields
                ],
            )
            .expect("namespace row");
        }
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    /// The vector index travels ONLY when the alias resolves to the same model
    /// with the same dimension, metric and metadata schema. Everything else
    /// falls back to a re-index, because a matching dimension produced by a
    /// different model is silent garbage in retrieval.
    #[test]
    fn vector_reuse_requires_a_matching_embedding_fingerprint() {
        let fields = local_field_fingerprint();
        let base = EmbeddingMeta {
            alias: super::super::ingest::EMBEDDINGS_ALIAS.to_string(),
            target_model: "jina-embeddings-v3".to_string(),
            dim: 1024,
            metric: "cosine".to_string(),
            fields: fields.clone(),
            vector_count: 4200,
        };
        let manifest = |embedding: Option<EmbeddingMeta>| ArchiveManifest {
            nodes: Vec::new(),
            version: ARCHIVE_VERSION,
            schema_version: 1,
            exported_at: "t".to_string(),
            source_node_id: "n1".to_string(),
            project: ProjectMeta::default(),
            options: ExportOptions {
                include_vectors: true,
                ..ExportOptions::default()
            },
            embedding,
            inventory: Inventory::default(),
            files: Vec::new(),
            functions: super::super::models::default_project_functions(),
        };

        // Path A — identical fingerprint: the file is moved verbatim. The local
        // namespace row carries the same dimension the archive was built with.
        let core = core_db_with_alias("jina-embeddings-v3", Some((1024, "cosine", &fields, 10)));
        let (reusable, reason) = vectors_reusable(&core, "org-t", &manifest(Some(base.clone())));
        assert!(reusable, "identical fingerprint must reuse: {reason}");

        // Path B — same dimension, DIFFERENT model: re-index, with the reason
        // naming both models so the UI can explain itself.
        let core = core_db_with_alias("nemotron-embed", None);
        let (reusable, reason) = vectors_reusable(&core, "org-t", &manifest(Some(base.clone())));
        assert!(!reusable);
        assert!(reason.contains("nemotron-embed") && reason.contains("jina-embeddings-v3"));

        // Same alias, same model, DIFFERENT dimension: the local projects of
        // this org embed at 512, so the archived 1024-wide index would answer
        // every query from another space.
        let core = core_db_with_alias("jina-embeddings-v3", Some((512, "cosine", &fields, 7)));
        let (reusable, reason) = vectors_reusable(&core, "org-t", &manifest(Some(base.clone())));
        assert!(
            !reusable,
            "a different local dimension must force a re-index"
        );
        assert!(
            reason.contains("1024") && reason.contains("512"),
            "{reason}"
        );
        // Another org's namespaces say nothing about this one.
        assert!(vectors_reusable(&core, "org-inna", &manifest(Some(base.clone()))).0);

        // A changed metadata schema and a foreign metric are refused too.
        let core = core_db_with_alias("jina-embeddings-v3", None);
        let mut other_fields = base.clone();
        other_fields.fields = "[]".to_string();
        assert!(!vectors_reusable(&core, "org-t", &manifest(Some(other_fields))).0);
        let mut other_metric = base.clone();
        other_metric.metric = "l2".to_string();
        assert!(!vectors_reusable(&core, "org-t", &manifest(Some(other_metric))).0);

        // An archive without vectors, or with an empty index, has nothing to move.
        assert!(!vectors_reusable(&core, "org-t", &manifest(None)).0);
        let mut empty = base.clone();
        empty.vector_count = 0;
        assert!(!vectors_reusable(&core, "org-t", &manifest(Some(empty))).0);

        // An alias that does not exist locally cannot be matched.
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        conn.execute_batch(
            "CREATE TABLE model_aliases (alias TEXT PRIMARY KEY, target_model TEXT NOT NULL);",
        )
        .expect("schema");
        let core: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let (reusable, reason) = vectors_reusable(&core, "org-t", &manifest(Some(base)));
        assert!(!reusable);
        assert!(reason.contains("nie jest skonfigurowany"));
    }

    /// The snapshot is what travels, so it is what must be clean: encrypted
    /// secrets blanked (the key is per node — a copy is useless and dangerous)
    /// and every environment back to 'pending', so an import cannot resurrect an
    /// approved private target.
    #[test]
    fn database_snapshot_strips_secrets_and_approvals() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).expect("dir");
        let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("open");
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO environments (environment_id, name, base_url, secret_enc, \
                    approval_status, approval_reason, decided_by, is_private_address, \
                    requested_by) VALUES ('e1', 'lan', 'http://10.0.0.5', 'enc:v1:tajne', \
                    'approved', 'zgoda', 'admin-1', 1, 'u1')",
                [],
            )
            .expect("insert env");
            conn.execute(
                "INSERT INTO sources (source_id, kind, name, secret_enc, created_by) \
                 VALUES ('s1', 'git', 'Repo', 'enc:v1:ghp_token', 'u1')",
                [],
            )
            .expect("insert source");
            conn.execute(
                "INSERT INTO test_runs (run_id, run_no, name, assignment_mode, created_by) \
                 VALUES ('r1', 1, 'przebieg', 'pool', 'u1')",
                [],
            )
            .expect("insert run");
            conn.execute(
                "INSERT INTO schedules (schedule_id, name, enabled, run_type, case_ids_json, \
                    environment_id, schedule_kind, schedule_expr, next_run_at, last_run_id, \
                    last_status, last_trigger_at, consecutive_failures, created_by) \
                 VALUES ('sc1', 'nocny', 1, 'manual', '[\"c1\"]', 'e1', 'interval', '30m', \
                    '2020-01-01T00:00:00Z', 'r1', 'started', '2020-01-01T00:00:00Z', 3, 'u1')",
                [],
            )
            .expect("insert schedule");
        }

        let dest = tmp.path().join("snapshot.db");
        snapshot_database(
            &pool,
            &dest,
            &ExportOptions {
                include_runs: false,
                ..ExportOptions::default()
            },
        )
        .expect("snapshot");

        let copy = rusqlite::Connection::open(&dest).expect("open snapshot");
        let (secret, status, reason, decided): (String, String, String, String) = copy
            .query_row(
                "SELECT secret_enc, approval_status, approval_reason, decided_by \
                 FROM environments WHERE environment_id = 'e1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("env row");
        assert_eq!(secret, "", "the environment secret must not travel");
        assert_eq!(status, "pending", "an approval never travels");
        assert!(reason.is_empty() && decided.is_empty());
        let source_secret: String = copy
            .query_row(
                "SELECT secret_enc FROM sources WHERE source_id = 's1'",
                [],
                |r| r.get(0),
            )
            .expect("source row");
        assert_eq!(source_secret, "", "the source token must not travel");
        let runs: i64 = copy
            .query_row("SELECT COUNT(*) FROM test_runs", [], |r| r.get(0))
            .expect("runs");
        assert_eq!(runs, 0, "include_runs = false drops the run history");

        // A schedule travels switched OFF with no fire instant and no history
        // pointers: on the importing node it would otherwise fire immediately,
        // against an environment and a runner that do not exist there.
        let (enabled, disabled, next, last_run, last_status, failures): (
            i64,
            i64,
            Option<String>,
            String,
            String,
            i64,
        ) = copy
            .query_row(
                "SELECT enabled, auto_disabled, next_run_at, last_run_id, last_status, \
                    consecutive_failures FROM schedules WHERE schedule_id = 'sc1'",
                [],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .expect("schedule row");
        assert_eq!(enabled, 0, "an imported schedule must not fire by itself");
        assert_eq!(disabled, 0, "and it is not reported as self-stopped either");
        assert!(next.is_none());
        assert!(last_run.is_empty() && last_status.is_empty());
        assert_eq!(failures, 0);

        // The SOURCE schedule keeps running — the snapshot is a copy.
        let live_enabled: i64 = pool
            .read()
            .expect("read")
            .query_row(
                "SELECT enabled FROM schedules WHERE schedule_id = 'sc1'",
                [],
                |r| r.get(0),
            )
            .expect("live schedule");
        assert_eq!(live_enabled, 1);

        // The SOURCE database is untouched — the snapshot is a copy, not an edit.
        let live: String = pool
            .read()
            .expect("read")
            .query_row(
                "SELECT secret_enc FROM environments WHERE environment_id = 'e1'",
                [],
                |r| r.get(0),
            )
            .expect("live env");
        assert_eq!(live, "enc:v1:tajne");
    }

    /// Full round trip in a temp tree: export a real project, import it back as
    /// a new one, and check what the import is REQUIRED to change — a fresh
    /// owner, re-mapped authorship, cleared open assignments, no ML links, and
    /// the import provenance recorded in settings.
    #[tokio::test]
    async fn export_import_round_trip_rebuilds_the_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let _ = super::super::db::init(&tmp.path().join("projects.db"));
        let state = crate::dispatch::state::AppState::for_test();
        crate::dispatch::app_gate::test_support::install_app(
            &state,
            "projekty",
            &["project_studio.read"],
        );
        let owner = crate::db::repository::create_user_account(
            &state.db,
            "archive-source-owner",
            "hash",
            "Archive owner",
            "owner@example.test",
        )
        .expect("owner account");
        let importer = crate::db::repository::create_user_account(
            &state.db,
            "archive-importer",
            "hash",
            "Archive importer",
            "importer@example.test",
        )
        .expect("importer account");
        let target_org = crate::services::org::DEFAULT_ORG_ID;
        crate::services::org::add_membership(
            &state.db,
            target_org,
            &importer,
            "role-org-admin",
            "test",
        )
        .expect("importer organization");
        let context = crate::dispatch::HandlerContext {
            session: tentaflow_protocol::SessionAuth::Anonymous,
            correlation_id: 0,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: Some(
                crate::services::rbac::resolve_org_context(&state.db, &importer, Some(target_org))
                    .expect("importer access"),
            ),
            origin: crate::dispatch::RequestOrigin::Local,
        };

        let source_id = format!("src-{}", uuid::Uuid::new_v4());
        let dir = tmp.path().join("source-project");
        std::fs::create_dir_all(dir.join("files")).expect("files dir");
        {
            let (pool, _) = super::super::project_db::open_pool_at(&dir).expect("open");
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO test_cases (case_id, kind, title, content_json, status, created_by) \
                 VALUES ('c1', 'manual', 'Logowanie', '{}', 'approved', 'autor-obcy')",
                [],
            )
            .expect("case");
            conn.execute(
                "INSERT INTO tasks (task_id, task_no, task_key, task_type, title, status, assigned_to, created_by) \
                 VALUES ('t1', 1, 'PR-1', 'technical', 'Otwarte', 'todo', 'obcy-1', 'autor-obcy')",
                [],
            )
            .expect("open task");
            conn.execute(
                "INSERT INTO tasks (task_id, task_no, task_key, task_type, title, status, assigned_to, created_by) \
                 VALUES ('t2', 2, 'PR-2', 'technical', 'Zamkniete', 'done', 'obcy-2', 'autor-obcy')",
                [],
            )
            .expect("done task");
            conn.execute(
                "INSERT INTO environments (environment_id, name, base_url, secret_enc, \
                    approval_status, requested_by) VALUES ('e1', 'staging', \
                    'https://staging.example', 'enc:v1:tajne', 'approved', 'autor-obcy')",
                [],
            )
            .expect("env");
            conn.execute(
                "INSERT INTO ml_links (link_id, ml_project_id, origin, created_by) \
                 VALUES ('l1', 'ml-obcy', 'linked_existing', 'autor-obcy')",
                [],
            )
            .expect("ml link");
            conn.execute(
                "INSERT INTO sources (source_id, kind, name, created_by) \
                 VALUES (?1, 'document', 'Specyfikacja', 'autor-obcy')",
                params![source_id],
            )
            .expect("source");
        }
        // One knowledge blob, addressed by content hash exactly like ingest does.
        let blob = b"tresc dokumentu";
        let sha = {
            let mut hasher = Sha256::new();
            hasher.update(blob);
            hex(&hasher.finalize())
        };
        std::fs::write(dir.join("files").join(&sha), blob).expect("blob");
        let (source_pool, _) = super::super::project_db::open_pool_at(&dir).expect("source pool");
        source_pool
            .write()
            .expect("writer")
            .execute(
                "UPDATE tasks SET attachments_json=?1 WHERE task_id='t1'",
                [serde_json::to_string(&vec![
                    tentaflow_protocol::project_studio::AttachmentWire {
                        sha256: sha.clone(),
                        name: "document.txt".into(),
                        size_bytes: blob.len() as u64,
                        mime: "text/plain".into(),
                    },
                ])
                .expect("attachment")],
            )
            .expect("referenced blob");

        let project_id = format!("exp-{}", uuid::Uuid::new_v4());
        super::super::repository::create_project(
            &project_id,
            "org-t",
            &format!("Projekt {project_id}"),
            "opis",
            "tests",
            "[\"knowledge\",\"tests\"]",
            &owner,
            &dir.to_string_lossy(),
            "",
            None,
            false,
            false,
            false,
            &[],
        )
        .expect("registry row");
        let mut developer = super::super::repository::list_functions(&project_id)
            .expect("catalogue")
            .into_iter()
            .find(|function| function.function_id == "developer")
            .expect("developer");
        developer.name = "Engineering".to_string();
        developer
            .grants
            .iter_mut()
            .find(|grant| {
                grant.area == tentaflow_protocol::project_studio::access::ProjectArea::Knowledge
            })
            .expect("knowledge grant")
            .level = tentaflow_protocol::project_studio::access::ProjectPermissionLevel::Read;
        super::super::repository::save_function(&project_id, &developer)
            .expect("customised builtin");
        let mut external = developer.clone();
        external.function_id = "external_reviewer".to_string();
        external.name = "External reviewer".to_string();
        external.builtin = false;
        super::super::repository::save_function(&project_id, &external).expect("custom function");

        let export_ref = format!("psexp_{}", uuid::Uuid::new_v4());
        let dest = tmp.path().join(format!("{export_ref}.zip"));
        let source_record = super::super::repository::get_project("org-t", &project_id)
            .expect("registry")
            .expect("source");
        let task = ExportTask {
            nodes: vec![source_record.clone()],
            scope: tentaflow_protocol::project_studio::ProjectTreeScope::Node,
            core_db: state.db.clone(),
            org_id: "org-t".to_string(),
            user_id: owner.clone(),
            node_id: "node-a".to_string(),
            project_id: project_id.clone(),
            dir_path: dir.clone(),
            project: project_meta(&source_record).expect("source metadata"),
            options: ExportOptions {
                include_runs: false,
                include_vectors: false,
                include_user_names: true,
            },
            export_ref: export_ref.clone(),
            dest_zip: dest.clone(),
        };
        let tx = log_bus::sender_for("test-export");
        let (bytes, inventory) = build_export("test-export", &tx, &task).expect("export");
        assert!(bytes > 0 && dest.is_file());
        assert_eq!(inventory.cases, 1);
        assert_eq!(inventory.tasks, 2);
        assert_eq!(inventory.sources, 1);
        assert_eq!(inventory.files, 1, "the knowledge blob travels");

        let manifest = read_manifest(&dest).expect("manifest");
        assert_eq!(manifest.project.name, format!("Projekt {project_id}"));
        assert!(manifest
            .files
            .iter()
            .any(|f| f.path == format!("files/{sha}")));
        assert!(manifest
            .files
            .iter()
            .any(|f| f.path == "db/user_names.json"));

        // --- import as a brand-new project on the "target" node ---
        let denial = crate::dispatch::project_studio::require_import_grant(&context).expect_err(
            "readable archive and organization membership do not grant project creation",
        );
        assert_eq!(
            denial.code,
            tentaflow_protocol::ProtocolErrorCode::PolicyDenied
        );
        assert_eq!(
            denial.message,
            "importing a project requires a creator grant"
        );
        super::super::repository::set_creator_grant(&importer, target_org, &importer, true)
            .expect("actual project creator grant");
        crate::dispatch::project_studio::require_import_grant(&context)
            .expect("importer now has the current creator grant");
        let import = ImportTask {
            context,
            core_db: state.db.clone(),
            router: state.router.clone(),
            org_id: target_org.into(),
            user_id: importer.clone(),
            archive_path: dest.clone(),
            manifest,
            name_override: String::new(),
            import_vectors: false,
            import_runs: false,
        };
        let import_id = uuid::Uuid::new_v4().to_string();
        set_job(
            &import_id,
            ArchiveJob {
                owner_user_id: importer.clone(),
                ..ArchiveJob::default()
            },
        );
        run_import(&import_id, &log_bus::sender_for(&import_id), &import).expect("import");
        let target_id = job(&import_id).expect("actual import job").project_id;
        assert!(!target_id.is_empty());
        let target_dir = super::super::project_dir(&target_id);

        // The registry row belongs to the IMPORTER and their org, never to the
        // identity recorded in the archive.
        let record = super::super::repository::get_project(target_org, &target_id)
            .expect("get")
            .expect("row");
        assert_eq!(record.owner_user_id, importer);
        assert_eq!(record.org_id, target_org);
        assert_eq!(record.template, "tests");
        let cloned_ids: Vec<String> = super::super::project_db::open(&target_id)
            .expect("clone pool")
            .read()
            .expect("clone reader")
            .prepare("SELECT task_id FROM tasks ORDER BY task_no")
            .expect("query")
            .query_map([], |row| row.get(0))
            .expect("rows")
            .collect::<rusqlite::Result<_>>()
            .expect("identities");
        assert!(
            cloned_ids.iter().all(|id| id != "t1" && id != "t2"),
            "import clones task UUIDs instead of redirecting source tasks"
        );
        let membership = super::super::repository::member_access(&target_id, &importer)
            .expect("membership")
            .expect("owner member");
        assert!(membership.project_admin);
        let archived_function_ids: HashSet<_> = import
            .manifest
            .functions
            .iter()
            .map(|function| function.function_id.clone())
            .collect();
        assert!(archived_function_ids.contains("external_reviewer"));
        assert_eq!(
            membership.functions.iter().cloned().collect::<HashSet<_>>(),
            archived_function_ids,
            "the importer receives exactly the archived catalogue, including the custom function"
        );
        assert!(membership.expires_at.is_none());
        assert_eq!(
            super::super::repository::list_members(&target_id)
                .expect("members")
                .len(),
            1
        );
        assert_eq!(
            super::super::repository::list_functions(&target_id).expect("restored catalogue"),
            import.manifest.functions
        );

        let (pool, _) = super::super::project_db::open_pool_at(&target_dir).expect("open target");
        let conn = pool.read().expect("read");
        let case_author: String = conn
            .query_row(
                "SELECT created_by FROM test_cases WHERE case_id = 'c1'",
                [],
                |r| r.get(0),
            )
            .expect("case");
        assert_eq!(case_author, importer, "authorship is re-mapped");
        let open_assignee: String = conn
            .query_row("SELECT assigned_to FROM tasks WHERE task_no = 1", [], |r| {
                r.get(0)
            })
            .expect("open task");
        assert_eq!(
            open_assignee, "",
            "an open assignment to a user who cannot see the project is cleared"
        );
        let done_assignee: String = conn
            .query_row("SELECT assigned_to FROM tasks WHERE task_no = 2", [], |r| {
                r.get(0)
            })
            .expect("done task");
        assert_eq!(done_assignee, "obcy-2", "history keeps its assignee");
        let (secret, status): (String, String) = conn
            .query_row(
                "SELECT secret_enc, approval_status FROM environments WHERE environment_id = 'e1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("env");
        assert_eq!(secret, "");
        assert_eq!(
            status, "pending",
            "the imported environment needs re-approval"
        );
        let links: i64 = conn
            .query_row("SELECT COUNT(*) FROM ml_links", [], |r| r.get(0))
            .expect("links");
        assert_eq!(links, 0, "an ML link cannot follow a project across nodes");
        drop(conn);

        // The knowledge blob is on disk under the same content address.
        assert_eq!(
            std::fs::read(target_dir.join("files").join(&sha)).expect("blob"),
            blob
        );
        // Provenance is recorded for the UI and for the re-index decision.
        let imported_from = super::super::repository::get_setting(&pool, "imported_from")
            .expect("setting")
            .expect("value");
        assert!(imported_from.contains("node-a"));
        let vectors = super::super::repository::get_setting(&pool, "imported_vectors")
            .expect("setting")
            .expect("value");
        assert!(vectors.contains("\"reused\":false"));
        assert!(
            super::super::repository::get_setting(&pool, "imported_user_display_names")
                .expect("setting")
                .is_some()
        );
    }

    /// Registry fields of a manifest are validated, not copied: a template or
    /// module this node does not know is refused outright (the value reaches a
    /// registry row and, for the template, a code path keyed by it), and the
    /// free-text description is bounded and de-duplicated.
    #[test]
    fn manifest_registry_metadata_is_validated() {
        let meta = |template: &str, modules: &[&str]| ProjectMeta {
            name: "Projekt".to_string(),
            description: format!("  {}  ", "o".repeat(5_000)),
            template: template.to_string(),
            modules: modules.iter().map(|m| m.to_string()).collect(),
            key_prefix: "ARC".into(),
            lifecycle: "active".into(),
            ..ProjectMeta::default()
        };

        let (description, template, modules) =
            validated_registry_meta(&meta("tests", &["tests", "knowledge", "tests"]))
                .expect("known template and modules");
        assert_eq!(template, "tests");
        assert_eq!(modules, vec!["tests".to_string(), "knowledge".to_string()]);
        assert_eq!(description.chars().count(), 2_000, "description is bounded");

        for bad in [
            meta("../../etc/passwd", &[]),
            meta("", &[]),
            meta("tests", &["knowledge", "wlasny-modul"]),
        ] {
            assert!(
                validated_registry_meta(&bad).is_err(),
                "accepted {} / {:?}",
                bad.template,
                bad.modules
            );
        }
    }

    /// A traversal path, a symlink entry and an entry outside the layout are all
    /// refused, and nothing is written outside the staging directory.
    #[test]
    fn extraction_refuses_hostile_entries() {
        let staging_root = tempfile::tempdir().expect("staging");

        for (label, entries, symlink) in [
            (
                "traversal",
                vec![("../../escaped.txt", b"x".as_slice())],
                None,
            ),
            (
                "outside layout",
                vec![("etc/passwd", b"x".as_slice())],
                None,
            ),
            (
                "symlink",
                vec![("db/project.db", b"x".as_slice())],
                Some("/etc/passwd"),
            ),
        ] {
            let (_tmp, path) = zip_with(&entries, symlink);
            let manifest = read_manifest(&path).expect("manifest");
            let staging = staging_root.path().join(label.replace(' ', "_"));
            std::fs::create_dir_all(&staging).expect("staging dir");
            let result = extract_all(&path, &staging, &manifest);
            assert!(result.is_err(), "{label} was accepted");
        }
        assert!(
            !staging_root
                .path()
                .parent()
                .unwrap()
                .join("escaped.txt")
                .exists(),
            "traversal wrote outside the staging directory"
        );
    }

    /// A tampered payload fails the per-file checksum, and a stripped archive
    /// fails the completeness check.
    #[test]
    fn extraction_verifies_checksums_and_completeness() {
        let (_tmp, path) = zip_with(&[("db/project.db", b"payload")], None);
        let staging_root = tempfile::tempdir().expect("staging");
        let staging = staging_root.path().join("ok");
        std::fs::create_dir_all(&staging).expect("dir");
        let manifest = read_manifest(&path).expect("manifest");
        extract_all(&path, &staging, &manifest).expect("clean archive extracts");
        assert_eq!(
            std::fs::read(staging.join("db/project.db")).expect("read"),
            b"payload"
        );

        // Same archive, manifest claiming a different hash.
        let mut tampered = manifest.clone();
        tampered.files[0].sha256 = "0".repeat(64);
        let staging = staging_root.path().join("tampered");
        std::fs::create_dir_all(&staging).expect("dir");
        assert!(extract_all(&path, &staging, &tampered).is_err());

        // Manifest declaring a file the archive does not carry.
        let mut incomplete = manifest.clone();
        incomplete.files.push(FileEntry {
            path: "files/missing".to_string(),
            size: 1,
            sha256: "0".repeat(64),
        });
        let staging = staging_root.path().join("incomplete");
        std::fs::create_dir_all(&staging).expect("dir");
        assert!(extract_all(&path, &staging, &incomplete).is_err());
    }

    /// An archive that declares more bytes than the cap is refused BEFORE a
    /// single entry is written.
    #[test]
    fn extraction_refuses_a_zip_bomb() {
        let (_tmp, path) = zip_with(&[("db/project.db", b"x")], None);
        let mut manifest = read_manifest(&path).expect("manifest");
        manifest.files[0].size = MAX_IMPORT_BYTES + 1;
        let staging_root = tempfile::tempdir().expect("staging");
        let err = extract_all(&path, staging_root.path(), &manifest)
            .expect_err("declared size over the cap must be refused");
        assert!(err.to_string().contains("limit"));
        assert!(
            !staging_root.path().join("db").exists(),
            "nothing is written before the budget check"
        );
    }

    /// An archive from a newer schema is refused at manifest level — before
    /// anything is unpacked.
    #[test]
    fn manifest_refuses_a_newer_schema() {
        let (_tmp, path) = zip_with(&[("db/project.db", b"x")], None);
        let mut manifest = read_manifest(&path).expect("manifest");
        assert_eq!(manifest.version, ARCHIVE_VERSION);

        // Rewrite the archive with a schema this binary cannot understand.
        manifest.schema_version = super::super::project_db::LATEST_SCHEMA_VERSION + 1;
        let tmp = tempfile::tempdir().expect("tempdir");
        let newer = tmp.path().join("newer.zip");
        {
            let file = std::fs::File::create(&newer).expect("create");
            let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
            zip.start_file("db/project.db".to_string(), stored_options())
                .expect("start");
            zip.write_all(b"x").expect("write");
            zip.start_file(MANIFEST_ENTRY.to_string(), stored_options())
                .expect("start manifest");
            zip.write_all(&serde_json::to_vec(&manifest).expect("json"))
                .expect("write");
            zip.finish().expect("finish");
        }
        let err = read_manifest(&newer).expect_err("newer schema must be refused");
        assert!(err.to_string().contains("nowszej"));
    }
}
