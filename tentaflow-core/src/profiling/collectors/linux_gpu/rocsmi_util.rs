#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux_gpu/rocsmi_util.rs — Continuous AMD GPU sampler that
// invokes `rocm-smi --showuse --showmemuse --showpower --showtemp --json` once
// per second, parses the JSON snapshot, and tees one row per device into a
// CSV artifact. The parser fans each row out into three TimelineEvents
// (GpuUtilSample, GpuMemSample, PowerSample) so GUI lanes can render them
// independently — same shape as the nvidia-smi sibling.
// =============================================================================

use std::collections::HashMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Write;
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, GpuVendor, PowerDomain,
    TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner, PlatformSet,
    ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "linux.rocsmi.gpu_util";
const CSV_FILENAME: &str = "gpu.csv";
const SAMPLE_PERIOD: Duration = Duration::from_secs(1);

/// Continuous AMD GPU sampler driven by rocm-smi.
pub struct LinuxRocmSmiGpuCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxRocmSmiGpuCollector {
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
                vendor: Some(GpuVendor::Amd),
                description: "AMD GPU utilization, memory and power via rocm-smi at 1 Hz.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxRocmSmiGpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn rocm_smi_binary() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("rocm-smi");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

impl ProfileCollector for LinuxRocmSmiGpuCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        if rocm_smi_binary().is_some() {
            ProbeResult::Available { version: None }
        } else {
            ProbeResult::Unavailable {
                reason: "rocm-smi not found in PATH".into(),
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        let bin = rocm_smi_binary()
            .ok_or_else(|| CollectorError::Spawn("rocm-smi binary not found".into()))?;
        fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples_observed = Arc::new(AtomicU64::new(0));
        let started_at = Instant::now();
        let start_clock_ns = read_monotonic_ns();

        let stop_t = stop_flag.clone();
        let samples_t = samples_observed.clone();
        let csv_t = csv_path.clone();
        let bin_t = bin;

        let handle = thread::Builder::new()
            .name("tf-rocsmi-collector".into())
            .spawn(move || {
                if let Err(e) = polling_loop(bin_t, csv_t, stop_t, samples_t, started_at) {
                    eprintln!("linux.rocsmi.gpu_util polling loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("rocsmi thread spawn: {e}")))?;

        Ok(Box::new(LinuxRocmSmiGpuRunning {
            id: COLLECTOR_ID.to_string(),
            output_dir: ctx.output_dir.clone(),
            csv_path,
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
            "linux.rocsmi.gpu_util is Linux-only".into(),
        ))
    }
}

pub struct LinuxRocmSmiGpuRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
    started_at: Instant,
    start_clock_ns: u64,
}

impl RunningCollector for LinuxRocmSmiGpuRunning {
    fn collector_id(&self) -> &str {
        &self.id
    }

    fn stop(mut self: Box<Self>) -> Result<RawCapture, CollectorError> {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let end_clock_ns = read_monotonic_ns();
        let end_session_ns = self.started_at.elapsed().as_nanos() as u64;

        let mut metadata: HashMap<String, String> = HashMap::new();
        metadata.insert("source".into(), "rocm-smi --json (1 Hz polling)".into());
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
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = fs::remove_dir_all(&self.output_dir);
    }
}

#[cfg(target_os = "linux")]
fn polling_loop(
    bin: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    started_at: Instant,
) -> Result<(), CollectorError> {
    let mut file = fs::File::create(&csv_path)?;
    writeln!(
        file,
        "timestamp_ns,device_id,compute_pct,mem_pct,mem_used_bytes,temp_c,power_w"
    )?;

    while !stop_flag.load(Ordering::Relaxed) {
        let snapshot = match Command::new(&bin)
            .args([
                "--showuse",
                "--showmemuse",
                "--showpower",
                "--showtemp",
                "--json",
            ])
            .output()
        {
            Ok(out) if out.status.success() => out.stdout,
            // Tool failure is non-fatal: log to stderr-like fallback (eprintln in
            // the spawning thread already), skip this tick, keep polling.
            Ok(_) | Err(_) => {
                if interruptible_sleep(&stop_flag, SAMPLE_PERIOD) {
                    break;
                }
                continue;
            }
        };
        let text = String::from_utf8_lossy(&snapshot);
        let ts_ns = started_at.elapsed().as_nanos() as u64;
        for sample in parse_rocm_smi_json(&text) {
            writeln!(
                file,
                "{ts_ns},{},{:.4},{:.4},{},{:.4},{:.4}",
                sample.device_id,
                sample.compute_pct,
                sample.mem_pct,
                sample.mem_used_bytes,
                sample.temp_c,
                sample.power_w
            )?;
            samples_observed.fetch_add(1, Ordering::Relaxed);
        }
        if interruptible_sleep(&stop_flag, SAMPLE_PERIOD) {
            break;
        }
    }
    file.flush()?;
    Ok(())
}

/// Sleep up to `dur` while honouring the stop flag at 100 ms granularity.
/// Returns `true` if the stop flag fired during the sleep.
#[cfg(target_os = "linux")]
fn interruptible_sleep(stop_flag: &Arc<AtomicBool>, dur: Duration) -> bool {
    let step = Duration::from_millis(100);
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if stop_flag.load(Ordering::Relaxed) {
            return true;
        }
        thread::sleep(step.min(deadline.saturating_duration_since(Instant::now())));
    }
    stop_flag.load(Ordering::Relaxed)
}

/// One parsed entry from a `rocm-smi --json` snapshot.
#[derive(Debug, Clone, PartialEq)]
struct RocmSample {
    device_id: u32,
    compute_pct: f32,
    mem_pct: f32,
    mem_used_bytes: u64,
    temp_c: f32,
    power_w: f32,
}

/// Parse one `rocm-smi --json` payload into per-card samples.
///
/// rocm-smi prints a top-level object whose keys are `card0`, `card1`, ...,
/// each value being an object with human-readable keys like
/// `"GPU use (%)"`, `"GPU Memory Allocated (VRAM%)"`, `"Temperature (Sensor edge) (C)"`,
/// `"Average Graphics Package Power (W)"`. The vendor sometimes adds a top-level
/// `"system"` key with version metadata; we skip anything that is not a `card<N>`.
fn parse_rocm_smi_json(text: &str) -> Vec<RocmSample> {
    let value: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(obj) = value.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, card) in obj {
        let Some(idx_str) = key.strip_prefix("card") else {
            continue;
        };
        let Ok(device_id) = idx_str.parse::<u32>() else {
            continue;
        };
        let Some(card_obj) = card.as_object() else {
            continue;
        };
        let compute_pct = pct_field(card_obj, &["GPU use (%)", "GPU use(%)"]).unwrap_or(0.0);
        let mem_pct = pct_field(
            card_obj,
            &[
                "GPU Memory Allocated (VRAM%)",
                "GPU memory use (%)",
                "GPU Memory Allocated(VRAM%)",
            ],
        )
        .unwrap_or(0.0);
        let mem_used_bytes = bytes_field(
            card_obj,
            &["VRAM Total Used Memory (B)", "GPU memory used (B)"],
        )
        .unwrap_or(0);
        let temp_c = float_field(
            card_obj,
            &[
                "Temperature (Sensor edge) (C)",
                "Temperature (Sensor junction) (C)",
                "Temperature (Sensor memory) (C)",
            ],
        )
        .unwrap_or(0.0);
        let power_w = float_field(
            card_obj,
            &[
                "Average Graphics Package Power (W)",
                "Current Socket Graphics Package Power (W)",
            ],
        )
        .unwrap_or(0.0);
        out.push(RocmSample {
            device_id,
            compute_pct,
            mem_pct,
            mem_used_bytes,
            temp_c,
            power_w,
        });
    }
    out.sort_by_key(|s| s.device_id);
    out
}

fn raw_str<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a str> {
    for k in keys {
        if let Some(v) = obj.get(*k) {
            if let Some(s) = v.as_str() {
                return Some(s);
            }
        }
    }
    None
}

fn float_field(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<f32> {
    let s = raw_str(obj, keys)?;
    s.trim().trim_end_matches('%').parse::<f32>().ok()
}

fn pct_field(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<f32> {
    float_field(obj, keys)
}

fn bytes_field(obj: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> Option<u64> {
    let s = raw_str(obj, keys)?;
    s.trim().parse::<u64>().ok()
}

#[cfg(unix)]
fn read_monotonic_ns() -> u64 {
    let mut ts: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime with CLOCK_MONOTONIC and a stack-allocated timespec.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

#[cfg(not(unix))]
fn read_monotonic_ns() -> u64 {
    0
}

/// Parser implementation paired with `LinuxRocmSmiGpuCollector`. Each CSV row
/// expands into three TimelineEvents (util, mem, power), mirroring the
/// nvidia-smi parser shape.
pub struct LinuxRocmSmiGpuParser;

impl CollectorParser for LinuxRocmSmiGpuParser {
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
            if cols.len() < 7 {
                continue;
            }
            let ts: u64 = match cols[0].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let device_id: u32 = cols[1].parse().unwrap_or(0);
            let lane = device_id.min(u16::MAX as u32) as u16;
            let compute_pct: f32 = cols[2].parse().unwrap_or(0.0);
            let mem_pct: f32 = cols[3].parse().unwrap_or(0.0);
            let mem_used_bytes: u64 = cols[4].parse().unwrap_or(0);
            let temp_c: f32 = cols[5].parse().unwrap_or(0.0);
            let power_w: f32 = cols[6].parse().unwrap_or(0.0);

            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::GpuUtilSample,
                lane_hint: lane,
                payload: EventPayload::GpuUtilSample {
                    device_id,
                    compute_pct,
                    mem_pct,
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
                    // rocm-smi --json does not surface free VRAM directly; we
                    // emit allocated-only and let GUI compute totals if a side
                    // channel ever provides them.
                    free_bytes: 0,
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
    fn rocsmi_default_id_and_capability() {
        let c = LinuxRocmSmiGpuCollector::new();
        assert_eq!(c.id(), "linux.rocsmi.gpu_util");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::GpuUtilSample));
        assert!(cap.categories.contains(&EventCategory::GpuMemSample));
        assert!(cap.categories.contains(&EventCategory::PowerSample));
        assert_eq!(cap.elevation, ElevationRequirement::None);
        assert_eq!(cap.vendor, Some(GpuVendor::Amd));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
        assert!(cap.platforms.contains(PlatformSet::LINUX_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::WINDOWS_X64));
    }

    #[test]
    fn rocsmi_probe_returns_unavailable_when_binary_missing() {
        let dir = TempDir::new().unwrap();
        // Force PATH to a directory that cannot contain rocm-smi.
        let prev = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        let c = LinuxRocmSmiGpuCollector::new();
        let probe = c.probe();
        // Restore PATH first to avoid leaking on assertion failure.
        if let Some(p) = prev {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        match probe {
            ProbeResult::Unavailable { reason } => {
                assert!(reason.contains("rocm-smi"), "got: {reason}");
            }
            ProbeResult::Available { .. } | ProbeResult::NeedsElevation { .. } => {
                panic!("expected Unavailable")
            }
        }
    }

    #[test]
    fn rocsmi_parser_handles_empty_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("gpu.csv");
        fs::write(
            &csv,
            "timestamp_ns,device_id,compute_pct,mem_pct,mem_used_bytes,temp_c,power_w\n",
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
        let evs = LinuxRocmSmiGpuParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn rocsmi_parser_emits_three_events_per_sample() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("gpu.csv");
        let body = "timestamp_ns,device_id,compute_pct,mem_pct,mem_used_bytes,temp_c,power_w\n\
                    1000,0,42.0000,15.0000,1073741824,55.0000,120.5000\n\
                    2000,1,80.0000,30.0000,2147483648,70.0000,210.0000\n";
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
        let evs = LinuxRocmSmiGpuParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
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
                assert_eq!(*mem_used_bytes, 1_073_741_824);
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
        // Second card.
        match &evs[3].payload {
            EventPayload::GpuUtilSample { device_id, .. } => assert_eq!(*device_id, 1),
            _ => panic!("wrong payload"),
        }
    }

    #[test]
    fn rocsmi_json_parser_extracts_all_fields() {
        let json = r#"{
            "system": {"Driver version": "6.0.0"},
            "card0": {
                "GPU use (%)": "42",
                "GPU Memory Allocated (VRAM%)": "15",
                "VRAM Total Used Memory (B)": "1073741824",
                "Temperature (Sensor edge) (C)": "55.0",
                "Average Graphics Package Power (W)": "120.5"
            }
        }"#;
        let samples = parse_rocm_smi_json(json);
        assert_eq!(samples.len(), 1);
        let s = &samples[0];
        assert_eq!(s.device_id, 0);
        assert!((s.compute_pct - 42.0).abs() < 0.01);
        assert!((s.mem_pct - 15.0).abs() < 0.01);
        assert_eq!(s.mem_used_bytes, 1_073_741_824);
        assert!((s.temp_c - 55.0).abs() < 0.01);
        assert!((s.power_w - 120.5).abs() < 0.01);
    }

    #[test]
    fn rocsmi_json_parser_handles_empty() {
        assert!(parse_rocm_smi_json("").is_empty());
        assert!(parse_rocm_smi_json("{}").is_empty());
        assert!(parse_rocm_smi_json("not json").is_empty());
    }
}
