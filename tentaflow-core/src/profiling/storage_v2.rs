// =============================================================================
// File: profiling/storage_v2.rs — multi-source profiling session storage (v2)
// =============================================================================
//
// Layout:
//   <TENTAFLOW_HOME>/profiling/<node_id>/<session_id>/
//   ├── manifest.json     (serde JSON, operator-readable)
//   ├── summary.bin       (rkyv ProfileReportEnvelope::V2(ProfileReportV2))
//   ├── flamegraph.bin    (optional, side-data — owned by CPU sampling parser)
//   └── raw/<collector_id>/<artifact files...>
//
// Legacy nsight sessions live under <TENTAFLOW_HOME>/nsight/<node_id>/<session_id>/
// with `summary.bin` (rkyv ProfileReport) + `report.nsys-rep`. The `migrate_*`
// helpers convert each legacy session into the new layout idempotently.
//
// Path traversal is rejected at every entry point: node_id, session_id and
// collector_id are validated against strict character-class regexes before any
// filesystem operation. Recursive size accounting underpins the per-session
// 1 GiB cap and the 20-sessions-per-node FIFO.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use tentaflow_protocol::profiling::{
    validate_collector_id, NsightScope, ProfileReport, ProfileReportEnvelope, ProfileReportV2,
    ProfileScope,
};
use tokio::fs;

// -----------------------------------------------------------------------------
// Constants & validators
// -----------------------------------------------------------------------------

/// FIFO retention per node — newest N sessions are kept, the rest deleted.
pub const DEFAULT_FIFO_LIMIT: usize = 20;

/// Per-session disk budget: 1 GiB. Enforced after `write_session` plus on demand.
pub const DEFAULT_PER_SESSION_SIZE_CAP: u64 = 1u64 << 30;

/// Schema version embedded in `SessionManifest`.
pub const MANIFEST_SCHEMA_VERSION: u32 = 2;

static SESSION_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-f0-9]{16,32}$").expect("valid session id regex"));

static NODE_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{1,64}$").expect("valid node id regex"));

fn check_session_id(s: &str) -> Result<(), StorageError> {
    if SESSION_ID_RE.is_match(s) {
        Ok(())
    } else {
        Err(StorageError::InvalidSessionId(s.to_string()))
    }
}

fn check_node_id(s: &str) -> Result<(), StorageError> {
    if NODE_ID_RE.is_match(s) {
        Ok(())
    } else {
        Err(StorageError::InvalidNodeId(s.to_string()))
    }
}

fn check_collector_id(s: &str) -> Result<(), StorageError> {
    validate_collector_id(s).map_err(|_| StorageError::InvalidCollectorId(s.to_string()))
}

// -----------------------------------------------------------------------------
// Error type
// -----------------------------------------------------------------------------

#[derive(thiserror::Error, Debug)]
pub enum StorageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid session id: {0}")]
    InvalidSessionId(String),
    #[error("invalid node id: {0}")]
    InvalidNodeId(String),
    #[error("invalid collector id: {0}")]
    InvalidCollectorId(String),
    #[error("manifest parse: {0}")]
    ManifestParse(String),
    #[error("rkyv: {0}")]
    Rkyv(String),
    #[error("session size cap exceeded: {actual} > {cap}")]
    SizeCapExceeded { actual: u64, cap: u64 },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("path traversal attempt: {0}")]
    PathTraversal(String),
}

// -----------------------------------------------------------------------------
// Manifest types (serde JSON, NOT rkyv)
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    MultiSource,
    LegacyNsight,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SkippedCollector {
    pub id: String,
    pub reason: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionManifest {
    pub schema_version: u32,
    pub session_id: String,
    pub label: String,
    pub node_id: String,
    pub kind: SessionKind,
    /// RFC3339 (UTC).
    pub started_at: String,
    pub duration_ns: u64,
    /// Free-form serialization of `ProfileScope` to keep manifest forward-compatible.
    pub scope: serde_json::Value,
    pub collectors_used: Vec<String>,
    pub collectors_skipped: Vec<SkippedCollector>,
    pub size_bytes: u64,
    pub warnings: Vec<String>,
}

/// Slim entry returned from `list_sessions`.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub session_id: String,
    pub label: String,
    pub started_at: String,
    pub duration_ns: u64,
    pub kind: SessionKind,
    pub collectors_used: Vec<String>,
    pub size_bytes: u64,
}

// -----------------------------------------------------------------------------
// Storage
// -----------------------------------------------------------------------------

pub struct ProfileStorageV2 {
    root: PathBuf,
    fifo_limit: usize,
    per_session_size_cap: u64,
}

impl ProfileStorageV2 {
    pub fn new(tentaflow_home: &Path) -> Self {
        Self::new_with_limits(
            tentaflow_home,
            DEFAULT_FIFO_LIMIT,
            DEFAULT_PER_SESSION_SIZE_CAP,
        )
    }

    pub fn new_with_limits(
        tentaflow_home: &Path,
        fifo_limit: usize,
        per_session_size_cap: u64,
    ) -> Self {
        Self {
            root: tentaflow_home.join("profiling"),
            fifo_limit,
            per_session_size_cap,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn fifo_limit(&self) -> usize {
        self.fifo_limit
    }
    pub fn per_session_size_cap(&self) -> u64 {
        self.per_session_size_cap
    }

    fn node_dir(&self, node_id: &str) -> Result<PathBuf, StorageError> {
        check_node_id(node_id)?;
        Ok(self.root.join(node_id))
    }

    fn session_dir_path(&self, node_id: &str, session_id: &str) -> Result<PathBuf, StorageError> {
        check_session_id(session_id)?;
        Ok(self.node_dir(node_id)?.join(session_id))
    }

    /// Returns directory for a session, creating it on demand.
    pub async fn session_dir(
        &self,
        node_id: &str,
        session_id: &str,
    ) -> Result<PathBuf, StorageError> {
        let dir = self.session_dir_path(node_id, session_id)?;
        fs::create_dir_all(&dir).await?;
        Ok(dir)
    }

    /// Returns `raw/<collector_id>/` inside the session directory, creating it.
    pub async fn collector_raw_dir(
        &self,
        node_id: &str,
        session_id: &str,
        collector_id: &str,
    ) -> Result<PathBuf, StorageError> {
        check_collector_id(collector_id)?;
        let session_dir = self.session_dir(node_id, session_id).await?;
        let raw = session_dir.join("raw").join(collector_id);
        fs::create_dir_all(&raw).await?;
        // Anti-traversal: resolved path must stay under the session dir.
        ensure_under(&session_dir, &raw).await?;
        Ok(raw)
    }

    /// Path to optional flamegraph side-table; not created on call.
    pub fn flamegraph_path(&self, node_id: &str, session_id: &str) -> PathBuf {
        // Validation deferred to actual fs operations; this method is a pure path builder.
        self.root
            .join(node_id)
            .join(session_id)
            .join("flamegraph.bin")
    }

    /// Persist manifest + summary, then compute total size and rewrite manifest.
    pub async fn write_session(
        &self,
        node_id: &str,
        session_id: &str,
        manifest: &SessionManifest,
        report: &ProfileReportV2,
    ) -> Result<(), StorageError> {
        let dir = self.session_dir(node_id, session_id).await?;

        // 1) Write summary first; manifest size_bytes will include it.
        let envelope = ProfileReportEnvelope::V2(report.clone());
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope)
            .map_err(|e| StorageError::Rkyv(format!("encode: {e}")))?;
        fs::write(dir.join("summary.bin"), bytes.as_ref()).await?;

        // 2) Write manifest with placeholder size, then update.
        let mut manifest = manifest.clone();
        manifest.schema_version = MANIFEST_SCHEMA_VERSION;
        manifest.session_id = session_id.to_string();
        manifest.node_id = node_id.to_string();
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| StorageError::ManifestParse(format!("encode: {e}")))?;
        fs::write(dir.join("manifest.json"), &json).await?;

        let total = compute_dir_size(&dir).await?;
        manifest.size_bytes = total;
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| StorageError::ManifestParse(format!("encode: {e}")))?;
        fs::write(dir.join("manifest.json"), &json).await?;

        Ok(())
    }

    /// List sessions sorted by `started_at` desc.
    pub async fn list_sessions(&self, node_id: &str) -> Result<Vec<SessionEntry>, StorageError> {
        let dir = self.node_dir(node_id)?;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out: Vec<SessionEntry> = Vec::new();
        let mut rd = fs::read_dir(&dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            if !entry.file_type().await?.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if check_session_id(&name).is_err() {
                continue;
            }
            // Skip sessions without manifest (in-progress / crashed).
            let mp = entry.path().join("manifest.json");
            if !mp.exists() {
                continue;
            }
            match self.read_manifest(node_id, &name).await {
                Ok(m) => out.push(SessionEntry {
                    session_id: m.session_id.clone(),
                    label: m.label.clone(),
                    started_at: m.started_at.clone(),
                    duration_ns: m.duration_ns,
                    kind: m.kind.clone(),
                    collectors_used: m.collectors_used.clone(),
                    size_bytes: m.size_bytes,
                }),
                Err(_) => continue,
            }
        }
        out.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        Ok(out)
    }

    pub async fn read_manifest(
        &self,
        node_id: &str,
        session_id: &str,
    ) -> Result<SessionManifest, StorageError> {
        let dir = self.session_dir_path(node_id, session_id)?;
        let p = dir.join("manifest.json");
        if !p.exists() {
            return Err(StorageError::NotFound(format!(
                "manifest.json for {session_id}"
            )));
        }
        let bytes = fs::read(&p).await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| StorageError::ManifestParse(format!("decode: {e}")))
    }

    pub async fn read_report(
        &self,
        node_id: &str,
        session_id: &str,
    ) -> Result<ProfileReportEnvelope, StorageError> {
        let dir = self.session_dir_path(node_id, session_id)?;
        let p = dir.join("summary.bin");
        if !p.exists() {
            return Err(StorageError::NotFound(format!(
                "summary.bin for {session_id}"
            )));
        }
        let bytes = fs::read(&p).await?;
        rkyv::from_bytes::<ProfileReportEnvelope, rkyv::rancor::Error>(&bytes)
            .map_err(|e| StorageError::Rkyv(format!("decode: {e}")))
    }

    /// Idempotent: delete a session directory if it exists.
    pub async fn delete_session(
        &self,
        node_id: &str,
        session_id: &str,
    ) -> Result<(), StorageError> {
        let dir = self.session_dir_path(node_id, session_id)?;
        if !dir.exists() {
            return Ok(());
        }
        // Resolve and ensure the path stays under the node dir.
        let node_dir = self.node_dir(node_id)?;
        ensure_under(&node_dir, &dir).await?;
        fs::remove_dir_all(&dir).await?;
        Ok(())
    }

    /// Keep newest `fifo_limit` sessions per node; delete older ones.
    /// Returns count of deleted sessions.
    pub async fn enforce_fifo(&self, node_id: &str) -> Result<usize, StorageError> {
        let entries = self.list_sessions(node_id).await?;
        if entries.len() <= self.fifo_limit {
            return Ok(0);
        }
        let mut deleted = 0usize;
        for old in entries.iter().skip(self.fifo_limit) {
            if self.delete_session(node_id, &old.session_id).await.is_ok() {
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    pub async fn compute_session_size(
        &self,
        node_id: &str,
        session_id: &str,
    ) -> Result<u64, StorageError> {
        let dir = self.session_dir_path(node_id, session_id)?;
        if !dir.exists() {
            return Err(StorageError::NotFound(session_id.to_string()));
        }
        compute_dir_size(&dir).await
    }

    /// Drops largest `raw/<collector>/` subdirectories until the session fits the cap.
    /// Whole subdirs are removed (raw artifacts cannot be safely truncated mid-file).
    pub async fn enforce_size_cap(
        &self,
        node_id: &str,
        session_id: &str,
    ) -> Result<u64, StorageError> {
        let dir = self.session_dir_path(node_id, session_id)?;
        let cap = self.per_session_size_cap;
        let mut total = compute_dir_size(&dir).await?;
        if total <= cap {
            return Ok(total);
        }

        let raw_root = dir.join("raw");
        if raw_root.exists() {
            // Collect (size, path) for each collector subdir.
            let mut subdirs: Vec<(u64, PathBuf)> = Vec::new();
            let mut rd = fs::read_dir(&raw_root).await?;
            while let Some(entry) = rd.next_entry().await? {
                if entry.file_type().await?.is_dir() {
                    let p = entry.path();
                    let s = compute_dir_size(&p).await?;
                    subdirs.push((s, p));
                }
            }
            // Largest first.
            // Largest first: negate via reverse key.
            subdirs.sort_by_key(|(sz, _)| std::cmp::Reverse(*sz));
            for (sz, p) in subdirs {
                if total <= cap {
                    break;
                }
                if fs::remove_dir_all(&p).await.is_ok() {
                    total = total.saturating_sub(sz);
                }
            }
        }

        if total > cap {
            return Err(StorageError::SizeCapExceeded { actual: total, cap });
        }
        // Recompute exactly post-removal so the manifest can be refreshed by the caller.
        let exact = compute_dir_size(&dir).await?;
        Ok(exact)
    }
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

async fn compute_dir_size(dir: &Path) -> Result<u64, StorageError> {
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    let mut total: u64 = 0;
    while let Some(d) = stack.pop() {
        let mut rd = match fs::read_dir(&d).await {
            Ok(r) => r,
            // A symlink or vanished entry should not abort accounting.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(StorageError::Io(e)),
        };
        while let Some(entry) = rd.next_entry().await? {
            let md = fs::symlink_metadata(entry.path()).await?;
            let ft = md.file_type();
            if ft.is_symlink() {
                // Skip symlinks — anti-traversal guard, also prevents double counting.
                continue;
            }
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total = total.saturating_add(md.len());
            }
        }
    }
    Ok(total)
}

/// Ensure that `child` resolves to a path strictly under `parent` (anti path-traversal).
async fn ensure_under(parent: &Path, child: &Path) -> Result<(), StorageError> {
    let parent_canon = fs::canonicalize(parent).await.map_err(StorageError::Io)?;
    let child_canon = fs::canonicalize(child).await.map_err(StorageError::Io)?;
    if !child_canon.starts_with(&parent_canon) {
        return Err(StorageError::PathTraversal(
            child.to_string_lossy().into_owned(),
        ));
    }
    Ok(())
}

// -----------------------------------------------------------------------------
// Legacy nsight migration
// -----------------------------------------------------------------------------

#[derive(Debug, Default)]
pub struct MigrationReport {
    pub migrated: usize,
    pub skipped_existing: usize,
    pub failed: Vec<(String, String)>,
}

/// Heuristic: pick a `NsightScope` from a legacy `ProfileReport` if its `meta.scope`
/// happens to be the default `Cpu` (the only Default-like value in older builds).
/// Otherwise the stored scope is preserved verbatim. Presence of `gpu_*` rows hints
/// at GpuAll/BothAll regardless.
fn infer_scope(report: &ProfileReport) -> NsightScope {
    let has_gpu = !report.gpu_kernels_top.is_empty()
        || !report.gpu_mem_ops.is_empty()
        || !report.gpu_util_timeline.is_empty();
    let has_cpu = !report.cpu_samples_top.is_empty();
    match (has_cpu, has_gpu, &report.meta.scope) {
        // Trust the stored scope when it isn't the trivial default.
        (_, _, NsightScope::GpuIndex(_))
        | (_, _, NsightScope::GpuAll)
        | (_, _, NsightScope::BothIndex(_))
        | (_, _, NsightScope::BothAll) => report.meta.scope.clone(),
        (true, true, _) => NsightScope::BothAll,
        (false, true, _) => NsightScope::GpuAll,
        (true, false, _) => NsightScope::Cpu,
        (false, false, _) => report.meta.scope.clone(),
    }
}

fn ms_to_rfc3339(ms: u64) -> String {
    let secs = (ms / 1000) as i64;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    DateTime::<Utc>::from_timestamp(secs, nanos)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("epoch is valid"))
        .to_rfc3339()
}

async fn migrate_one_session(
    tentaflow_home: &Path,
    node_id: &str,
    session_id: &str,
    legacy_node_dir: &Path,
    storage: &ProfileStorageV2,
) -> Result<bool, StorageError> {
    let legacy_session_dir = legacy_node_dir.join(session_id);
    let new_session_dir = storage.root.join(node_id).join(session_id);

    if new_session_dir.exists() && new_session_dir.join("manifest.json").exists() {
        return Ok(false); // skipped_existing
    }

    // Read legacy summary.bin (rkyv ProfileReport).
    let legacy_summary = legacy_session_dir.join("summary.bin");
    let bytes = fs::read(&legacy_summary).await?;
    let legacy_report = rkyv::from_bytes::<ProfileReport, rkyv::rancor::Error>(&bytes)
        .map_err(|e| StorageError::Rkyv(format!("legacy decode: {e}")))?;

    let scope = infer_scope(&legacy_report);
    let v2_scope: ProfileScope = scope.clone().into();
    let v2_report = legacy_report.clone().into_v2(
        session_id.to_string(),
        node_id.to_string(),
        v2_scope.clone(),
    );

    // Build manifest.
    let manifest = SessionManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        session_id: session_id.to_string(),
        label: legacy_report.meta.label.clone(),
        node_id: node_id.to_string(),
        kind: SessionKind::LegacyNsight,
        started_at: ms_to_rfc3339(legacy_report.meta.started_at_ms),
        duration_ns: legacy_report.meta.duration_ms.saturating_mul(1_000_000),
        scope: serde_json::to_value(&v2_scope)
            .map_err(|e| StorageError::ManifestParse(format!("scope encode: {e}")))?,
        collectors_used: vec!["nvidia.nsys.gpu".to_string()],
        collectors_skipped: Vec::new(),
        size_bytes: 0,
        warnings: Vec::new(),
    };

    // Create target dir and raw/nvidia.nsys.gpu/.
    fs::create_dir_all(&new_session_dir).await?;
    let raw_dir = new_session_dir.join("raw").join("nvidia.nsys.gpu");
    fs::create_dir_all(&raw_dir).await?;

    // Move report.nsys-rep into raw/.
    let legacy_rep = legacy_session_dir.join("report.nsys-rep");
    if legacy_rep.exists() {
        let target = raw_dir.join("report.nsys-rep");
        // tokio::fs::rename is atomic on the same filesystem; otherwise fall back to copy+remove.
        if let Err(e) = fs::rename(&legacy_rep, &target).await {
            if matches!(
                e.kind(),
                std::io::ErrorKind::CrossesDevices | std::io::ErrorKind::Other
            ) {
                fs::copy(&legacy_rep, &target).await?;
                fs::remove_file(&legacy_rep).await?;
            } else {
                return Err(StorageError::Io(e));
            }
        }
    }

    // Write manifest + summary using the storage layer (handles size accounting).
    storage
        .write_session(node_id, session_id, &manifest, &v2_report)
        .await?;

    // Drop the now-empty legacy session dir.
    let _ = fs::remove_dir_all(&legacy_session_dir).await;
    let _ = tentaflow_home; // Reserved for future cross-checks.

    Ok(true)
}

/// Migrate all legacy nsight sessions for a given node.
pub async fn migrate_legacy_nsight_for_node(
    tentaflow_home: &Path,
    node_id: &str,
) -> Result<MigrationReport, StorageError> {
    check_node_id(node_id)?;
    let mut report = MigrationReport::default();
    let legacy_node_dir = tentaflow_home.join("nsight").join(node_id);
    if !legacy_node_dir.exists() {
        return Ok(report);
    }
    let storage = ProfileStorageV2::new(tentaflow_home);
    let mut rd = fs::read_dir(&legacy_node_dir).await?;
    while let Some(entry) = rd.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if check_session_id(&name).is_err() {
            continue;
        }
        match migrate_one_session(tentaflow_home, node_id, &name, &legacy_node_dir, &storage).await
        {
            Ok(true) => report.migrated += 1,
            Ok(false) => report.skipped_existing += 1,
            Err(e) => {
                tracing::warn!(node=%node_id, session=%name, error=%e, "legacy nsight migration failed");
                report.failed.push((name, e.to_string()));
            }
        }
    }
    Ok(report)
}

/// Migrate all nodes under `<TENTAFLOW_HOME>/nsight/`.
pub async fn migrate_legacy_nsight_all(
    tentaflow_home: &Path,
) -> Result<MigrationReport, StorageError> {
    let mut total = MigrationReport::default();
    let nsight_root = tentaflow_home.join("nsight");
    if !nsight_root.exists() {
        return Ok(total);
    }
    let mut rd = fs::read_dir(&nsight_root).await?;
    while let Some(entry) = rd.next_entry().await? {
        if !entry.file_type().await?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if check_node_id(&name).is_err() {
            continue;
        }
        match migrate_legacy_nsight_for_node(tentaflow_home, &name).await {
            Ok(r) => {
                total.migrated += r.migrated;
                total.skipped_existing += r.skipped_existing;
                total.failed.extend(r.failed);
            }
            Err(e) => {
                tracing::warn!(node=%name, error=%e, "node migration failed");
                total.failed.push((name, e.to_string()));
            }
        }
    }
    Ok(total)
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests_v2 {
    use super::*;
    use tempfile::TempDir;
    use tentaflow_protocol::profiling::{
        CollectorRunInfo, CollectorStatus, DriftReport, EventCategory, EventPayload, GpuTargets,
        GpuUtilSample, GpuUtilSeries, NsightScope, ProfileKpi, ProfileMeta, ProfileReport,
        ProfileScope, ProfileSourceFlags, ProfileTarget, ProfileTopRow, TimelineEvent,
        PROFILE_REPORT_V2_SCHEMA_VERSION,
    };

    fn make_storage() -> (TempDir, ProfileStorageV2) {
        let tmp = tempfile::tempdir().unwrap();
        let st = ProfileStorageV2::new(tmp.path());
        (tmp, st)
    }

    fn sample_scope() -> ProfileScope {
        ProfileScope {
            sources: ProfileSourceFlags(ProfileSourceFlags::GPU),
            gpu_targets: GpuTargets::All,
            cpu_sampling_hz: 99,
            target: ProfileTarget::SystemWide,
            duration_seconds: 0,
            label: "lbl".into(),
        }
    }

    fn sample_report_v2(session: &str, node: &str) -> ProfileReportV2 {
        ProfileReportV2 {
            schema_version: PROFILE_REPORT_V2_SCHEMA_VERSION,
            session_id: session.into(),
            node_id: node.into(),
            scope: sample_scope(),
            t0_monotonic_ns: 0,
            t0_wallclock_unix_ns: 0,
            duration_ns: 1_000_000,
            collectors: vec![CollectorRunInfo {
                id: "nvidia.nsys.gpu".into(),
                status: CollectorStatus::Used,
                samples_collected: 1,
                raw_size_bytes: 0,
                primary_category: EventCategory::GpuKernel,
                duration_ns: 1_000_000,
            }],
            events: vec![TimelineEvent {
                source_idx: 0,
                t_start_ns: 0,
                t_end_ns: 100,
                category: EventCategory::GpuKernel,
                lane_hint: 0,
                payload: EventPayload::GpuKernel {
                    device_id: 0,
                    name_id: 0,
                    grid: [1, 1, 1],
                    block: [1, 1, 1],
                    shared_mem_bytes: 0,
                },
            }],
            frames: Vec::new(),
            stacks: Vec::new(),
            names: vec!["k".into()],
            drift_report: DriftReport::empty(),
            warnings: Vec::new(),
        }
    }

    fn sample_manifest(
        session: &str,
        node: &str,
        started_at: &str,
        kind: SessionKind,
    ) -> SessionManifest {
        SessionManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            session_id: session.into(),
            label: "label".into(),
            node_id: node.into(),
            kind,
            started_at: started_at.into(),
            duration_ns: 1_000_000,
            scope: serde_json::to_value(sample_scope()).unwrap(),
            collectors_used: vec!["nvidia.nsys.gpu".into()],
            collectors_skipped: Vec::new(),
            size_bytes: 0,
            warnings: Vec::new(),
        }
    }

    // 1
    #[tokio::test]
    async fn session_dir_creates_validated() {
        let (_tmp, st) = make_storage();
        let p = st.session_dir("node-1", "deadbeefdeadbeef").await.unwrap();
        assert!(p.exists());
        assert!(p.ends_with("profiling/node-1/deadbeefdeadbeef"));
    }

    // 2
    #[tokio::test]
    async fn session_dir_rejects_bad_session_id() {
        let (_tmp, st) = make_storage();
        let err = st.session_dir("node-1", "bad-id").await.unwrap_err();
        assert!(matches!(err, StorageError::InvalidSessionId(_)));
    }

    // 3
    #[tokio::test]
    async fn session_dir_rejects_path_traversal_node() {
        let (_tmp, st) = make_storage();
        let err = st
            .session_dir("../../../etc", "deadbeefdeadbeef")
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::InvalidNodeId(_)));
    }

    // 4
    #[tokio::test]
    async fn collector_raw_dir_creates_subdir() {
        let (_tmp, st) = make_storage();
        let p = st
            .collector_raw_dir("node-1", "deadbeefdeadbeef", "nvidia.nsys.gpu")
            .await
            .unwrap();
        assert!(p.exists());
        assert!(p.ends_with("raw/nvidia.nsys.gpu"));
    }

    // 5
    #[tokio::test]
    async fn write_and_read_session_round_trip() {
        let (_tmp, st) = make_storage();
        let sid = "deadbeefdeadbeef";
        let node = "node-1";
        let report = sample_report_v2(sid, node);
        let manifest = sample_manifest(sid, node, "2026-04-28T10:00:00Z", SessionKind::MultiSource);
        st.write_session(node, sid, &manifest, &report)
            .await
            .unwrap();
        let m = st.read_manifest(node, sid).await.unwrap();
        assert_eq!(m.session_id, sid);
        assert_eq!(m.kind, SessionKind::MultiSource);
        assert!(m.size_bytes > 0);
        let env = st.read_report(node, sid).await.unwrap();
        match env {
            ProfileReportEnvelope::V2(r) => {
                assert_eq!(r.session_id, sid);
                assert_eq!(r.events.len(), 1);
            }
            _ => panic!("expected V2"),
        }
    }

    // 6
    #[tokio::test]
    async fn list_sessions_returns_sorted_desc() {
        let (_tmp, st) = make_storage();
        let node = "node-1";
        let triples = [
            ("aaaaaaaaaaaaaaaa", "2026-04-28T10:00:00Z"),
            ("bbbbbbbbbbbbbbbb", "2026-04-28T12:00:00Z"),
            ("cccccccccccccccc", "2026-04-28T11:00:00Z"),
        ];
        for (sid, ts) in &triples {
            let r = sample_report_v2(sid, node);
            let m = sample_manifest(sid, node, ts, SessionKind::MultiSource);
            st.write_session(node, sid, &m, &r).await.unwrap();
        }
        let list = st.list_sessions(node).await.unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].session_id, "bbbbbbbbbbbbbbbb");
        assert_eq!(list[1].session_id, "cccccccccccccccc");
        assert_eq!(list[2].session_id, "aaaaaaaaaaaaaaaa");
    }

    // 7
    #[tokio::test]
    async fn delete_session_idempotent() {
        let (_tmp, st) = make_storage();
        let sid = "deadbeefdeadbeef";
        let node = "node-1";
        st.session_dir(node, sid).await.unwrap();
        st.delete_session(node, sid).await.unwrap();
        st.delete_session(node, sid).await.unwrap();
    }

    // 8
    #[tokio::test]
    async fn enforce_fifo_keeps_newest_n() {
        let tmp = tempfile::tempdir().unwrap();
        let st = ProfileStorageV2::new_with_limits(tmp.path(), 20, DEFAULT_PER_SESSION_SIZE_CAP);
        let node = "node-1";
        for i in 0..25u32 {
            // 16 hex chars; encode i in last 4 to keep them unique.
            let sid = format!("{:0>12x}{:0>4x}", i, i);
            let ts = format!("2026-04-28T10:{:02}:00Z", i);
            let r = sample_report_v2(&sid, node);
            let m = sample_manifest(&sid, node, &ts, SessionKind::MultiSource);
            st.write_session(node, &sid, &m, &r).await.unwrap();
        }
        assert_eq!(st.list_sessions(node).await.unwrap().len(), 25);
        let deleted = st.enforce_fifo(node).await.unwrap();
        assert_eq!(deleted, 5);
        assert_eq!(st.list_sessions(node).await.unwrap().len(), 20);
    }

    // 9
    #[tokio::test]
    async fn compute_session_size_recursive() {
        let (_tmp, st) = make_storage();
        let node = "node-1";
        let sid = "deadbeefdeadbeef";
        let raw_a = st
            .collector_raw_dir(node, sid, "nvidia.nsys.gpu")
            .await
            .unwrap();
        let raw_b = st
            .collector_raw_dir(node, sid, "linux.proc.cpu_util")
            .await
            .unwrap();
        fs::write(raw_a.join("file1"), vec![0u8; 1000])
            .await
            .unwrap();
        fs::write(raw_b.join("file2"), vec![0u8; 500])
            .await
            .unwrap();
        let total = st.compute_session_size(node, sid).await.unwrap();
        assert_eq!(total, 1500);
    }

    // 10
    #[tokio::test]
    async fn enforce_size_cap_removes_largest_raw() {
        let tmp = tempfile::tempdir().unwrap();
        // Cap chosen so that 6 KiB + 5 KiB exceeds cap, but 5 KiB alone fits.
        let st = ProfileStorageV2::new_with_limits(tmp.path(), 20, 8 * 1024);
        let node = "node-1";
        let sid = "deadbeefdeadbeef";
        let big = st
            .collector_raw_dir(node, sid, "linux.rapl.power")
            .await
            .unwrap();
        let small = st
            .collector_raw_dir(node, sid, "nvidia.nsys.gpu")
            .await
            .unwrap();
        fs::write(big.join("big.bin"), vec![0u8; 6 * 1024])
            .await
            .unwrap();
        fs::write(small.join("small.bin"), vec![0u8; 5 * 1024])
            .await
            .unwrap();
        let new_total = st.enforce_size_cap(node, sid).await.unwrap();
        assert!(new_total <= 8 * 1024);
        assert!(!big.exists(), "largest raw subdir must be removed");
        assert!(small.exists(), "smaller raw subdir must remain");
    }

    // 11
    #[tokio::test]
    async fn enforce_size_cap_returns_err_if_still_over() {
        let tmp = tempfile::tempdir().unwrap();
        let st = ProfileStorageV2::new_with_limits(tmp.path(), 20, 1024);
        let node = "node-1";
        let sid = "deadbeefdeadbeef";
        // Place a huge file directly inside session dir (NOT in raw/), so size cap cannot be reduced.
        let dir = st.session_dir(node, sid).await.unwrap();
        fs::write(dir.join("summary.bin"), vec![0u8; 4096])
            .await
            .unwrap();
        let err = st.enforce_size_cap(node, sid).await.unwrap_err();
        assert!(matches!(err, StorageError::SizeCapExceeded { .. }));
    }

    // 12
    #[tokio::test]
    async fn read_report_v2_roundtrip() {
        let (_tmp, st) = make_storage();
        let sid = "deadbeefdeadbeef";
        let node = "node-1";
        let r = sample_report_v2(sid, node);
        let m = sample_manifest(sid, node, "2026-04-28T10:00:00Z", SessionKind::MultiSource);
        st.write_session(node, sid, &m, &r).await.unwrap();
        let env = st.read_report(node, sid).await.unwrap();
        assert!(matches!(env, ProfileReportEnvelope::V2(_)));
    }

    fn legacy_dummy_report(session: &str) -> ProfileReport {
        ProfileReport {
            meta: ProfileMeta {
                session_id: session.into(),
                label: "legacy".into(),
                scope: NsightScope::GpuAll,
                hostname: "h".into(),
                started_at_ms: 1_700_000_000_000,
                duration_ms: 250,
                nsys_version: "2025.1".into(),
                gpu_targets: Vec::new(),
            },
            kpi: ProfileKpi::default(),
            gpu_kernels_top: vec![ProfileTopRow {
                name: "kernel_a".into(),
                total_ms: 1.5,
                calls: 3,
                avg_ms: 0.5,
                pct: 100.0,
            }],
            cuda_api_top: Vec::new(),
            gpu_mem_ops: Vec::new(),
            cpu_samples_top: Vec::new(),
            nvtx_ranges_top: Vec::new(),
            gpu_util_timeline: vec![GpuUtilSeries {
                gpu_idx: 0,
                power_limit_w: 350.0,
                samples: vec![GpuUtilSample {
                    t_ms: 0,
                    sm_pct: 50,
                    mem_pct: 30,
                    vram_used_mb: 1024,
                    power_w: 100.0,
                }],
            }],
        }
    }

    // 13
    #[tokio::test]
    async fn read_report_legacy_compat() {
        let (_tmp, st) = make_storage();
        let sid = "deadbeefdeadbeef";
        let node = "node-1";
        // Manually craft a legacy envelope and write it.
        let dir = st.session_dir(node, sid).await.unwrap();
        let envelope = ProfileReportEnvelope::V1Legacy(legacy_dummy_report(sid));
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&envelope).unwrap();
        fs::write(dir.join("summary.bin"), bytes.as_ref())
            .await
            .unwrap();
        let env = st.read_report(node, sid).await.unwrap();
        assert!(matches!(env, ProfileReportEnvelope::V1Legacy(_)));
    }

    async fn seed_legacy_session(
        tentaflow_home: &Path,
        node: &str,
        sid: &str,
        report: &ProfileReport,
    ) {
        let dir = tentaflow_home.join("nsight").join(node).join(sid);
        fs::create_dir_all(&dir).await.unwrap();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(report).unwrap();
        fs::write(dir.join("summary.bin"), bytes.as_ref())
            .await
            .unwrap();
        fs::write(dir.join("report.nsys-rep"), b"nsys-bytes")
            .await
            .unwrap();
    }

    // 14
    #[tokio::test]
    async fn migration_legacy_session_to_v2() {
        let tmp = tempfile::tempdir().unwrap();
        let node = "node-1";
        let sid = "abcdef0123456789";
        let legacy = legacy_dummy_report(sid);
        seed_legacy_session(tmp.path(), node, sid, &legacy).await;

        let report = migrate_legacy_nsight_for_node(tmp.path(), node)
            .await
            .unwrap();
        assert_eq!(report.migrated, 1);
        assert!(report.failed.is_empty());

        // New layout is in place.
        let new_dir = tmp.path().join("profiling").join(node).join(sid);
        assert!(new_dir.exists());
        assert!(new_dir.join("manifest.json").exists());
        assert!(new_dir.join("summary.bin").exists());
        assert!(new_dir
            .join("raw")
            .join("nvidia.nsys.gpu")
            .join("report.nsys-rep")
            .exists());

        // Manifest content.
        let st = ProfileStorageV2::new(tmp.path());
        let m = st.read_manifest(node, sid).await.unwrap();
        assert_eq!(m.kind, SessionKind::LegacyNsight);
        assert_eq!(m.collectors_used, vec!["nvidia.nsys.gpu".to_string()]);

        // Report is V2 and carries the migrated kernel event.
        let env = st.read_report(node, sid).await.unwrap();
        match env {
            ProfileReportEnvelope::V2(v2) => {
                assert!(!v2.events.is_empty());
            }
            _ => panic!("expected V2"),
        }

        // Old layout removed.
        assert!(!tmp.path().join("nsight").join(node).join(sid).exists());
    }

    // 15
    #[tokio::test]
    async fn migration_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let node = "node-1";
        let sid = "abcdef0123456789";
        let legacy = legacy_dummy_report(sid);
        seed_legacy_session(tmp.path(), node, sid, &legacy).await;

        let r1 = migrate_legacy_nsight_for_node(tmp.path(), node)
            .await
            .unwrap();
        assert_eq!(r1.migrated, 1);

        // Second run: legacy dir was removed, but the new layout exists; re-seeding
        // a legacy session with the same id must be detected as already migrated.
        seed_legacy_session(tmp.path(), node, sid, &legacy).await;
        let r2 = migrate_legacy_nsight_for_node(tmp.path(), node)
            .await
            .unwrap();
        assert_eq!(r2.skipped_existing, 1);
        assert_eq!(r2.migrated, 0);
    }

    // 16
    #[tokio::test]
    async fn migration_partial_failure_continues() {
        let tmp = tempfile::tempdir().unwrap();
        let node = "node-1";
        let good_sid = "abcdef0123456789";
        let bad_sid = "1234567890abcdef";
        let legacy = legacy_dummy_report(good_sid);
        seed_legacy_session(tmp.path(), node, good_sid, &legacy).await;

        // Bad session: corrupted summary.bin (random non-rkyv bytes).
        let bad_dir = tmp.path().join("nsight").join(node).join(bad_sid);
        fs::create_dir_all(&bad_dir).await.unwrap();
        fs::write(bad_dir.join("summary.bin"), b"not rkyv at all")
            .await
            .unwrap();

        let report = migrate_legacy_nsight_for_node(tmp.path(), node)
            .await
            .unwrap();
        assert_eq!(report.migrated, 1);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, bad_sid.to_string());

        // Good session migrated, bad session left in legacy dir.
        let st = ProfileStorageV2::new(tmp.path());
        assert!(st.read_manifest(node, good_sid).await.is_ok());
        assert!(bad_dir.exists());
    }
}
