// =============================================================================
// File: collectors/nvidia_nsys.rs — NVIDIA Nsight Systems collector adapter
// implementing the ProfileCollector trait. Wraps the existing `nsys profile`
// child-process plumbing from `profiling::nsys` (binary discovery, scope-to-args
// translation, SIGTERM-based teardown) and exposes it via the multi-source API.
// =============================================================================

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use std::sync::Mutex as StdMutex;
use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, GpuTargets, GpuVendor, ProfileScope,
    ProfileSourceFlags,
};
use tokio::process::{Child, Command};
use tokio::sync::OwnedMutexGuard;

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, PlatformSet, ProbeResult, ProfileCollector, RawCapture,
    RunningCollector, SessionCtx,
};
use crate::profiling::nsys::{
    build_nsys_args, nsys_binary, nsys_process_lock, send_sigterm, NsightScope,
};

/// Stable identifier exposed by the collector.
const COLLECTOR_ID: &str = "nvidia.nsys.gpu";

/// File name used for the raw nsys report inside `output_dir`.
const REPORT_FILENAME: &str = "report.nsys-rep";

/// Adapter for NVIDIA Nsight Systems wired into the multi-source registry.
pub struct NvidiaNsysCollector {
    capability: CollectorCapability,
    id: String,
}

impl NvidiaNsysCollector {
    pub fn new() -> Self {
        let capability = CollectorCapability {
            categories: vec![
                EventCategory::GpuKernel,
                EventCategory::GpuApiCall,
                EventCategory::GpuUtilSample,
                EventCategory::GpuMemSample,
                EventCategory::GpuMemTransfer,
                EventCategory::NvtxRange,
            ],
            elevation: ElevationRequirement::None,
            // nsys ships for Linux x64, Linux arm64-sbsa, and Windows x64.
            // Apple, Windows ARM64 and Android are explicitly out of scope.
            platforms: PlatformSet::from_flags(
                PlatformSet::LINUX_X64 | PlatformSet::LINUX_ARM64 | PlatformSet::WINDOWS_X64,
            ),
            vendor: Some(GpuVendor::Nvidia),
            description:
                "NVIDIA Nsight Systems profiler (CUDA kernels, API calls, NVTX ranges, GPU metrics).",
        };
        Self {
            capability,
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for NvidiaNsysCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for NvidiaNsysCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        // Cheap, sync probe: only check that the nsys binary is discoverable.
        // The full `nsys --version` round trip happens async via the legacy
        // runner's capability cache; we keep `probe()` non-blocking to honour
        // the trait contract (called from GUI code paths).
        match nsys_binary() {
            Some(_) => ProbeResult::Available { version: None },
            None => ProbeResult::Unavailable {
                reason: "nsys binary not found in PATH or known NVIDIA install locations".into(),
            },
        }
    }

    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        let nsight_scope = map_scope_to_nsight(&ctx.scope)?;

        let nsys_path = nsys_binary()
            .ok_or_else(|| CollectorError::Spawn("nsys binary not found".to_string()))?;

        // Acquire the process-wide nsys lock synchronously; collector start runs
        // inside an async context (orchestrator) so blocking briefly here is
        // acceptable — the lock contends only with the legacy runner.
        let lock = nsys_process_lock();
        let process_guard: OwnedMutexGuard<()> = lock
            .try_lock_owned()
            .map_err(|_| CollectorError::Spawn("another nsys session is already running".into()))?;

        // Ensure output directory exists; it was created by the orchestrator
        // but a defensive `create_dir_all` keeps this collector independent.
        std::fs::create_dir_all(&ctx.output_dir)?;

        let report_path = ctx.output_dir.join(REPORT_FILENAME);
        // duration_secs forwarded — nsys 2025.x wymaga target-command (sleep)
        // na koncu argumentow; build_nsys_args dokleja `sleep <duration>`.
        // Manual mode (orchestrator-driven stop) → duration=0 → bardzo dlugi
        // sleep ktory SIGTERM przerywa.
        let duration_secs = ctx.scope.duration_seconds;
        let args = build_nsys_args(&nsight_scope, &report_path, duration_secs);

        let child = Command::new(nsys_path)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| CollectorError::Spawn(format!("nsys spawn: {e}")))?;

        let child_pid = child.id().unwrap_or(0);
        let started_at = Instant::now();

        Ok(Box::new(NvidiaNsysRunning {
            id: COLLECTOR_ID.to_string(),
            child: StdMutex::new(Some(child)),
            child_pid,
            scope: nsight_scope,
            output_dir: ctx.output_dir.clone(),
            report_path,
            started_at,
            _process_guard: process_guard,
        }))
    }
}

/// Live nsys session owned by the orchestrator.
pub struct NvidiaNsysRunning {
    id: String,
    child: StdMutex<Option<Child>>,
    child_pid: u32,
    scope: NsightScope,
    output_dir: PathBuf,
    report_path: PathBuf,
    started_at: Instant,
    _process_guard: OwnedMutexGuard<()>,
}

impl RunningCollector for NvidiaNsysRunning {
    fn collector_id(&self) -> &str {
        &self.id
    }

    fn stop(self: Box<Self>) -> Result<RawCapture, CollectorError> {
        // Cooperative teardown: send SIGTERM so nsys flushes its report file,
        // then wait for the child. The orchestrator calls `stop` from inside
        // `spawn_blocking`, so we use the runtime handle (when available) to
        // wait without re-entering the executor.
        send_sigterm(self.child_pid);

        let mut child_opt = self
            .child
            .lock()
            .map_err(|_| CollectorError::Custom("nsys child mutex poisoned".into()))?
            .take();

        if let (Some(child), Ok(handle)) =
            (child_opt.as_mut(), tokio::runtime::Handle::try_current())
        {
            let _ = handle.block_on(async {
                tokio::time::timeout(std::time::Duration::from_secs(30), child.wait()).await
            });
        }
        // If no runtime is available the Child drops at end of scope and the
        // `kill_on_drop` flag set at spawn-time terminates the nsys process.
        drop(child_opt);

        let mut metadata: HashMap<String, String> = HashMap::new();
        metadata.insert("nsys_scope".to_string(), format!("{:?}", self.scope));
        metadata.insert(
            "started_at_monotonic_ns".to_string(),
            self.started_at.elapsed().as_nanos().to_string(),
        );
        metadata.insert("child_pid".to_string(), self.child_pid.to_string());

        let artifacts = if self.report_path.exists() {
            vec![self.report_path.clone()]
        } else {
            Vec::new()
        };

        Ok(RawCapture {
            artifacts,
            metadata,
            // nsys does not expose monotonic-vs-wallclock pair samples to the
            // outside; events read from the SQLite export are already aligned
            // to nsys' own session t=0, so an empty pair list is correct.
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.to_string(),
                pairs: Vec::new(),
            },
            samples_observed: 0,
        })
    }

    fn abort(self: Box<Self>) {
        send_sigterm(self.child_pid);
        // Drop child on best-effort: kill_on_drop ensures cleanup even if the
        // SIGTERM was lost. Output artifacts remain in `output_dir` for the
        // orchestrator to remove via storage.delete_session.
        let _ = self.output_dir;
    }
}

/// Translate a multi-source `ProfileScope` into the legacy `NsightScope` taxonomy.
///
/// Rules:
/// - Sources containing CPU* but no GPU map to `Cpu`.
/// - Sources containing GPU but no CPU* map to `GpuAll` / `GpuIndex(i)`.
/// - Sources containing both map to `BothAll` / `BothIndex(i)`.
/// - `Indices` with multiple entries collapse to `*All` (nsys takes one index
///   or `all`; we cannot pick a single index without losing information).
/// - `ByVendor(other)` is rejected — the registry filter must have already
///   excluded non-NVIDIA scopes; reaching this branch means a programming error.
pub(crate) fn map_scope_to_nsight(scope: &ProfileScope) -> Result<NsightScope, CollectorError> {
    let bits = scope.sources.0;
    let cpu_mask = ProfileSourceFlags::CPU_SAMPLING
        | ProfileSourceFlags::CPU_COUNTERS
        | ProfileSourceFlags::CPU_UTIL;
    let has_cpu = (bits & cpu_mask) != 0;
    let has_gpu = (bits & ProfileSourceFlags::GPU) != 0;

    if !has_cpu && !has_gpu {
        return Err(CollectorError::Custom(
            "nvidia.nsys.gpu requires CPU or GPU bits in scope.sources".into(),
        ));
    }

    let gpu_index: Option<u8> = match &scope.gpu_targets {
        GpuTargets::None => None,
        GpuTargets::All => None,
        GpuTargets::Indices(v) if v.len() == 1 => {
            let i = v[0];
            if i > u8::MAX as u32 {
                return Err(CollectorError::Custom(format!(
                    "gpu index {i} exceeds u8 range supported by nsys"
                )));
            }
            Some(i as u8)
        }
        // Multi-index: nsys exposes a single device or `all`; collapse to all.
        GpuTargets::Indices(_) => None,
        GpuTargets::ByVendor(GpuVendor::Nvidia) => None,
        GpuTargets::ByVendor(other) => {
            return Err(CollectorError::Custom(format!(
                "nvidia.nsys.gpu cannot profile vendor {:?}",
                other
            )));
        }
    };

    Ok(match (has_cpu, has_gpu, gpu_index) {
        (true, false, _) => NsightScope::Cpu,
        (false, true, Some(i)) => NsightScope::GpuIndex(i),
        (false, true, None) => NsightScope::GpuAll,
        (true, true, Some(i)) => NsightScope::BothIndex(i),
        (true, true, None) => NsightScope::BothAll,
        // Unreachable: covered by the early `!has_cpu && !has_gpu` branch.
        (false, false, _) => unreachable!("has_cpu/has_gpu both false handled above"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::profiling::{ProfileScope, ProfileTarget};

    fn scope_with(sources: u32, targets: GpuTargets) -> ProfileScope {
        ProfileScope {
            sources: ProfileSourceFlags(sources),
            gpu_targets: targets,
            cpu_sampling_hz: 99,
            target: ProfileTarget::OwnProcess,
            duration_seconds: 0,
            label: "t".into(),
        }
    }

    #[test]
    fn collector_default_id_and_capability() {
        let c = NvidiaNsysCollector::new();
        assert_eq!(c.id(), "nvidia.nsys.gpu");
        let cap = c.capability();
        assert_eq!(cap.vendor, Some(GpuVendor::Nvidia));
        assert!(cap.categories.contains(&EventCategory::GpuKernel));
        assert!(cap.categories.contains(&EventCategory::NvtxRange));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
        assert!(cap.platforms.contains(PlatformSet::LINUX_ARM64));
        assert!(cap.platforms.contains(PlatformSet::WINDOWS_X64));
        // Explicitly excluded.
        assert!(!cap.platforms.contains(PlatformSet::WINDOWS_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::MACOS_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::ANDROID_ARM64));
    }

    #[test]
    fn map_scope_to_nsight_cpu() {
        let s = scope_with(
            ProfileSourceFlags::CPU_SAMPLING | ProfileSourceFlags::CPU_UTIL,
            GpuTargets::None,
        );
        assert!(matches!(map_scope_to_nsight(&s).unwrap(), NsightScope::Cpu));
    }

    #[test]
    fn map_scope_to_nsight_gpu_all() {
        let s = scope_with(ProfileSourceFlags::GPU, GpuTargets::All);
        assert!(matches!(
            map_scope_to_nsight(&s).unwrap(),
            NsightScope::GpuAll
        ));
    }

    #[test]
    fn map_scope_to_nsight_gpu_index() {
        let s = scope_with(ProfileSourceFlags::GPU, GpuTargets::Indices(vec![1]));
        assert!(matches!(
            map_scope_to_nsight(&s).unwrap(),
            NsightScope::GpuIndex(1)
        ));
    }

    #[test]
    fn map_scope_to_nsight_both_all() {
        let s = scope_with(
            ProfileSourceFlags::CPU_SAMPLING | ProfileSourceFlags::GPU,
            GpuTargets::All,
        );
        assert!(matches!(
            map_scope_to_nsight(&s).unwrap(),
            NsightScope::BothAll
        ));
    }

    #[test]
    fn map_scope_to_nsight_both_index() {
        let s = scope_with(
            ProfileSourceFlags::CPU_SAMPLING | ProfileSourceFlags::GPU,
            GpuTargets::Indices(vec![0]),
        );
        assert!(matches!(
            map_scope_to_nsight(&s).unwrap(),
            NsightScope::BothIndex(0)
        ));
    }

    #[test]
    fn map_scope_to_nsight_rejects_amd_vendor() {
        let s = scope_with(
            ProfileSourceFlags::GPU,
            GpuTargets::ByVendor(GpuVendor::Amd),
        );
        assert!(map_scope_to_nsight(&s).is_err());
    }

    #[test]
    fn map_scope_to_nsight_multi_index_falls_back_to_all() {
        let s = scope_with(ProfileSourceFlags::GPU, GpuTargets::Indices(vec![0, 1]));
        // Multi-index collapses to GpuAll because nsys does not accept >1 index.
        assert!(matches!(
            map_scope_to_nsight(&s).unwrap(),
            NsightScope::GpuAll
        ));
    }

    #[test]
    fn map_scope_to_nsight_empty_sources_errors() {
        let s = scope_with(0, GpuTargets::None);
        assert!(map_scope_to_nsight(&s).is_err());
    }

    #[test]
    fn map_scope_to_nsight_by_vendor_nvidia_is_all() {
        let s = scope_with(
            ProfileSourceFlags::GPU,
            GpuTargets::ByVendor(GpuVendor::Nvidia),
        );
        assert!(matches!(
            map_scope_to_nsight(&s).unwrap(),
            NsightScope::GpuAll
        ));
    }

    #[test]
    fn probe_returns_some_result() {
        // We cannot guarantee the test environment has (or lacks) nsys, so we
        // assert only that probe() returns one of the recognised variants
        // without panicking. Either Available or Unavailable is acceptable.
        let c = NvidiaNsysCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => {
                panic!("nsys probe must not request elevation")
            }
        }
    }
}
