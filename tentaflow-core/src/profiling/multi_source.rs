// =============================================================================
// File: multi_source.rs — Orchestrator that runs multiple ProfileCollectors in
// parallel, merges their TimelineEvents into a single ProfileReportV2, and
// persists via ProfileStorageV2.
// =============================================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use tentaflow_protocol::profiling::{
    ClockSamples, CollectorRunInfo, CollectorStatus, DriftReport, EventCategory, EventPayload,
    ProfileReportV2, ProfileScope, TimelineEvent, DRIFT_TOLERANCE_NS,
    PROFILE_REPORT_V2_SCHEMA_VERSION,
};

use crate::profiling::collectors::{
    CollectorParser, CollectorRegistry, ElevationKind, ElevationToken, FrameInterner, FrameKey,
    NameInterner, ProbeResult, ProfileCollector, RunningCollector, SessionCtx,
};
use crate::profiling::storage_v2::{
    ProfileStorageV2, SessionKind, SessionManifest, SkippedCollector, StorageError,
};

// -----------------------------------------------------------------------------
// Public types
// -----------------------------------------------------------------------------

#[derive(thiserror::Error, Debug)]
pub enum SessionError {
    #[error("another session is already active")]
    AlreadyActive,
    #[error("no collectors available for the requested scope")]
    NoCollectorsAvailable,
    #[error("all collectors failed to start")]
    AllCollectorsFailed,
    #[error("invalid scope: {0}")]
    InvalidScope(String),
    #[error("storage: {0}")]
    Storage(#[from] StorageError),
    #[error("collector start: id={id} error={error}")]
    CollectorStartFailure { id: String, error: String },
    #[error("session handle is stale (orchestrator no longer tracks it)")]
    StaleHandle,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("merge: {0}")]
    Merge(String),
}

/// Lightweight reference returned from `start`. Re-validated against the
/// orchestrator's `epoch` counter on every method that consumes it.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    pub session_id: String,
    pub node_id: String,
    pub started_at_monotonic: Instant,
    epoch: u64,
}

#[derive(Debug, Clone)]
pub struct ActiveSessionInfo {
    pub session_id: String,
    pub node_id: String,
    pub label: String,
    pub started_at_unix_ns: u64,
    pub planned_duration_ns: u64,
    pub elapsed_ns: u64,
    pub collectors_running: Vec<String>,
    pub collectors_skipped: Vec<SkippedCollector>,
}

/// Maps `collector_id` to its parser implementation.
pub struct ParserRegistry {
    parsers: HashMap<String, Arc<dyn CollectorParser>>,
}

impl ParserRegistry {
    pub fn new() -> Self {
        Self {
            parsers: HashMap::new(),
        }
    }

    pub fn register(&mut self, collector_id: String, parser: Arc<dyn CollectorParser>) {
        self.parsers.insert(collector_id, parser);
    }

    pub fn get(&self, collector_id: &str) -> Option<Arc<dyn CollectorParser>> {
        self.parsers.get(collector_id).cloned()
    }

    /// Build a registry pre-populated with parsers for every collector
    /// currently registered by `CollectorRegistry::discover()`.
    pub fn default_registry() -> Self {
        use crate::profiling::collectors as c;
        let mut r = Self::new();
        // NVIDIA Nsight Systems SQLite parser.
        r.register("nvidia.nsys.gpu".to_string(), Arc::new(c::NvidiaNsysParser));
        // Linux no-priv parsers.
        r.register("linux.proc.cpu_util".to_string(), Arc::new(c::linux::cpu_util::LinuxProcCpuUtilParser));
        r.register("linux.proc.ram".to_string(), Arc::new(c::linux::ram::LinuxProcRamParser));
        r.register("linux.iostat.disk".to_string(), Arc::new(c::linux::disk::LinuxIostatDiskParser));
        r.register("linux.rapl.power".to_string(), Arc::new(c::linux::rapl_power::LinuxRaplPowerParser));
        r.register("linux.nvsmi.gpu_util".to_string(), Arc::new(c::linux::nvsmi_gpu::LinuxNvsmiGpuParser));
        // Linux GPU vendor parsers.
        r.register("linux.rocsmi.gpu_util".to_string(), Arc::new(c::linux_gpu::rocsmi_util::LinuxRocmSmiGpuParser));
        r.register("linux.rocprof.gpu_kernels".to_string(), Arc::new(c::linux_gpu::rocprof_kernels::LinuxRocprofKernelsParser));
        r.register("linux.intel_gpu_top.gpu".to_string(), Arc::new(c::linux_gpu::intel_gpu_top::LinuxIntelGpuTopParser));
        // macOS parsers.
        r.register("macos.vm_stat.ram".to_string(), Arc::new(c::macos::vm_stat_ram::MacosVmStatRamParser));
        r.register("macos.iostat.disk".to_string(), Arc::new(c::macos::iostat_disk::MacosIostatDiskParser));
        r.register("macos.powermetrics.power".to_string(), Arc::new(c::macos::powermetrics_power::MacosPowermetricsPowerParser));
        r.register("macos.powermetrics.gpu".to_string(), Arc::new(c::macos::powermetrics_gpu::MacosPowermetricsGpuParser));
        // Windows PDH parsers.
        r.register("windows.pdh.cpu_util".to_string(), Arc::new(c::windows::pdh_cpu_util::WindowsPdhCpuUtilParser));
        r.register("windows.pdh.ram".to_string(), Arc::new(c::windows::pdh_ram::WindowsPdhRamParser));
        r.register("windows.pdh.disk".to_string(), Arc::new(c::windows::pdh_disk::WindowsPdhDiskParser));
        r.register("windows.pdh.gpu".to_string(), Arc::new(c::windows::pdh_gpu::WindowsPdhGpuParser));
        r
    }
}

impl Default for ParserRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// -----------------------------------------------------------------------------
// Internal state
// -----------------------------------------------------------------------------

struct WatchdogControl {
    cancel_tx: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

struct ActiveSessionState {
    session_id: String,
    node_id: String,
    label: String,
    scope: ProfileScope,
    epoch: u64,
    t0_wallclock_unix_ns: u64,
    t0_instant: Instant,
    planned_duration_ns: u64,
    running: HashMap<String, Box<dyn RunningCollector>>,
    /// Per-collector primary category, captured at start-time so we can build
    /// `CollectorRunInfo` after the running handle has been consumed.
    primary_categories: HashMap<String, EventCategory>,
    skipped: Vec<SkippedCollector>,
    /// Pre-computed status for skipped collectors, mirroring `skipped` order.
    skipped_statuses: Vec<CollectorStatus>,
    watchdog: Option<WatchdogControl>,
    parsers: Arc<ParserRegistry>,
    started_at_iso: String,
}

// -----------------------------------------------------------------------------
// Orchestrator
// -----------------------------------------------------------------------------

pub struct MultiSourceSession {
    storage: Arc<ProfileStorageV2>,
    registry: Arc<CollectorRegistry>,
    active: Mutex<Option<ActiveSessionState>>,
    epoch_counter: AtomicU64,
}

impl MultiSourceSession {
    pub fn new(storage: Arc<ProfileStorageV2>, registry: Arc<CollectorRegistry>) -> Arc<Self> {
        Arc::new(Self {
            storage,
            registry,
            active: Mutex::new(None),
            epoch_counter: AtomicU64::new(0),
        })
    }

    pub async fn is_active(&self) -> bool {
        self.active.lock().await.is_some()
    }

    pub async fn active_info(&self) -> Option<ActiveSessionInfo> {
        let guard = self.active.lock().await;
        guard.as_ref().map(|s| ActiveSessionInfo {
            session_id: s.session_id.clone(),
            node_id: s.node_id.clone(),
            label: s.label.clone(),
            started_at_unix_ns: s.t0_wallclock_unix_ns,
            planned_duration_ns: s.planned_duration_ns,
            elapsed_ns: s.t0_instant.elapsed().as_nanos() as u64,
            collectors_running: s.running.keys().cloned().collect(),
            collectors_skipped: s.skipped.clone(),
        })
    }

    /// Start a new session.
    pub async fn start(
        self: Arc<Self>,
        scope: ProfileScope,
        node_id: String,
        session_id: String,
        label: String,
        elevation: Option<Arc<ElevationToken>>,
        parsers: Arc<ParserRegistry>,
    ) -> Result<SessionHandle, SessionError> {
        scope
            .validate()
            .map_err(|e| SessionError::InvalidScope(e.to_string()))?;

        let mut guard = self.active.lock().await;
        if guard.is_some() {
            return Err(SessionError::AlreadyActive);
        }

        let epoch = self.epoch_counter.fetch_add(1, Ordering::SeqCst) + 1;
        let session_dir = self.storage.session_dir(&node_id, &session_id).await?;
        let _ = session_dir; // Side-effect: directory is created.

        let t0_instant = Instant::now();
        let t0_wallclock_unix_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let started_at_iso = DateTime::<Utc>::from_timestamp(
            (t0_wallclock_unix_ns / 1_000_000_000) as i64,
            (t0_wallclock_unix_ns % 1_000_000_000) as u32,
        )
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).expect("epoch"))
        .to_rfc3339();

        // Filter by host platform + requested sources + GPU vendor selector.
        let candidates = self
            .registry
            .filter_for_scope(scope.sources.0, &scope.gpu_targets);

        let mut skipped: Vec<SkippedCollector> = Vec::new();
        let mut skipped_statuses: Vec<CollectorStatus> = Vec::new();
        let mut admitted: Vec<Arc<dyn ProfileCollector>> = Vec::new();
        let mut had_any_candidate = false;

        for c in candidates {
            had_any_candidate = true;
            match c.probe() {
                ProbeResult::Available { .. } => admitted.push(c),
                ProbeResult::NeedsElevation { kind, reason } => match elevation.as_ref() {
                    Some(tok) if tok.kind() == kind && kind != ElevationKind::None => {
                        admitted.push(c);
                    }
                    _ => {
                        let r = format!("elevation required (kind={kind:?}): {reason}");
                        skipped.push(SkippedCollector {
                            id: c.id().to_string(),
                            reason: r,
                        });
                        skipped_statuses.push(CollectorStatus::SkippedRequiresElevation);
                    }
                },
                ProbeResult::Unavailable { reason } => {
                    skipped.push(SkippedCollector {
                        id: c.id().to_string(),
                        reason: reason.clone(),
                    });
                    skipped_statuses.push(CollectorStatus::SkippedUnavailable(reason));
                }
            }
        }

        if !had_any_candidate || admitted.is_empty() {
            return Err(SessionError::NoCollectorsAvailable);
        }

        // Spawn collectors. Failure to start is recorded but not fatal unless ALL fail.
        let planned_duration_ns = (scope.duration_seconds as u64).saturating_mul(1_000_000_000);
        let target_pid = match scope.target {
            tentaflow_protocol::profiling::ProfileTarget::Pid(p) => Some(p),
            _ => None,
        };

        let mut running: HashMap<String, Box<dyn RunningCollector>> = HashMap::new();
        let mut primary_categories: HashMap<String, EventCategory> = HashMap::new();
        let mut start_failures = 0usize;
        let candidate_count = admitted.len();

        for c in admitted {
            let id = c.id().to_string();
            let cap = c.capability();
            let primary = cap
                .categories
                .first()
                .copied()
                .unwrap_or(EventCategory::Custom);
            let raw_dir = self
                .storage
                .collector_raw_dir(&node_id, &session_id, &id)
                .await?;
            let ctx = SessionCtx {
                session_id: session_id.clone(),
                t0_monotonic_ns: 0,
                t0_wallclock_unix_ns,
                output_dir: raw_dir,
                scope: scope.clone(),
                target_pid,
                elevation: elevation.clone(),
                planned_duration_ns,
            };
            match c.start(ctx) {
                Ok(handle) => {
                    running.insert(id.clone(), handle);
                    primary_categories.insert(id, primary);
                }
                Err(e) => {
                    let msg = format!("start failed: {e}");
                    warn!(collector = %id, error = %e, "collector failed to start");
                    skipped.push(SkippedCollector {
                        id: id.clone(),
                        reason: msg.clone(),
                    });
                    skipped_statuses.push(CollectorStatus::Failed(msg));
                    start_failures += 1;
                }
            }
        }

        if start_failures == candidate_count {
            // All admitted collectors failed to start — clean up created dirs.
            let _ = self.storage.delete_session(&node_id, &session_id).await;
            return Err(SessionError::AllCollectorsFailed);
        }

        // Watchdog (auto-stop) when duration > 0.
        let watchdog = if planned_duration_ns > 0 {
            let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
            let self_arc = Arc::clone(&self);
            let handle_clone = SessionHandle {
                session_id: session_id.clone(),
                node_id: node_id.clone(),
                started_at_monotonic: t0_instant,
                epoch,
            };
            let dur = Duration::from_nanos(planned_duration_ns);
            let join = tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::time::sleep(dur) => {
                        info!(
                            session_id = %handle_clone.session_id,
                            "watchdog auto-stop firing"
                        );
                        if let Err(e) = self_arc.stop(handle_clone).await {
                            warn!(error = %e, "watchdog auto-stop failed");
                        }
                    }
                    _ = cancel_rx => {
                        debug!("watchdog cancelled");
                    }
                }
            });
            Some(WatchdogControl {
                cancel_tx: Some(cancel_tx),
                handle: join,
            })
        } else {
            None
        };

        let state = ActiveSessionState {
            session_id: session_id.clone(),
            node_id: node_id.clone(),
            label,
            scope,
            epoch,
            t0_wallclock_unix_ns,
            t0_instant,
            planned_duration_ns,
            running,
            primary_categories,
            skipped,
            skipped_statuses,
            watchdog,
            parsers,
            started_at_iso,
        };

        *guard = Some(state);

        Ok(SessionHandle {
            session_id,
            node_id,
            started_at_monotonic: t0_instant,
            epoch,
        })
    }

    /// Stop the currently active session by its public `session_id`. Used by
    /// callers that hold the id but not the original `SessionHandle` (e.g. the
    /// dispatch layer where the handle is process-local). Returns `StaleHandle`
    /// when no session matches.
    pub async fn stop_by_id(
        self: Arc<Self>,
        session_id: &str,
    ) -> Result<ProfileReportV2, SessionError> {
        let epoch = {
            let guard = self.active.lock().await;
            match guard.as_ref() {
                Some(s) if s.session_id == session_id => s.epoch,
                _ => return Err(SessionError::StaleHandle),
            }
        };
        let handle = SessionHandle {
            session_id: session_id.to_string(),
            node_id: String::new(),
            started_at_monotonic: Instant::now(),
            epoch,
        };
        self.stop(handle).await
    }

    /// Stop the active session, run parsers, merge timelines and persist the report.
    pub async fn stop(
        self: Arc<Self>,
        handle: SessionHandle,
    ) -> Result<ProfileReportV2, SessionError> {
        let state = {
            let mut guard = self.active.lock().await;
            match guard.take() {
                Some(s) if s.epoch == handle.epoch => s,
                Some(other) => {
                    // Put back the foreign session and report stale.
                    *guard = Some(other);
                    return Err(SessionError::StaleHandle);
                }
                None => return Err(SessionError::StaleHandle),
            }
        };

        // Cancel watchdog if pending. Awaiting the JoinHandle is best-effort.
        if let Some(mut wd) = state.watchdog {
            if let Some(tx) = wd.cancel_tx.take() {
                let _ = tx.send(());
            }
            // If the watchdog is the caller (auto-stop), the join is on the same task —
            // we cannot await ourselves. Detach instead.
            wd.handle.abort();
        }

        let duration_ns = state.t0_instant.elapsed().as_nanos() as u64;

        // Stop each collector synchronously inside spawn_blocking and gather raw captures.
        let mut raw_results: Vec<(
            String,
            Result<crate::profiling::collectors::RawCapture, String>,
        )> = Vec::new();
        for (id, handle) in state.running {
            let id_clone = id.clone();
            let res = tokio::task::spawn_blocking(move || handle.stop())
                .await
                .map_err(|e| format!("join: {e}"));
            let mapped: Result<crate::profiling::collectors::RawCapture, String> = match res {
                Ok(Ok(rc)) => Ok(rc),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(e),
            };
            raw_results.push((id_clone, mapped));
        }

        // Per-collector parse with local interners. Then merge.
        let mut final_names = NameInterner::new();
        let mut final_frames = FrameInterner::new();
        let mut all_events: Vec<TimelineEvent> = Vec::new();
        let mut collectors_info: Vec<CollectorRunInfo> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let mut per_collector_clocks: Vec<ClockSamples> = Vec::new();

        // Build list of (id, primary_category) for stable source_idx ordering.
        let mut source_order: Vec<String> = raw_results.iter().map(|(id, _)| id.clone()).collect();
        // Append skipped at the end, so source_idx of running collectors stays stable.
        source_order.extend(state.skipped.iter().map(|s| s.id.clone()));

        let mut idx_of: HashMap<String, u16> = HashMap::new();
        for (i, id) in source_order.iter().enumerate() {
            let i_u16 =
                u16::try_from(i).map_err(|_| SessionError::Merge("too many collectors".into()))?;
            idx_of.insert(id.clone(), i_u16);
        }

        for (collector_id, raw_or_err) in raw_results {
            let primary = state
                .primary_categories
                .get(&collector_id)
                .copied()
                .unwrap_or(EventCategory::Custom);
            let source_idx = *idx_of.get(&collector_id).expect("idx populated");

            match raw_or_err {
                Err(err_msg) => {
                    warnings.push(format!("collector {collector_id} stop failed: {err_msg}"));
                    collectors_info.push(CollectorRunInfo {
                        id: collector_id,
                        status: CollectorStatus::Failed(err_msg),
                        samples_collected: 0,
                        raw_size_bytes: 0,
                        primary_category: primary,
                        duration_ns,
                    });
                }
                Ok(raw) => {
                    let samples = raw.samples_observed;
                    let raw_size = self
                        .storage
                        .collector_raw_dir(&state.node_id, &state.session_id, &collector_id)
                        .await
                        .ok()
                        .and_then(|p| compute_dir_size_blocking(p).ok())
                        .unwrap_or(0);

                    per_collector_clocks.push(raw.clock_samples.clone());

                    // Run the parser, if registered, with private interners.
                    if let Some(parser) = state.parsers.get(&collector_id) {
                        let ctx = SessionCtx {
                            session_id: state.session_id.clone(),
                            t0_monotonic_ns: 0,
                            t0_wallclock_unix_ns: state.t0_wallclock_unix_ns,
                            output_dir: self
                                .storage
                                .collector_raw_dir(&state.node_id, &state.session_id, &collector_id)
                                .await?,
                            scope: state.scope.clone(),
                            target_pid: match state.scope.target {
                                tentaflow_protocol::profiling::ProfileTarget::Pid(p) => Some(p),
                                _ => None,
                            },
                            elevation: None,
                            planned_duration_ns: state.planned_duration_ns,
                        };
                        let mut local_names = NameInterner::new();
                        let mut local_frames = FrameInterner::new();
                        match parser.parse(raw, &ctx, &mut local_names, &mut local_frames) {
                            Ok(mut events) => {
                                let local_names_vec = local_names.into_vec();
                                let (local_frames_vec, local_stacks_vec) =
                                    local_frames.into_parts();

                                // Build remap tables: local id -> final id.
                                let name_remap: Vec<u32> = local_names_vec
                                    .iter()
                                    .map(|s| final_names.intern(s))
                                    .collect();

                                let frame_remap: Vec<u32> = local_frames_vec
                                    .into_iter()
                                    .map(|f| final_frames.intern_frame(FrameKey::from(f)))
                                    .collect();

                                let stack_remap: Vec<u32> = local_stacks_vec
                                    .into_iter()
                                    .map(|s| {
                                        let translated: Vec<u32> = s
                                            .into_iter()
                                            .map(|fid| frame_remap[fid as usize])
                                            .collect();
                                        final_frames.intern_stack(translated)
                                    })
                                    .collect();

                                for ev in events.iter_mut() {
                                    ev.source_idx = source_idx;
                                    remap_event_payload(&mut ev.payload, &name_remap, &stack_remap);
                                }

                                let event_count = events.len();
                                all_events.append(&mut events);

                                collectors_info.push(CollectorRunInfo {
                                    id: collector_id,
                                    status: CollectorStatus::Used,
                                    samples_collected: samples.max(event_count as u64),
                                    raw_size_bytes: raw_size,
                                    primary_category: primary,
                                    duration_ns,
                                });
                            }
                            Err(e) => {
                                let msg = format!("parse failed: {e}");
                                warnings.push(format!("collector {collector_id}: {msg}"));
                                collectors_info.push(CollectorRunInfo {
                                    id: collector_id,
                                    status: CollectorStatus::Failed(msg),
                                    samples_collected: 0,
                                    raw_size_bytes: raw_size,
                                    primary_category: primary,
                                    duration_ns,
                                });
                            }
                        }
                    } else {
                        // No parser registered — record as Used with no events.
                        warnings.push(format!(
                            "collector {collector_id}: no parser registered, raw artifacts retained"
                        ));
                        collectors_info.push(CollectorRunInfo {
                            id: collector_id,
                            status: CollectorStatus::Used,
                            samples_collected: samples,
                            raw_size_bytes: raw_size,
                            primary_category: primary,
                            duration_ns,
                        });
                    }
                }
            }
        }

        // Append skipped collectors as info rows.
        for (sk, status) in state.skipped.iter().zip(state.skipped_statuses.iter()) {
            collectors_info.push(CollectorRunInfo {
                id: sk.id.clone(),
                status: status.clone(),
                samples_collected: 0,
                raw_size_bytes: 0,
                primary_category: EventCategory::Custom,
                duration_ns: 0,
            });
        }

        all_events.sort_by_key(|e| e.t_start_ns);

        // Build drift report.
        let max_drift = compute_max_drift(&per_collector_clocks);
        let drift_report = DriftReport {
            per_collector: per_collector_clocks,
            max_observed_drift_ns: max_drift,
            exceeded_tolerance: max_drift > DRIFT_TOLERANCE_NS,
            tolerance_ns: DRIFT_TOLERANCE_NS,
        };
        if drift_report.exceeded_tolerance {
            warnings.push(format!(
                "clock drift exceeded tolerance: {} ns > {} ns",
                max_drift, DRIFT_TOLERANCE_NS
            ));
        }

        let (frames_vec, stacks_vec) = final_frames.into_parts();
        let names_vec = final_names.into_vec();

        let collectors_used: Vec<String> = collectors_info
            .iter()
            .filter(|c| matches!(c.status, CollectorStatus::Used))
            .map(|c| c.id.clone())
            .collect();

        let report = ProfileReportV2 {
            schema_version: PROFILE_REPORT_V2_SCHEMA_VERSION,
            session_id: state.session_id.clone(),
            node_id: state.node_id.clone(),
            scope: state.scope.clone(),
            t0_monotonic_ns: 0,
            t0_wallclock_unix_ns: state.t0_wallclock_unix_ns,
            duration_ns,
            collectors: collectors_info,
            events: all_events,
            frames: frames_vec,
            stacks: stacks_vec,
            names: names_vec,
            drift_report,
            warnings: warnings.clone(),
        };

        let scope_value = serde_json::to_value(&state.scope)
            .map_err(|e| SessionError::Merge(format!("scope serialization: {e}")))?;

        let manifest = SessionManifest {
            schema_version: 0, // overwritten by storage
            session_id: state.session_id.clone(),
            label: state.label.clone(),
            node_id: state.node_id.clone(),
            kind: SessionKind::MultiSource,
            started_at: state.started_at_iso.clone(),
            duration_ns,
            scope: scope_value,
            collectors_used,
            collectors_skipped: state.skipped.clone(),
            size_bytes: 0,
            warnings,
        };

        self.storage
            .write_session(&state.node_id, &state.session_id, &manifest, &report)
            .await?;

        match self.storage.enforce_fifo(&state.node_id).await {
            Ok(n) if n > 0 => info!(deleted = n, "FIFO rotation removed old sessions"),
            Ok(_) => {}
            Err(e) => warn!(error = %e, "FIFO enforcement failed"),
        }
        match self
            .storage
            .enforce_size_cap(&state.node_id, &state.session_id)
            .await
        {
            Ok(_) => {}
            Err(e) => warn!(error = %e, "size cap enforcement failed"),
        }

        Ok(report)
    }

    /// Hard abort: drop all running collectors and remove the session directory.
    pub async fn abort(self: Arc<Self>, handle: SessionHandle) -> Result<(), SessionError> {
        let state = {
            let mut guard = self.active.lock().await;
            match guard.take() {
                Some(s) if s.epoch == handle.epoch => s,
                Some(other) => {
                    *guard = Some(other);
                    return Err(SessionError::StaleHandle);
                }
                None => return Err(SessionError::StaleHandle),
            }
        };

        if let Some(mut wd) = state.watchdog {
            if let Some(tx) = wd.cancel_tx.take() {
                let _ = tx.send(());
            }
            wd.handle.abort();
        }

        for (_id, h) in state.running {
            // abort is sync. Run on blocking pool to avoid blocking the runtime thread.
            let _ = tokio::task::spawn_blocking(move || h.abort()).await;
        }

        self.storage
            .delete_session(&state.node_id, &state.session_id)
            .await?;
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// Helpers
// -----------------------------------------------------------------------------

/// Replace `name_id` / `stack_id` references inside an `EventPayload` using
/// the local-to-final remap tables. Only payload variants that index into the
/// side tables are touched.
fn remap_event_payload(payload: &mut EventPayload, name_remap: &[u32], stack_remap: &[u32]) {
    match payload {
        EventPayload::CpuSample { stack_id, .. } => {
            if let Some(new_id) = stack_remap.get(*stack_id as usize) {
                *stack_id = *new_id;
            }
        }
        EventPayload::GpuKernel { name_id, .. }
        | EventPayload::GpuApiCall { name_id, .. }
        | EventPayload::NvtxRange { name_id, .. }
        | EventPayload::Custom { name_id, .. } => {
            if let Some(new_id) = name_remap.get(*name_id as usize) {
                *name_id = *new_id;
            }
        }
        _ => {}
    }
}

/// Compute the maximum residual between observed `(local, monotonic_session)`
/// pairs and a per-collector linear regression through them. Returns 0 if no
/// collector contributed at least two samples.
fn compute_max_drift(per_collector: &[ClockSamples]) -> u64 {
    let mut max_drift: u64 = 0;
    for cs in per_collector {
        if cs.pairs.len() < 2 {
            continue;
        }
        let n = cs.pairs.len() as f64;
        let sum_x: f64 = cs.pairs.iter().map(|(x, _)| *x as f64).sum();
        let sum_y: f64 = cs.pairs.iter().map(|(_, y)| *y as f64).sum();
        let mean_x = sum_x / n;
        let mean_y = sum_y / n;

        let mut num = 0.0f64;
        let mut den = 0.0f64;
        for &(x, y) in cs.pairs.iter() {
            let dx = x as f64 - mean_x;
            let dy = y as f64 - mean_y;
            num += dx * dy;
            den += dx * dx;
        }
        let slope = if den > 0.0 { num / den } else { 0.0 };
        let intercept = mean_y - slope * mean_x;

        for &(x, y) in cs.pairs.iter() {
            let predicted = slope * (x as f64) + intercept;
            let resid = (y as f64 - predicted).abs();
            let resid_u64 = if resid.is_finite() && resid >= 0.0 {
                resid as u64
            } else {
                0
            };
            if resid_u64 > max_drift {
                max_drift = resid_u64;
            }
        }
    }
    max_drift
}

/// Synchronous directory size walk used inside `stop` (raw dirs are small,
/// avoids needing async recursion through the storage helper for this path).
fn compute_dir_size_blocking(path: std::path::PathBuf) -> std::io::Result<u64> {
    let mut stack = vec![path];
    let mut total: u64 = 0;
    while let Some(d) = stack.pop() {
        let rd = match std::fs::read_dir(&d) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        for entry in rd {
            let entry = entry?;
            let md = std::fs::symlink_metadata(entry.path())?;
            let ft = md.file_type();
            if ft.is_symlink() {
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

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;
    use tentaflow_protocol::profiling::{
        ElevationRequirement, EventCategory, GpuTargets, GpuVendor, ProfileSourceFlags,
        ProfileTarget,
    };

    use crate::profiling::collectors::{
        CollectorCapability, CollectorError, PlatformSet, RawCapture,
    };

    // --- Mock collector -------------------------------------------------------

    struct MockCollector {
        id: String,
        cap: CollectorCapability,
        probe_result: StdMutex<Option<ProbeResult>>,
        start_should_fail: bool,
        stop_samples: StdMutex<Vec<(u64, u64)>>, // ClockSamples pairs
        stop_observed: u64,
    }

    impl MockCollector {
        fn new_cpu(id: &str) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                cap: CollectorCapability {
                    categories: vec![EventCategory::CpuSample],
                    elevation: ElevationRequirement::None,
                    platforms: PlatformSet::all(),
                    vendor: None,
                    description: "mock cpu",
                },
                probe_result: StdMutex::new(Some(ProbeResult::Available { version: None })),
                start_should_fail: false,
                stop_samples: StdMutex::new(Vec::new()),
                stop_observed: 0,
            })
        }

        fn new_gpu(id: &str) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                cap: CollectorCapability {
                    categories: vec![EventCategory::GpuKernel],
                    elevation: ElevationRequirement::None,
                    platforms: PlatformSet::all(),
                    vendor: Some(GpuVendor::Nvidia),
                    description: "mock gpu",
                },
                probe_result: StdMutex::new(Some(ProbeResult::Available { version: None })),
                start_should_fail: false,
                stop_samples: StdMutex::new(Vec::new()),
                stop_observed: 0,
            })
        }

        fn with_probe(self: Arc<Self>, p: ProbeResult) -> Arc<Self> {
            *self.probe_result.lock().unwrap() = Some(p);
            self
        }

        fn with_start_failure(mut self) -> Self {
            self.start_should_fail = true;
            self
        }
    }

    impl ProfileCollector for MockCollector {
        fn id(&self) -> &str {
            &self.id
        }
        fn capability(&self) -> &CollectorCapability {
            &self.cap
        }
        fn probe(&self) -> ProbeResult {
            // Replace the slot with Available so subsequent calls remain stable.
            let mut g = self.probe_result.lock().unwrap();
            match g.take() {
                Some(p) => {
                    let cloned = match &p {
                        ProbeResult::Available { version } => ProbeResult::Available {
                            version: version.clone(),
                        },
                        ProbeResult::NeedsElevation { kind, reason } => {
                            ProbeResult::NeedsElevation {
                                kind: *kind,
                                reason: reason.clone(),
                            }
                        }
                        ProbeResult::Unavailable { reason } => ProbeResult::Unavailable {
                            reason: reason.clone(),
                        },
                    };
                    *g = Some(p);
                    cloned
                }
                None => ProbeResult::Available { version: None },
            }
        }

        fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
            if self.start_should_fail {
                return Err(CollectorError::Spawn("mock-fail".into()));
            }
            let id = self.id.clone();
            let pairs = self.stop_samples.lock().unwrap().clone();
            let observed = self.stop_observed;
            Ok(Box::new(MockRunning {
                id,
                pairs,
                observed,
            }))
        }
    }

    struct MockRunning {
        id: String,
        pairs: Vec<(u64, u64)>,
        observed: u64,
    }

    impl RunningCollector for MockRunning {
        fn collector_id(&self) -> &str {
            &self.id
        }
        fn stop(self: Box<Self>) -> Result<RawCapture, CollectorError> {
            Ok(RawCapture {
                artifacts: Vec::new(),
                metadata: HashMap::new(),
                clock_samples: ClockSamples {
                    collector_id: self.id.clone(),
                    pairs: self.pairs.clone(),
                },
                samples_observed: self.observed,
            })
        }
        fn abort(self: Box<Self>) {}
    }

    // --- Mock parser ----------------------------------------------------------

    struct MockParser {
        events: StdMutex<Vec<TimelineEvent>>,
        names: Vec<String>,
    }

    impl MockParser {
        fn new(events: Vec<TimelineEvent>, names: Vec<String>) -> Arc<Self> {
            Arc::new(Self {
                events: StdMutex::new(events),
                names,
            })
        }
    }

    impl CollectorParser for MockParser {
        fn parse(
            &self,
            _raw: RawCapture,
            _ctx: &SessionCtx,
            names: &mut NameInterner,
            _frames: &mut FrameInterner,
        ) -> Result<Vec<TimelineEvent>, CollectorError> {
            // Intern this parser's local names so each event's name_id is local-correct.
            let local_ids: Vec<u32> = self.names.iter().map(|n| names.intern(n)).collect();
            let mut out = self.events.lock().unwrap().clone();
            // Rewrite name_ids in each event so they point at this parser's local table.
            for ev in out.iter_mut() {
                if let EventPayload::GpuKernel { name_id, .. }
                | EventPayload::GpuApiCall { name_id, .. }
                | EventPayload::NvtxRange { name_id, .. }
                | EventPayload::Custom { name_id, .. } = &mut ev.payload
                {
                    let idx = *name_id as usize;
                    if idx < local_ids.len() {
                        *name_id = local_ids[idx];
                    }
                }
            }
            Ok(out)
        }
    }

    // --- Setup helpers --------------------------------------------------------

    fn make_orchestrator() -> (
        TempDir,
        Arc<MultiSourceSession>,
        Arc<CollectorRegistry>,
        Arc<ParserRegistry>,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let registry = Arc::new(CollectorRegistry::new());
        let parsers = Arc::new(ParserRegistry::new());
        let orch = MultiSourceSession::new(storage, Arc::clone(&registry));
        (tmp, orch, registry, parsers)
    }

    fn cpu_scope() -> ProfileScope {
        ProfileScope {
            sources: ProfileSourceFlags(ProfileSourceFlags::CPU_SAMPLING),
            gpu_targets: GpuTargets::None,
            cpu_sampling_hz: 99,
            target: ProfileTarget::OwnProcess,
            duration_seconds: 0,
            label: "test".into(),
        }
    }

    fn gpu_scope() -> ProfileScope {
        ProfileScope {
            sources: ProfileSourceFlags(ProfileSourceFlags::GPU),
            gpu_targets: GpuTargets::All,
            cpu_sampling_hz: 99,
            target: ProfileTarget::OwnProcess,
            duration_seconds: 0,
            label: "test".into(),
        }
    }

    fn gpu_kernel_event(t_start: u64, name_local_id: u32) -> TimelineEvent {
        TimelineEvent {
            source_idx: 0,
            t_start_ns: t_start,
            t_end_ns: t_start + 100,
            category: EventCategory::GpuKernel,
            lane_hint: 0,
            payload: EventPayload::GpuKernel {
                device_id: 0,
                name_id: name_local_id,
                grid: [1, 1, 1],
                block: [1, 1, 1],
                shared_mem_bytes: 0,
            },
        }
    }

    // --- Tests ----------------------------------------------------------------

    #[tokio::test]
    async fn start_with_empty_registry_returns_no_collectors_available() {
        let (_tmp, orch, _reg, parsers) = make_orchestrator();
        let err = orch
            .start(
                cpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "lbl".into(),
                None,
                parsers,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::NoCollectorsAvailable));
    }

    #[tokio::test]
    async fn start_already_active_returns_already_active() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_cpu("mock.cpu"));
        let registry = Arc::new(reg);
        let parsers = Arc::new(ParserRegistry::new());
        let orch = MultiSourceSession::new(storage, registry);

        let _h1 = Arc::clone(&orch)
            .start(
                cpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                Arc::clone(&parsers),
            )
            .await
            .unwrap();
        let err = Arc::clone(&orch)
            .start(
                cpu_scope(),
                "node-1".into(),
                "cafef00dcafef00d".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::AlreadyActive));
    }

    #[tokio::test]
    async fn start_invalid_scope_returns_invalid_scope() {
        let (_tmp, orch, _reg, parsers) = make_orchestrator();
        let mut bad = cpu_scope();
        bad.cpu_sampling_hz = 10_000;
        let err = orch
            .start(
                bad,
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::InvalidScope(_)));
    }

    #[tokio::test]
    async fn start_filters_unavailable_collectors_to_none_available() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(
            MockCollector::new_cpu("mock.cpu").with_probe(ProbeResult::Unavailable {
                reason: "no /proc".into(),
            }),
        );
        let registry = Arc::new(reg);
        let parsers = Arc::new(ParserRegistry::new());
        let orch = MultiSourceSession::new(storage, registry);

        let err = orch
            .start(
                cpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::NoCollectorsAvailable));
    }

    #[tokio::test]
    async fn start_skips_needs_elevation_when_no_token() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_cpu("mock.cpu_a"));
        reg.register(MockCollector::new_cpu("mock.cpu_b").with_probe(
            ProbeResult::NeedsElevation {
                kind: ElevationKind::Sudo,
                reason: "needs sudo".into(),
            },
        ));
        let registry = Arc::new(reg);
        let parsers = Arc::new(ParserRegistry::new());
        let orch = MultiSourceSession::new(storage, registry);

        let _h = Arc::clone(&orch)
            .start(
                cpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap();
        let info = orch.active_info().await.unwrap();
        assert_eq!(info.collectors_running, vec!["mock.cpu_a".to_string()]);
        assert_eq!(info.collectors_skipped.len(), 1);
        assert_eq!(info.collectors_skipped[0].id, "mock.cpu_b");
    }

    #[tokio::test]
    async fn start_uses_elevation_when_token_provided() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(
            MockCollector::new_cpu("mock.cpu").with_probe(ProbeResult::NeedsElevation {
                kind: ElevationKind::Sudo,
                reason: "needs sudo".into(),
            }),
        );
        let registry = Arc::new(reg);
        let parsers = Arc::new(ParserRegistry::new());
        let orch = MultiSourceSession::new(storage, registry);

        let token = Arc::new(ElevationToken::new_sudo("hunter2".into()));
        let _h = Arc::clone(&orch)
            .start(
                cpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                Some(token),
                parsers,
            )
            .await
            .unwrap();
        let info = orch.active_info().await.unwrap();
        assert_eq!(info.collectors_running.len(), 1);
        assert!(info.collectors_skipped.is_empty());
    }

    #[tokio::test]
    async fn start_all_collectors_fail_returns_all_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        let failing = Arc::new(
            (*MockCollector::new_cpu("mock.cpu_fail"))
                .clone_dummy()
                .with_start_failure(),
        );
        reg.register(failing);
        let registry = Arc::new(reg);
        let parsers = Arc::new(ParserRegistry::new());
        let orch = MultiSourceSession::new(storage, registry);

        let err = orch
            .start(
                cpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SessionError::AllCollectorsFailed));
    }

    impl MockCollector {
        // Helper used by the all-failed test (cannot Clone Arc<dyn ProfileCollector>
        // straight into a new MockCollector because the inner type erases).
        fn clone_dummy(&self) -> MockCollector {
            MockCollector {
                id: self.id.clone(),
                cap: CollectorCapability {
                    categories: self.cap.categories.clone(),
                    elevation: self.cap.elevation.clone(),
                    platforms: self.cap.platforms,
                    vendor: self.cap.vendor,
                    description: self.cap.description,
                },
                probe_result: StdMutex::new(Some(ProbeResult::Available { version: None })),
                start_should_fail: self.start_should_fail,
                stop_samples: StdMutex::new(self.stop_samples.lock().unwrap().clone()),
                stop_observed: self.stop_observed,
            }
        }
    }

    #[tokio::test]
    async fn stop_returns_assembled_report_with_merged_events() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_gpu("mock.gpu_a"));
        reg.register(MockCollector::new_gpu("mock.gpu_b"));
        let registry = Arc::new(reg);
        let mut pr = ParserRegistry::new();
        pr.register(
            "mock.gpu_a".into(),
            MockParser::new(
                vec![
                    gpu_kernel_event(30, 0),
                    gpu_kernel_event(10, 1),
                    gpu_kernel_event(20, 0),
                ],
                vec!["kernel_foo".into(), "kernel_bar".into()],
            ),
        );
        pr.register(
            "mock.gpu_b".into(),
            MockParser::new(
                vec![
                    gpu_kernel_event(15, 0),
                    gpu_kernel_event(25, 1),
                    gpu_kernel_event(5, 0),
                ],
                vec!["kernel_foo".into(), "kernel_baz".into()],
            ),
        );
        let parsers = Arc::new(pr);
        let orch = MultiSourceSession::new(storage, registry);

        let h = Arc::clone(&orch)
            .start(
                gpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "lbl".into(),
                None,
                parsers,
            )
            .await
            .unwrap();
        let report = orch.stop(h).await.unwrap();
        assert_eq!(report.events.len(), 6);
        // Sorted by t_start_ns asc.
        let starts: Vec<u64> = report.events.iter().map(|e| e.t_start_ns).collect();
        assert_eq!(starts, vec![5, 10, 15, 20, 25, 30]);
        // Names deduplicated: kernel_foo, kernel_bar, kernel_baz = 3 unique.
        assert_eq!(report.names.len(), 3);
        assert!(report.names.contains(&"kernel_foo".to_string()));
        assert!(report.names.contains(&"kernel_bar".to_string()));
        assert!(report.names.contains(&"kernel_baz".to_string()));
    }

    #[tokio::test]
    async fn stop_persists_via_storage() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_gpu("mock.gpu"));
        let registry = Arc::new(reg);
        let mut pr = ParserRegistry::new();
        pr.register(
            "mock.gpu".into(),
            MockParser::new(vec![gpu_kernel_event(0, 0)], vec!["k".into()]),
        );
        let parsers = Arc::new(pr);
        let orch = MultiSourceSession::new(Arc::clone(&storage), registry);

        let sid = "deadbeefdeadbeef";
        let h = Arc::clone(&orch)
            .start(
                gpu_scope(),
                "node-1".into(),
                sid.into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap();
        let _ = orch.stop(h).await.unwrap();
        let m = storage.read_manifest("node-1", sid).await.unwrap();
        assert_eq!(m.session_id, sid);
        assert_eq!(m.kind, SessionKind::MultiSource);
        assert!(m.size_bytes > 0);
    }

    #[tokio::test]
    async fn abort_removes_session_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_gpu("mock.gpu"));
        let registry = Arc::new(reg);
        let parsers = Arc::new(ParserRegistry::new());
        let orch = MultiSourceSession::new(Arc::clone(&storage), registry);

        let sid = "deadbeefdeadbeef";
        let h = Arc::clone(&orch)
            .start(
                gpu_scope(),
                "node-1".into(),
                sid.into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap();
        let path = tmp.path().join("profiling").join("node-1").join(sid);
        assert!(path.exists());
        orch.abort(h).await.unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn stale_handle_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_gpu("mock.gpu"));
        let registry = Arc::new(reg);
        let mut pr = ParserRegistry::new();
        pr.register(
            "mock.gpu".into(),
            MockParser::new(vec![gpu_kernel_event(0, 0)], vec!["k".into()]),
        );
        let parsers = Arc::new(pr);
        let orch = MultiSourceSession::new(storage, registry);

        let h = Arc::clone(&orch)
            .start(
                gpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap();
        let h2 = h.clone();
        let _ = Arc::clone(&orch).stop(h).await.unwrap();
        let err = Arc::clone(&orch).stop(h2).await.unwrap_err();
        assert!(matches!(err, SessionError::StaleHandle));
    }

    #[tokio::test]
    async fn watchdog_auto_stops_after_duration() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_gpu("mock.gpu"));
        let registry = Arc::new(reg);
        let mut pr = ParserRegistry::new();
        pr.register(
            "mock.gpu".into(),
            MockParser::new(vec![gpu_kernel_event(0, 0)], vec!["k".into()]),
        );
        let parsers = Arc::new(pr);
        let orch = MultiSourceSession::new(storage, registry);

        let mut s = gpu_scope();
        s.duration_seconds = 1; // 1s duration -> watchdog fires
        let _h = Arc::clone(&orch)
            .start(
                s,
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap();
        assert!(orch.is_active().await);
        // Wait long enough for sleep(1s) + stop work to complete.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !orch.is_active().await,
            "watchdog did not auto-stop within 1.5s"
        );
    }

    #[test]
    fn parser_registry_default_has_nsys() {
        let r = ParserRegistry::default_registry();
        assert!(r.get("nvidia.nsys.gpu").is_some());
    }

    #[test]
    fn drift_report_no_collectors_zero() {
        let drift = compute_max_drift(&[]);
        assert_eq!(drift, 0);
    }

    #[test]
    fn drift_report_within_tolerance() {
        // y = x exactly -> zero residuals.
        let cs = ClockSamples {
            collector_id: "c1".into(),
            pairs: vec![(0, 0), (1_000_000, 1_000_000), (2_000_000, 2_000_000)],
        };
        let drift = compute_max_drift(&[cs]);
        assert_eq!(drift, 0);
        assert!(drift <= DRIFT_TOLERANCE_NS);
    }

    #[test]
    fn drift_report_exceeds_tolerance() {
        // Inject a large offset on the middle point — when the regression line is
        // forced through the two endpoints, the middle point's residual approaches
        // the full offset. 30 ms middle offset -> ~20 ms residual, well above 5 ms.
        let cs = ClockSamples {
            collector_id: "c1".into(),
            pairs: vec![
                (0, 0),
                (1_000_000_000, 1_000_000_000 + 30_000_000),
                (2_000_000_000, 2_000_000_000),
            ],
        };
        let drift = compute_max_drift(&[cs]);
        assert!(drift > DRIFT_TOLERANCE_NS, "drift={drift} ns");
    }

    #[tokio::test]
    async fn merge_dedupes_names_and_remaps_event_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_gpu("mock.a"));
        reg.register(MockCollector::new_gpu("mock.b"));
        let registry = Arc::new(reg);
        let mut pr = ParserRegistry::new();
        // Both parsers reference the SAME logical kernel name, but in different local id slots.
        pr.register(
            "mock.a".into(),
            MockParser::new(
                vec![gpu_kernel_event(0, 0)],
                vec!["kernel_foo".into(), "kernel_bar".into()],
            ),
        );
        pr.register(
            "mock.b".into(),
            MockParser::new(
                vec![gpu_kernel_event(1, 1)], // local id 1
                vec!["kernel_baz".into(), "kernel_foo".into()],
            ),
        );
        let parsers = Arc::new(pr);
        let orch = MultiSourceSession::new(storage, registry);

        let h = Arc::clone(&orch)
            .start(
                gpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap();
        let report = orch.stop(h).await.unwrap();
        // Expected unique names: kernel_foo, kernel_bar, kernel_baz.
        assert_eq!(report.names.len(), 3);
        let kernel_foo_id = report
            .names
            .iter()
            .position(|n| n == "kernel_foo")
            .expect("kernel_foo must be interned") as u32;
        // Both events should now reference the same final kernel_foo id.
        for ev in &report.events {
            if let EventPayload::GpuKernel { name_id, .. } = ev.payload {
                assert_eq!(name_id, kernel_foo_id);
            } else {
                panic!("unexpected payload");
            }
        }
    }

    #[tokio::test]
    async fn merge_preserves_event_categories() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_gpu("mock.gpu"));
        let registry = Arc::new(reg);
        let mut pr = ParserRegistry::new();
        let mixed = vec![
            TimelineEvent {
                source_idx: 0,
                t_start_ns: 0,
                t_end_ns: 0,
                category: EventCategory::PowerSample,
                lane_hint: 0,
                payload: EventPayload::PowerSample {
                    domain: tentaflow_protocol::profiling::PowerDomain::Gpu(0),
                    watts: 100.0,
                },
            },
            gpu_kernel_event(1, 0),
        ];
        pr.register("mock.gpu".into(), MockParser::new(mixed, vec!["k".into()]));
        let parsers = Arc::new(pr);
        let orch = MultiSourceSession::new(storage, registry);

        let h = Arc::clone(&orch)
            .start(
                gpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap();
        let report = orch.stop(h).await.unwrap();
        let cats: Vec<EventCategory> = report.events.iter().map(|e| e.category).collect();
        assert!(cats.contains(&EventCategory::PowerSample));
        assert!(cats.contains(&EventCategory::GpuKernel));
    }

    #[tokio::test]
    async fn is_active_and_info_during_session() {
        let tmp = tempfile::tempdir().unwrap();
        let storage = Arc::new(ProfileStorageV2::new(tmp.path()));
        let mut reg = CollectorRegistry::new();
        reg.register(MockCollector::new_gpu("mock.gpu"));
        let registry = Arc::new(reg);
        let parsers = Arc::new(ParserRegistry::new());
        let orch = MultiSourceSession::new(storage, registry);

        assert!(!orch.is_active().await);
        let _h = Arc::clone(&orch)
            .start(
                gpu_scope(),
                "node-1".into(),
                "deadbeefdeadbeef".into(),
                "l".into(),
                None,
                parsers,
            )
            .await
            .unwrap();
        assert!(orch.is_active().await);
        // Sleep briefly so elapsed_ns is > 0 deterministically.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let info = orch.active_info().await.unwrap();
        assert_eq!(info.collectors_running, vec!["mock.gpu".to_string()]);
        assert!(info.elapsed_ns > 0);
    }
}
