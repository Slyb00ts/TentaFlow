#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux_gpu/rocprof_kernels.rs — AMD GPU kernel profiler
// driven by `rocprof --stats`. Attach mode only: requires SessionCtx::target_pid.
// rocprof writes a CSV with aggregate per-kernel stats; the parser emits one
// `GpuKernel` TimelineEvent per row with `t_start_ns=0` and
// `t_end_ns=avg_ns` (aggregate, not per-call). Per-call timeline would
// require `--hsa-trace`; documented as future work in metadata.
// =============================================================================

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::Child;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, GpuVendor, TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner, PlatformSet,
    ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "linux.rocprof.gpu_kernels";
const CSV_FILENAME: &str = "rocprof.csv";

/// AMD GPU kernel profiler (rocprof attach mode).
pub struct LinuxRocprofKernelsCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxRocprofKernelsCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::GpuKernel],
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(PlatformSet::LINUX_X64),
                vendor: Some(GpuVendor::Amd),
                description: "AMD GPU kernel-level profiling via rocprof. Requires target_pid \
                     (attach mode). Aggregate stats only — per-call timeline requires \
                     --hsa-trace (future work).",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxRocprofKernelsCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn rocprof_binary() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("rocprof");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

impl ProfileCollector for LinuxRocprofKernelsCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        if rocprof_binary().is_some() {
            ProbeResult::Available { version: None }
        } else {
            ProbeResult::Unavailable {
                reason: "rocprof not found in PATH".into(),
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        let target_pid = ctx.target_pid.ok_or_else(|| {
            CollectorError::Custom("rocprof requires target_pid (attach mode)".into())
        })?;
        let bin = rocprof_binary()
            .ok_or_else(|| CollectorError::Spawn("rocprof binary not found".into()))?;
        fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);

        // rocprof exits on its own after `-d duration`. If duration is 0 we use
        // a generous safety cap of 600 s — the same hard limit the protocol
        // enforces in ProfileScope::validate.
        let duration_seconds = if ctx.scope.duration_seconds == 0 {
            600
        } else {
            ctx.scope.duration_seconds
        };

        let child = Command::new(&bin)
            .args([
                "--stats",
                "-p",
                &target_pid.to_string(),
                "-o",
                csv_path.to_string_lossy().as_ref(),
                "-d",
                &duration_seconds.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| CollectorError::Spawn(format!("rocprof spawn: {e}")))?;
        let pid = child.id();
        let started_at = Instant::now();
        let start_clock_ns = read_monotonic_ns();
        let child_arc = Arc::new(Mutex::new(Some(child)));

        Ok(Box::new(LinuxRocprofKernelsRunning {
            id: COLLECTOR_ID.to_string(),
            output_dir: ctx.output_dir.clone(),
            csv_path,
            child: child_arc,
            child_pid: pid,
            started_at,
            start_clock_ns,
        }))
    }

    #[cfg(not(target_os = "linux"))]
    fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        Err(CollectorError::Custom(
            "linux.rocprof.gpu_kernels is Linux-only".into(),
        ))
    }
}

pub struct LinuxRocprofKernelsRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    child: Arc<Mutex<Option<Child>>>,
    child_pid: u32,
    started_at: Instant,
    start_clock_ns: u64,
}

#[cfg(unix)]
fn send_sigterm(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: kill() with a known pid and SIGTERM; failure reported via errno.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn send_sigterm(_pid: u32) {}

impl RunningCollector for LinuxRocprofKernelsRunning {
    fn collector_id(&self) -> &str {
        &self.id
    }

    fn stop(self: Box<Self>) -> Result<RawCapture, CollectorError> {
        // rocprof normally exits on its own when the -d window elapses. If we
        // are stopping early, ask politely first then wait.
        send_sigterm(self.child_pid);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut c) = guard.take() {
                let _ = c.wait();
            }
        }
        let end_clock_ns = read_monotonic_ns();
        let end_session_ns = self.started_at.elapsed().as_nanos() as u64;

        let mut metadata: HashMap<String, String> = HashMap::new();
        metadata.insert("source".into(), "rocprof --stats".into());
        metadata.insert("rocprof_mode".into(), "stats_aggregate".into());
        metadata.insert(
            "future_work".into(),
            "per-call timeline requires --hsa-trace".into(),
        );

        let artifacts = if self.csv_path.exists() {
            vec![self.csv_path.clone()]
        } else {
            Vec::new()
        };

        Ok(RawCapture {
            artifacts,
            metadata,
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.to_string(),
                pairs: vec![(self.start_clock_ns, 0), (end_clock_ns, end_session_ns)],
            },
            samples_observed: 0,
        })
    }

    fn abort(self: Box<Self>) {
        send_sigterm(self.child_pid);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut c) = guard.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        let _ = fs::remove_dir_all(&self.output_dir);
    }
}

#[cfg(unix)]
fn read_monotonic_ns() -> u64 {
    let mut ts: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime with CLOCK_MONOTONIC and a stack timespec.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

#[cfg(not(unix))]
fn read_monotonic_ns() -> u64 {
    0
}

/// Parser implementation paired with `LinuxRocprofKernelsCollector`. Reads the
/// `Index,KernelName,Calls,TotalDurationNs,AverageNs,Percentage` CSV emitted by
/// `rocprof --stats` and emits one `GpuKernel` event per row.
pub struct LinuxRocprofKernelsParser;

impl CollectorParser for LinuxRocprofKernelsParser {
    fn parse(
        &self,
        raw: RawCapture,
        _ctx: &SessionCtx,
        names: &mut NameInterner,
        _frames: &mut FrameInterner,
    ) -> Result<Vec<TimelineEvent>, CollectorError> {
        let Some(csv_path) = raw.artifacts.first() else {
            return Ok(Vec::new());
        };
        let content = match fs::read_to_string(csv_path) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let mut events = Vec::new();
        let mut header_seen = false;
        let mut name_idx: usize = 1;
        let mut avg_idx: usize = 4;
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            if !header_seen {
                header_seen = true;
                let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
                for (i, c) in cols.iter().enumerate() {
                    let lower = c.to_ascii_lowercase();
                    if lower.contains("kernelname") || lower == "name" {
                        name_idx = i;
                    } else if lower.contains("average") {
                        avg_idx = i;
                    }
                }
                continue;
            }
            let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if cols.len() <= name_idx.max(avg_idx) {
                continue;
            }
            let name = cols[name_idx].trim_matches('"');
            if name.is_empty() {
                continue;
            }
            // rocprof reports AverageNs as a float for kernels that ran more
            // than once. Accept both integer and float formats.
            let avg_ns: u64 = match cols[avg_idx].parse::<u64>() {
                Ok(v) => v,
                Err(_) => match cols[avg_idx].parse::<f64>() {
                    Ok(f) if f.is_finite() && f >= 0.0 => f as u64,
                    _ => continue,
                },
            };
            let name_id = names.intern(name);
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: 0,
                t_end_ns: avg_ns,
                category: EventCategory::GpuKernel,
                lane_hint: 0,
                payload: EventPayload::GpuKernel {
                    device_id: 0,
                    name_id,
                    grid: [0, 0, 0],
                    block: [0, 0, 0],
                    shared_mem_bytes: 0,
                },
            });
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use tentaflow_protocol::profiling::{
        GpuTargets, ProfileScope, ProfileSourceFlags, ProfileTarget,
    };

    fn ctx_with_dir(dir: PathBuf) -> SessionCtx {
        SessionCtx {
            session_id: "00000000000000ab".into(),
            t0_monotonic_ns: 0,
            t0_wallclock_unix_ns: 0,
            output_dir: dir,
            scope: ProfileScope {
                sources: ProfileSourceFlags(ProfileSourceFlags::GPU),
                gpu_targets: GpuTargets::All,
                cpu_sampling_hz: 99,
                target: ProfileTarget::OwnProcess,
                duration_seconds: 0,
                label: "t".into(),
            },
            target_pid: None,
            elevation: None,
            planned_duration_ns: 0,
        }
    }

    #[test]
    fn rocprof_default_id_and_capability() {
        let c = LinuxRocprofKernelsCollector::new();
        assert_eq!(c.id(), "linux.rocprof.gpu_kernels");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::GpuKernel));
        assert_eq!(cap.elevation, ElevationRequirement::None);
        assert_eq!(cap.vendor, Some(GpuVendor::Amd));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
        assert!(!cap.platforms.contains(PlatformSet::LINUX_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::WINDOWS_X64));
    }

    #[test]
    fn rocprof_probe_returns_unavailable_when_binary_missing() {
        let dir = TempDir::new().unwrap();
        let prev = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        let c = LinuxRocprofKernelsCollector::new();
        let probe = c.probe();
        if let Some(p) = prev {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        match probe {
            ProbeResult::Unavailable { reason } => {
                assert!(reason.contains("rocprof"), "got: {reason}");
            }
            _ => panic!("expected Unavailable"),
        }
    }

    #[test]
    fn rocprof_parser_handles_empty() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("rocprof.csv");
        fs::write(
            &csv,
            "Index,KernelName,Calls,TotalDurationNs,AverageNs,Percentage\n",
        )
        .unwrap();
        let raw = RawCapture {
            artifacts: vec![csv],
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.into(),
                pairs: Vec::new(),
            },
            samples_observed: 0,
        };
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let ctx = ctx_with_dir(dir.path().to_path_buf());
        let evs = LinuxRocprofKernelsParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn rocprof_parser_emits_kernel_event_per_row() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("rocprof.csv");
        let body = "Index,KernelName,Calls,TotalDurationNs,AverageNs,Percentage\n\
                    0,gemm_kernel,100,5000000,50000,40.0\n\
                    1,reduce_kernel,50,2500000,50000,20.0\n\
                    2,\"copy_kernel\",200,1000000,5000,10.0\n";
        fs::write(&csv, body).unwrap();
        let raw = RawCapture {
            artifacts: vec![csv],
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.into(),
                pairs: Vec::new(),
            },
            samples_observed: 0,
        };
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let ctx = ctx_with_dir(dir.path().to_path_buf());
        let evs = LinuxRocprofKernelsParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(evs.len(), 3);
        for ev in &evs {
            assert_eq!(ev.category, EventCategory::GpuKernel);
            assert_eq!(ev.t_start_ns, 0);
        }
        match &evs[0].payload {
            EventPayload::GpuKernel { name_id, .. } => {
                assert_eq!(evs[0].t_end_ns, 50_000);
                let v = names.intern("gemm_kernel");
                assert_eq!(*name_id, v);
            }
            _ => panic!("wrong payload"),
        }
        match &evs[2].payload {
            EventPayload::GpuKernel { name_id, .. } => {
                assert_eq!(evs[2].t_end_ns, 5_000);
                let v = names.intern("copy_kernel");
                assert_eq!(*name_id, v);
            }
            _ => panic!("wrong payload"),
        }
    }

    #[test]
    fn rocprof_parser_emits_events_from_sample_with_float_avg() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("rocprof.csv");
        let body = "Index,KernelName,Calls,TotalDurationNs,AverageNs,Percentage\n\
                    0,fused_attention,17,170000.5,10000.029,55.5\n";
        fs::write(&csv, body).unwrap();
        let raw = RawCapture {
            artifacts: vec![csv],
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.into(),
                pairs: Vec::new(),
            },
            samples_observed: 0,
        };
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let ctx = ctx_with_dir(dir.path().to_path_buf());
        let evs = LinuxRocprofKernelsParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].t_end_ns, 10_000);
    }
}
