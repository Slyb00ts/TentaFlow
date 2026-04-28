#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux/nvsmi_gpu.rs — Continuous NVIDIA GPU sampler that
// spawns `nvidia-smi --query-gpu ... -l 1` as a child process and tees its
// stdout into nvsmi.csv. Complements `nvidia.nsys.gpu` (which captures kernel
// events) with steady 1 Hz utilization / memory / power timeline samples.
// Internal helpers used by the reader thread are Linux-only by design.
// =============================================================================

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command};
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader, Write};
#[cfg(target_os = "linux")]
use std::thread;
use std::time::Instant;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, GpuVendor, PowerDomain,
    TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner,
    PlatformSet, ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "linux.nvsmi.gpu_util";
const CSV_FILENAME: &str = "nvsmi.csv";
const QUERY: &str =
    "index,utilization.gpu,utilization.memory,memory.used,memory.free,temperature.gpu,power.draw";

/// Continuous GPU sampler driven by nvidia-smi.
pub struct LinuxNvsmiGpuCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxNvsmiGpuCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![
                    EventCategory::GpuUtilSample,
                    EventCategory::GpuMemSample,
                    EventCategory::PowerSample,
                ],
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(
                    PlatformSet::LINUX_X64 | PlatformSet::LINUX_ARM64,
                ),
                vendor: Some(GpuVendor::Nvidia),
                description:
                    "NVIDIA GPU utilization, memory and power via nvidia-smi at 1 Hz. \
                     Complements nsys with continuous sampling.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxNvsmiGpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn nvidia_smi_binary() -> Option<PathBuf> {
    // Cheapest discovery: rely on PATH. nvidia-smi installs with the driver
    // and is always placed in /usr/bin on supported Linux distros.
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("nvidia-smi");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn parse_nvsmi_version(stdout: &str) -> Option<String> {
    // Output line example: "NVIDIA-SMI 535.54.03   Driver Version: 535.54.03 ..."
    for line in stdout.lines() {
        if let Some(rest) = line.split_whitespace().nth(1) {
            if !rest.is_empty() && rest.chars().any(|c| c.is_ascii_digit()) {
                return Some(rest.to_string());
            }
        }
    }
    None
}

impl ProfileCollector for LinuxNvsmiGpuCollector {
    fn id(&self) -> &str {
        &self.id
    }
    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        let Some(bin) = nvidia_smi_binary() else {
            return ProbeResult::Unavailable {
                reason: "nvidia-smi not found in PATH".into(),
            };
        };
        match Command::new(&bin).arg("--version").output() {
            Ok(out) if out.status.success() => {
                let v = parse_nvsmi_version(&String::from_utf8_lossy(&out.stdout));
                ProbeResult::Available { version: v }
            }
            Ok(_) | Err(_) => ProbeResult::Available { version: None },
        }
    }

    #[cfg(target_os = "linux")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        let bin = nvidia_smi_binary().ok_or_else(|| {
            CollectorError::Spawn("nvidia-smi binary not found".into())
        })?;
        fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);

        let child = Command::new(&bin)
            .args([
                "--query-gpu",
                QUERY,
                "--format=csv,noheader,nounits",
                "-l",
                "1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| CollectorError::Spawn(format!("nvidia-smi spawn: {e}")))?;
        let pid = child.id();
        let stdout = child
            .stdout
            .as_ref()
            .map(|_| ())
            .ok_or_else(|| CollectorError::Spawn("nvidia-smi stdout pipe missing".into()))?;
        let _ = stdout;
        let started_at = Instant::now();
        let start_clock_ns = read_monotonic_ns();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples_observed = Arc::new(AtomicU64::new(0));
        let child_arc = Arc::new(Mutex::new(Some(child)));

        let csv_t = csv_path.clone();
        let stop_t = stop_flag.clone();
        let samples_t = samples_observed.clone();
        let child_for_reader = child_arc.clone();
        let started_t = started_at;

        let handle = thread::Builder::new()
            .name("tf-nvsmi-collector".into())
            .spawn(move || {
                if let Err(e) =
                    reader_loop(child_for_reader, csv_t, stop_t, samples_t, started_t)
                {
                    eprintln!("linux.nvsmi.gpu_util reader loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("nvsmi thread spawn: {e}")))?;

        Ok(Box::new(LinuxNvsmiGpuRunning {
            id: COLLECTOR_ID.to_string(),
            output_dir: ctx.output_dir.clone(),
            csv_path,
            child: child_arc,
            child_pid: pid,
            stop_flag,
            samples_observed,
            handle: Some(handle),
            started_at,
            start_clock_ns,
        }))
    }

    #[cfg(not(target_os = "linux"))]
    fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        Err(CollectorError::Custom(
            "linux.nvsmi.gpu_util is Linux-only".into(),
        ))
    }
}

pub struct LinuxNvsmiGpuRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    child: Arc<Mutex<Option<Child>>>,
    child_pid: u32,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
    started_at: Instant,
    start_clock_ns: u64,
}

fn send_sigterm(pid: u32) {
    if pid == 0 {
        return;
    }
    // SAFETY: kill() with a known pid and SIGTERM is a single syscall;
    // failure is reported via errno and ignored — we proceed to SIGKILL via
    // Child::kill on stop().
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

impl RunningCollector for LinuxNvsmiGpuRunning {
    fn collector_id(&self) -> &str {
        &self.id
    }

    fn stop(mut self: Box<Self>) -> Result<RawCapture, CollectorError> {
        self.stop_flag.store(true, Ordering::Relaxed);
        send_sigterm(self.child_pid);
        // Reader thread exits as soon as the child closes stdout.
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut c) = guard.take() {
                let _ = c.wait();
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let end_clock_ns = read_monotonic_ns();
        let end_session_ns = self.started_at.elapsed().as_nanos() as u64;

        let mut metadata: HashMap<String, String> = HashMap::new();
        metadata.insert("source".into(), "nvidia-smi -l 1".into());
        metadata.insert("query".into(), QUERY.into());
        metadata.insert("sample_period_ms".into(), "1000".into());

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
            samples_observed: self.samples_observed.load(Ordering::Relaxed),
        })
    }

    fn abort(mut self: Box<Self>) {
        self.stop_flag.store(true, Ordering::Relaxed);
        send_sigterm(self.child_pid);
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut c) = guard.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = fs::remove_dir_all(&self.output_dir);
    }
}

#[cfg(target_os = "linux")]
fn reader_loop(
    child: Arc<Mutex<Option<Child>>>,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    started_at: Instant,
) -> Result<(), CollectorError> {
    let mut file = fs::File::create(&csv_path)?;
    writeln!(
        file,
        "timestamp_ns,index,util_gpu,util_mem,mem_used_mib,mem_free_mib,temp_c,power_w"
    )?;

    // Take the stdout handle out of the child so we can BufReader it without
    // holding the mutex during reads.
    let stdout = {
        let mut guard = child.lock().map_err(|_| {
            CollectorError::Custom("nvsmi child mutex poisoned".into())
        })?;
        guard
            .as_mut()
            .and_then(|c| c.stdout.take())
            .ok_or_else(|| CollectorError::Custom("nvsmi stdout missing".into()))?
    };
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let cols: Vec<String> = line.split(',').map(|s| s.trim().to_string()).collect();
        if cols.len() < 7 {
            continue;
        }
        let ts_ns = started_at.elapsed().as_nanos() as u64;
        // Re-emit verbatim columns; parser maps them to events.
        writeln!(
            file,
            "{ts_ns},{},{},{},{},{},{},{}",
            cols[0], cols[1], cols[2], cols[3], cols[4], cols[5], cols[6]
        )?;
        samples_observed.fetch_add(1, Ordering::Relaxed);
    }
    file.flush()?;
    Ok(())
}

fn read_monotonic_ns() -> u64 {
    let mut ts: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

/// Parser implementation paired with `LinuxNvsmiGpuCollector`. Each CSV row
/// expands into three TimelineEvents (util, mem, power) so downstream lanes
/// can render them independently.
pub struct LinuxNvsmiGpuParser;

impl CollectorParser for LinuxNvsmiGpuParser {
    fn parse(
        &self,
        raw: RawCapture,
        _ctx: &SessionCtx,
        _names: &mut NameInterner,
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
        for (idx, line) in content.lines().enumerate() {
            if idx == 0 || line.is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if cols.len() < 8 {
                continue;
            }
            let ts: u64 = match cols[0].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let device_id: u32 = cols[1].parse().unwrap_or(0);
            let lane = device_id.min(u16::MAX as u32) as u16;
            let util_gpu: f32 = cols[2].parse().unwrap_or(0.0);
            let util_mem: f32 = cols[3].parse().unwrap_or(0.0);
            let mem_used_mib: u64 = cols[4].parse().unwrap_or(0);
            let mem_free_mib: u64 = cols[5].parse().unwrap_or(0);
            let temp_c: f32 = cols[6].parse().unwrap_or(0.0);
            let power_w: f32 = cols[7].parse().unwrap_or(0.0);
            let mem_used_bytes = mem_used_mib * 1024 * 1024;
            let mem_free_bytes = mem_free_mib * 1024 * 1024;

            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::GpuUtilSample,
                lane_hint: lane,
                payload: EventPayload::GpuUtilSample {
                    device_id,
                    compute_pct: util_gpu,
                    mem_pct: util_mem,
                    mem_used_bytes,
                    temp_c,
                },
            });
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::GpuMemSample,
                lane_hint: lane,
                payload: EventPayload::GpuMemSample {
                    device_id,
                    allocated_bytes: mem_used_bytes,
                    free_bytes: mem_free_bytes,
                },
            });
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::PowerSample,
                lane_hint: lane,
                payload: EventPayload::PowerSample {
                    domain: PowerDomain::Gpu(device_id),
                    watts: power_w,
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
    fn nvsmi_collector_default_id_and_capability() {
        let c = LinuxNvsmiGpuCollector::new();
        assert_eq!(c.id(), "linux.nvsmi.gpu_util");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::GpuUtilSample));
        assert!(cap.categories.contains(&EventCategory::GpuMemSample));
        assert!(cap.categories.contains(&EventCategory::PowerSample));
        assert_eq!(cap.vendor, Some(GpuVendor::Nvidia));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
        assert!(cap.platforms.contains(PlatformSet::LINUX_ARM64));
    }

    #[test]
    fn nvsmi_probe_smoke() {
        let c = LinuxNvsmiGpuCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => panic!("must not request elevation"),
        }
    }

    #[test]
    fn nvsmi_parse_version_extracts_first_token() {
        let s = "NVIDIA-SMI 535.54.03   Driver Version: 535.54.03   CUDA Version: 12.2";
        assert_eq!(parse_nvsmi_version(s), Some("535.54.03".to_string()));
    }

    #[test]
    fn nvsmi_parser_handles_empty_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("nvsmi.csv");
        fs::write(
            &csv,
            "timestamp_ns,index,util_gpu,util_mem,mem_used_mib,mem_free_mib,temp_c,power_w\n",
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
        let evs = LinuxNvsmiGpuParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn nvsmi_parser_emits_three_events_per_row() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("nvsmi.csv");
        let body = "timestamp_ns,index,util_gpu,util_mem,mem_used_mib,mem_free_mib,temp_c,power_w\n\
                    1000,0,42.0,15.0,1024,7168,55.0,120.5\n\
                    2000,0,55.0,20.0,1500,6692,60.0,135.7\n";
        fs::write(&csv, body).unwrap();
        let raw = RawCapture {
            artifacts: vec![csv],
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.into(),
                pairs: Vec::new(),
            },
            samples_observed: 2,
        };
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let ctx = ctx_with_dir(dir.path().to_path_buf());
        let evs = LinuxNvsmiGpuParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        // 2 rows * 3 events each = 6.
        assert_eq!(evs.len(), 6);
        assert_eq!(evs[0].category, EventCategory::GpuUtilSample);
        assert_eq!(evs[1].category, EventCategory::GpuMemSample);
        assert_eq!(evs[2].category, EventCategory::PowerSample);
        match &evs[0].payload {
            EventPayload::GpuUtilSample {
                device_id,
                compute_pct,
                mem_used_bytes,
                temp_c,
                ..
            } => {
                assert_eq!(*device_id, 0);
                assert!((*compute_pct - 42.0).abs() < 0.01);
                assert_eq!(*mem_used_bytes, 1024 * 1024 * 1024);
                assert!((*temp_c - 55.0).abs() < 0.01);
            }
            _ => panic!("wrong payload"),
        }
        match &evs[2].payload {
            EventPayload::PowerSample { domain, watts } => {
                assert_eq!(*domain, PowerDomain::Gpu(0));
                assert!((*watts - 120.5).abs() < 0.01);
            }
            _ => panic!("wrong payload"),
        }
    }
}
