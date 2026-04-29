// =============================================================================
// Plik: profiling/storage.rs — multi-source profiling session storage.
// Opis: Layout per sesja:
//   <TENTAFLOW_HOME>/profiling/<node_id>/<session_id>/
//   ├── manifest.json     (serde JSON, operator-readable)
//   ├── summary.bin       (rkyv ProfileReportV2)
//   ├── flamegraph.bin    (optional, side-data — owned by CPU sampling parser)
//   └── raw/<collector_id>/<artifact files...>
//
// Path traversal jest odrzucana w kazdym entry-poincie: node_id, session_id i
// collector_id sa walidowane regexami przed jakakolwiek operacja FS.
// Rekurencyjne liczenie rozmiaru sluzy do egzekwowania per-session size cap
// (1 GiB) oraz FIFO 20 sesji per node.
// =============================================================================

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use tentaflow_protocol::profiling::{validate_collector_id, ProfileReportV2};
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

pub struct ProfileStorage {
    root: PathBuf,
    fifo_limit: usize,
    per_session_size_cap: u64,
}

impl ProfileStorage {
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
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(report)
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
            // Skip sessions bez summary.bin - to oznacza ze stop() nie ukonczyl
            // zapisu (proces tentaflow zabity, panic, OOM itp.). Pokazywanie ich
            // w liscie powoduje "Failed to load report: NotFound" gdy user kliknie.
            // Active session jest pokazywana w banner'ze (osobny code path),
            // nie przez list_sessions.
            let sp = entry.path().join("summary.bin");
            if !sp.exists() {
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
    ) -> Result<ProfileReportV2, StorageError> {
        let dir = self.session_dir_path(node_id, session_id)?;
        let p = dir.join("summary.bin");
        if !p.exists() {
            return Err(StorageError::NotFound(format!(
                "summary.bin for {session_id}"
            )));
        }
        let bytes = fs::read(&p).await?;
        rkyv::from_bytes::<ProfileReportV2, rkyv::rancor::Error>(&bytes)
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
    // Whole walk runs on the blocking pool: sessions with thousands of raw
    // files would otherwise issue thousands of `tokio::fs` calls (each
    // `spawn_blocking` round-trip) and stall the runtime worker.
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || compute_dir_size_sync(&dir))
        .await
        .map_err(|e| StorageError::Io(std::io::Error::other(format!("join: {e}"))))?
}

fn compute_dir_size_sync(dir: &Path) -> Result<u64, StorageError> {
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    let mut total: u64 = 0;
    while let Some(d) = stack.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(r) => r,
            // A symlink or vanished entry should not abort accounting.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(StorageError::Io(e)),
        };
        for entry in rd {
            let entry = entry?;
            let md = std::fs::symlink_metadata(entry.path())?;
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
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tentaflow_protocol::profiling::{
        CollectorRunInfo, CollectorStatus, DriftReport, EventCategory, EventPayload, GpuTargets,
        ProfileScope, ProfileSourceFlags, ProfileTarget, TimelineEvent,
        PROFILE_REPORT_V2_SCHEMA_VERSION,
    };

    fn make_storage() -> (TempDir, ProfileStorage) {
        let tmp = tempfile::tempdir().unwrap();
        let st = ProfileStorage::new(tmp.path());
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

    #[tokio::test]
    async fn session_dir_creates_validated() {
        let (_tmp, st) = make_storage();
        let p = st.session_dir("node-1", "deadbeefdeadbeef").await.unwrap();
        assert!(p.exists());
        assert!(p.ends_with("profiling/node-1/deadbeefdeadbeef"));
    }

    #[tokio::test]
    async fn session_dir_rejects_bad_session_id() {
        let (_tmp, st) = make_storage();
        let err = st.session_dir("node-1", "bad-id").await.unwrap_err();
        assert!(matches!(err, StorageError::InvalidSessionId(_)));
    }

    #[tokio::test]
    async fn session_dir_rejects_path_traversal_node() {
        let (_tmp, st) = make_storage();
        let err = st
            .session_dir("../../../etc", "deadbeefdeadbeef")
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::InvalidNodeId(_)));
    }

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
        let r = st.read_report(node, sid).await.unwrap();
        assert_eq!(r.session_id, sid);
        assert_eq!(r.events.len(), 1);
    }

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

    #[tokio::test]
    async fn delete_session_idempotent() {
        let (_tmp, st) = make_storage();
        let sid = "deadbeefdeadbeef";
        let node = "node-1";
        st.session_dir(node, sid).await.unwrap();
        st.delete_session(node, sid).await.unwrap();
        st.delete_session(node, sid).await.unwrap();
    }

    #[tokio::test]
    async fn enforce_fifo_keeps_newest_n() {
        let tmp = tempfile::tempdir().unwrap();
        let st = ProfileStorage::new_with_limits(tmp.path(), 20, DEFAULT_PER_SESSION_SIZE_CAP);
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

    #[tokio::test]
    async fn enforce_size_cap_removes_largest_raw() {
        let tmp = tempfile::tempdir().unwrap();
        // Cap chosen so that 6 KiB + 5 KiB exceeds cap, but 5 KiB alone fits.
        let st = ProfileStorage::new_with_limits(tmp.path(), 20, 8 * 1024);
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

    #[tokio::test]
    async fn enforce_size_cap_returns_err_if_still_over() {
        let tmp = tempfile::tempdir().unwrap();
        let st = ProfileStorage::new_with_limits(tmp.path(), 20, 1024);
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
}
