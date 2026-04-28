#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux_gpu/intel_gpu_top.rs — Intel GPU sampler that spawns
// `intel_gpu_top -J -s 1000` (streaming JSON, 1 s sampling) and tees one row
// per sample into a CSV artifact. Each row carries `engines.Render/3D/0.busy`
// as `compute_pct`. Memory and per-kernel data are not exposed by
// intel_gpu_top; kernel-level profiling on Intel GPUs requires VTune
// (commercial) and is intentionally out of scope.
// =============================================================================

use std::collections::HashMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::process::Stdio;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::thread;
use std::thread::JoinHandle;
use std::time::Instant;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, GpuVendor, TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner, PlatformSet,
    ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "linux.intel_gpu_top.gpu";
const CSV_FILENAME: &str = "intel_gpu.csv";

/// Continuous Intel GPU sampler driven by intel_gpu_top.
pub struct LinuxIntelGpuTopCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxIntelGpuTopCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::GpuUtilSample],
                elevation: ElevationRequirement::LinuxCap("CAP_PERFMON".into()),
                platforms: PlatformSet::from_flags(
                    PlatformSet::LINUX_X64 | PlatformSet::LINUX_ARM64,
                ),
                vendor: Some(GpuVendor::Intel),
                description: "Intel GPU utilization via intel_gpu_top. Kernel-level profiling \
                     unavailable without VTune (commercial).",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxIntelGpuTopCollector {
    fn default() -> Self {
        Self::new()
    }
}

fn intel_gpu_top_binary() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("intel_gpu_top");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

impl ProfileCollector for LinuxIntelGpuTopCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        let Some(bin) = intel_gpu_top_binary() else {
            return ProbeResult::Unavailable {
                reason: "intel_gpu_top not found in PATH".into(),
            };
        };
        // Cheap permission probe: invoke `-h`. intel_gpu_top normally short-
        // circuits help output before touching perf_event_open, but if the
        // build refuses to start without CAP_PERFMON we surface that.
        let out = match Command::new(&bin).arg("-h").output() {
            Ok(o) => o,
            Err(_) => return ProbeResult::Available { version: None },
        };
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
        .to_lowercase();
        if combined.contains("permission denied") || combined.contains("perf_event_paranoid") {
            ProbeResult::NeedsElevation {
                kind: crate::profiling::collectors::ElevationKind::LinuxCap,
                reason: "intel_gpu_top requires CAP_PERFMON or perf_event_paranoid <= 1".into(),
            }
        } else {
            ProbeResult::Available { version: None }
        }
    }

    #[cfg(target_os = "linux")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        let bin = intel_gpu_top_binary()
            .ok_or_else(|| CollectorError::Spawn("intel_gpu_top binary not found".into()))?;
        fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);

        let child = Command::new(&bin)
            .args(["-J", "-s", "1000"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| CollectorError::Spawn(format!("intel_gpu_top spawn: {e}")))?;
        let pid = child.id();
        let started_at = Instant::now();
        let start_clock_ns = read_monotonic_ns();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples_observed = Arc::new(AtomicU64::new(0));
        let child_arc = Arc::new(Mutex::new(Some(child)));

        let csv_t = csv_path.clone();
        let stop_t = stop_flag.clone();
        let samples_t = samples_observed.clone();
        let child_for_reader = child_arc.clone();

        let handle = thread::Builder::new()
            .name("tf-intelgputop-collector".into())
            .spawn(move || {
                if let Err(e) = reader_loop(child_for_reader, csv_t, stop_t, samples_t, started_at)
                {
                    eprintln!("linux.intel_gpu_top.gpu reader loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("intel_gpu_top thread spawn: {e}")))?;

        Ok(Box::new(LinuxIntelGpuTopRunning {
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
            "linux.intel_gpu_top.gpu is Linux-only".into(),
        ))
    }
}

pub struct LinuxIntelGpuTopRunning {
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

impl RunningCollector for LinuxIntelGpuTopRunning {
    fn collector_id(&self) -> &str {
        &self.id
    }

    fn stop(mut self: Box<Self>) -> Result<RawCapture, CollectorError> {
        self.stop_flag.store(true, Ordering::Relaxed);
        send_sigterm(self.child_pid);
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
        metadata.insert("source".into(), "intel_gpu_top -J -s 1000".into());
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
    writeln!(file, "timestamp_ns,compute_pct")?;

    let stdout = {
        let mut guard = child
            .lock()
            .map_err(|_| CollectorError::Custom("intel_gpu_top child mutex poisoned".into()))?;
        guard
            .as_mut()
            .and_then(|c| c.stdout.take())
            .ok_or_else(|| CollectorError::Custom("intel_gpu_top stdout missing".into()))?
    };
    let reader = BufReader::new(stdout);

    // intel_gpu_top -J emits a stream of JSON objects pretty-printed across
    // multiple lines. We accumulate text and try to parse whenever brace depth
    // returns to zero — robust to whitespace and to the leading "[" some
    // versions emit when wrapping the stream as a JSON array.
    let mut buf = String::new();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for line in reader.lines() {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        let Ok(line) = line else { break };
        for ch in line.chars() {
            if in_string {
                if escape {
                    escape = false;
                } else if ch == '\\' {
                    escape = true;
                } else if ch == '"' {
                    in_string = false;
                }
            } else if ch == '"' {
                in_string = true;
            } else if ch == '{' {
                depth += 1;
            } else if ch == '}' {
                depth -= 1;
            }
            buf.push(ch);
            if depth == 0 && !buf.trim().is_empty() {
                // Try to parse what we have as a single JSON value.
                if buf.contains('{') {
                    if let Some(pct) = parse_intel_gpu_top_sample(buf.trim()) {
                        let ts_ns = started_at.elapsed().as_nanos() as u64;
                        writeln!(file, "{ts_ns},{pct:.4}")?;
                        samples_observed.fetch_add(1, Ordering::Relaxed);
                    }
                    buf.clear();
                }
            }
        }
        buf.push('\n');
    }
    file.flush()?;
    Ok(())
}

/// Parse a single intel_gpu_top JSON sample and return the Render/3D/0 busy %.
/// Returns `None` if the document is malformed or lacks the expected key path.
///
/// Field path: `engines."Render/3D/0".busy` (newer builds) or
/// `engines.Render.busy` (older builds — best-effort fallback).
fn parse_intel_gpu_top_sample(text: &str) -> Option<f32> {
    // Strip optional leading `,` (intel_gpu_top wraps samples as JSON array).
    let trimmed = text.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let engines = value.get("engines")?.as_object()?;
    // Try the canonical "Render/3D/0" name first; fall back to anything that
    // starts with "Render".
    if let Some(obj) = engines.get("Render/3D/0").and_then(|v| v.as_object()) {
        return obj.get("busy").and_then(value_to_f32);
    }
    for (name, v) in engines {
        if name.starts_with("Render") {
            if let Some(obj) = v.as_object() {
                if let Some(b) = obj.get("busy").and_then(value_to_f32) {
                    return Some(b);
                }
            }
        }
    }
    None
}

fn value_to_f32(v: &serde_json::Value) -> Option<f32> {
    if let Some(f) = v.as_f64() {
        return Some(f as f32);
    }
    if let Some(s) = v.as_str() {
        return s.trim().parse::<f32>().ok();
    }
    None
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

/// Parser implementation paired with `LinuxIntelGpuTopCollector`. Each CSV row
/// expands into one `GpuUtilSample` event.
pub struct LinuxIntelGpuTopParser;

impl CollectorParser for LinuxIntelGpuTopParser {
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
            if cols.len() < 2 {
                continue;
            }
            let ts: u64 = match cols[0].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let compute_pct: f32 = cols[1].parse().unwrap_or(0.0);
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::GpuUtilSample,
                lane_hint: 0,
                payload: EventPayload::GpuUtilSample {
                    device_id: 0,
                    compute_pct,
                    mem_pct: 0.0,
                    mem_used_bytes: 0,
                    temp_c: 0.0,
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
    fn intel_gpu_top_default_id_and_capability() {
        let c = LinuxIntelGpuTopCollector::new();
        assert_eq!(c.id(), "linux.intel_gpu_top.gpu");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::GpuUtilSample));
        assert_eq!(
            cap.elevation,
            ElevationRequirement::LinuxCap("CAP_PERFMON".into())
        );
        assert_eq!(cap.vendor, Some(GpuVendor::Intel));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
        assert!(cap.platforms.contains(PlatformSet::LINUX_ARM64));
    }

    #[test]
    fn intel_gpu_top_probe_returns_unavailable_when_binary_missing() {
        let dir = TempDir::new().unwrap();
        let prev = std::env::var_os("PATH");
        std::env::set_var("PATH", dir.path());
        let c = LinuxIntelGpuTopCollector::new();
        let probe = c.probe();
        if let Some(p) = prev {
            std::env::set_var("PATH", p);
        } else {
            std::env::remove_var("PATH");
        }
        match probe {
            ProbeResult::Unavailable { reason } => {
                assert!(reason.contains("intel_gpu_top"), "got: {reason}");
            }
            _ => panic!("expected Unavailable"),
        }
    }

    #[test]
    fn intel_gpu_top_parser_handles_empty() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("intel_gpu.csv");
        fs::write(&csv, "timestamp_ns,compute_pct\n").unwrap();
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
        let evs = LinuxIntelGpuTopParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn intel_gpu_top_parser_emits_events_from_sample_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("intel_gpu.csv");
        let body = "timestamp_ns,compute_pct\n\
                    1000,42.0\n\
                    2000,73.5\n";
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
        let evs = LinuxIntelGpuTopParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(evs.len(), 2);
        match &evs[0].payload {
            EventPayload::GpuUtilSample {
                device_id,
                compute_pct,
                mem_pct,
                mem_used_bytes,
                ..
            } => {
                assert_eq!(*device_id, 0);
                assert!((*compute_pct - 42.0).abs() < 0.01);
                assert_eq!(*mem_pct, 0.0);
                assert_eq!(*mem_used_bytes, 0);
            }
            _ => panic!("wrong payload"),
        }
    }

    #[test]
    fn parser_extracts_render_engine_busy() {
        let json = r#"{
            "engines": {
                "Render/3D/0": {"busy": 47.3, "sema": 0.0, "wait": 0.0},
                "Blitter/0": {"busy": 0.0}
            },
            "frequency": {"actual": 1100}
        }"#;
        let pct = parse_intel_gpu_top_sample(json).unwrap();
        assert!((pct - 47.3).abs() < 0.01, "got {pct}");
    }

    #[test]
    fn parser_falls_back_to_render_prefix() {
        let json = r#"{"engines": {"Render": {"busy": 12.5}}}"#;
        let pct = parse_intel_gpu_top_sample(json).unwrap();
        assert!((pct - 12.5).abs() < 0.01);
    }

    #[test]
    fn parser_returns_none_on_missing_engines() {
        assert!(parse_intel_gpu_top_sample("{}").is_none());
        assert!(parse_intel_gpu_top_sample("garbage").is_none());
    }
}
