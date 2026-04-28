// =============================================================================
// File: collectors/macos/powermetrics_gpu.rs — Apple Silicon GPU collector
// backed by `sudo powermetrics --samplers gpu_power -i 500 -n 0 --format
// plist`. Surfaces compute utilisation (1 - idle_ratio) and GPU rail power.
// Memory util / temperature / per-kernel detail are NOT exposed by this
// sampler — Metal System Trace would be required for that.
// =============================================================================

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, GpuVendor, PowerDomain,
    TimelineEvent,
};

use crate::profiling::collectors::elevation::ElevationKind;
use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner, PlatformSet,
    ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "macos.powermetrics.gpu";
const CSV_FILENAME: &str = "gpu.csv";
const POWERMETRICS_BIN: &str = "/usr/bin/powermetrics";

pub struct MacosPowermetricsGpuCollector {
    capability: CollectorCapability,
    id: String,
}

impl MacosPowermetricsGpuCollector {
    pub fn new() -> Self {
        let capability = CollectorCapability {
            categories: vec![EventCategory::GpuUtilSample, EventCategory::PowerSample],
            elevation: ElevationRequirement::Sudo,
            // Apple-Silicon-only: powermetrics does not expose dGPU power on
            // Intel Macs even when a discrete GPU is present.
            platforms: PlatformSet::from_flag(PlatformSet::MACOS_ARM64),
            vendor: Some(GpuVendor::Apple),
            description:
                "Apple Silicon GPU utilization and power via powermetrics at 2 Hz. Kernel-level metrics not available without Metal System Trace.",
        };
        Self {
            capability,
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for MacosPowermetricsGpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for MacosPowermetricsGpuCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            if !std::path::Path::new(POWERMETRICS_BIN).exists() {
                return ProbeResult::Unavailable {
                    reason: format!("powermetrics binary not found at {POWERMETRICS_BIN}"),
                };
            }
            // SAFETY: getuid is reentrant.
            let uid = unsafe { libc::getuid() };
            if uid == 0 {
                ProbeResult::Available { version: None }
            } else {
                ProbeResult::NeedsElevation {
                    kind: ElevationKind::Sudo,
                    reason: "powermetrics gpu sampler requires root".into(),
                }
            }
        }
        #[cfg(all(target_os = "macos", not(target_arch = "aarch64")))]
        {
            ProbeResult::Unavailable {
                reason: "Apple GPU power sampler is Apple Silicon only".into(),
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            ProbeResult::Unavailable {
                reason: "powermetrics is macOS-only".into(),
            }
        }
    }

    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            macos_impl::start(ctx)
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = ctx;
            Err(CollectorError::Custom(
                "macos.powermetrics.gpu only runs on Apple Silicon macOS".into(),
            ))
        }
    }
}

// =============================================================================
// Parser.
// =============================================================================

pub struct MacosPowermetricsGpuParser;

impl CollectorParser for MacosPowermetricsGpuParser {
    fn parse(
        &self,
        raw: RawCapture,
        ctx: &SessionCtx,
        _names: &mut NameInterner,
        _frames: &mut FrameInterner,
    ) -> Result<Vec<TimelineEvent>, CollectorError> {
        let csv_path = match find_csv(&raw) {
            Some(p) => p,
            None => return Ok(Vec::new()),
        };
        let body = match std::fs::read_to_string(&csv_path) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        let mut out: Vec<TimelineEvent> = Vec::new();
        for (idx, line) in body.lines().enumerate() {
            if idx == 0 && line.starts_with("timestamp_ns,") {
                continue;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut cols = trimmed.split(',');
            let ts: u64 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let device_id: u32 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let compute_pct: f32 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);
            let power_w: f32 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);

            let t_ns = ts.saturating_sub(ctx.t0_monotonic_ns);
            out.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: t_ns,
                t_end_ns: t_ns,
                category: EventCategory::GpuUtilSample,
                lane_hint: device_id as u16,
                payload: EventPayload::GpuUtilSample {
                    device_id,
                    compute_pct,
                    mem_pct: 0.0,
                    mem_used_bytes: 0,
                    temp_c: 0.0,
                },
            });
            out.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: t_ns,
                t_end_ns: t_ns,
                category: EventCategory::PowerSample,
                lane_hint: device_id as u16,
                payload: EventPayload::PowerSample {
                    domain: PowerDomain::Gpu(device_id),
                    watts: power_w,
                },
            });
        }
        Ok(out)
    }
}

fn find_csv(raw: &RawCapture) -> Option<PathBuf> {
    raw.artifacts
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|s| s == CSV_FILENAME)
                .unwrap_or(false)
        })
        .cloned()
        .or_else(|| raw.artifacts.first().cloned())
}

// =============================================================================
// Plist extraction.
// =============================================================================

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GpuFrame {
    pub compute_pct: f32,
    pub power_w: f32,
}

#[cfg(target_os = "macos")]
fn get_dict_f64(v: &plist::Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for key in path {
        let dict = cur.as_dictionary()?;
        cur = dict.get(key)?;
    }
    cur.as_real()
        .or_else(|| cur.as_signed_integer().map(|i| i as f64))
}

/// Pull compute_pct and power_w out of one powermetrics gpu_power frame.
///
/// Field landscape across powermetrics builds:
/// - Idle ratio: `gpu.idle_ratio` (0..1).
/// - Busy ratio: occasionally `gpu.busy` (0..1) on newer builds.
/// - Power: `gpu.power` (mW) on newer builds, `processor.gpu_power` (W or mW)
///   on older ones.
#[cfg(target_os = "macos")]
fn extract_gpu_frame(plist_root: &plist::Value) -> Option<GpuFrame> {
    let busy_ratio = if let Some(busy) = get_dict_f64(plist_root, &["gpu", "busy"]) {
        busy.clamp(0.0, 1.0)
    } else if let Some(idle) = get_dict_f64(plist_root, &["gpu", "idle_ratio"]) {
        (1.0 - idle).clamp(0.0, 1.0)
    } else {
        return None;
    };

    let raw_power = get_dict_f64(plist_root, &["gpu", "power"])
        .or_else(|| get_dict_f64(plist_root, &["processor", "gpu_power"]))
        .unwrap_or(0.0);
    let watts = if raw_power > 200.0 {
        raw_power / 1000.0
    } else {
        raw_power
    };

    Some(GpuFrame {
        compute_pct: (busy_ratio * 100.0) as f32,
        power_w: watts as f32,
    })
}

// =============================================================================
// macOS spawn / reader.
// =============================================================================

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod macos_impl {
    use super::*;
    use std::io::{Read, Write};
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;
    use std::thread::JoinHandle;
    use std::time::Instant;

    pub(super) fn start(ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        std::fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);
        {
            let mut f = std::fs::File::create(&csv_path)?;
            writeln!(f, "timestamp_ns,device_id,compute_pct,power_w")?;
        }

        // SAFETY: getuid is reentrant.
        let need_sudo = unsafe { libc::getuid() } != 0;
        let mut cmd = if need_sudo {
            let mut c = Command::new("sudo");
            c.arg("-S").arg(POWERMETRICS_BIN);
            c
        } else {
            Command::new(POWERMETRICS_BIN)
        };
        cmd.args([
            "--samplers",
            "gpu_power",
            "-i",
            "500",
            "-n",
            "0",
            "--format",
            "plist",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| CollectorError::Spawn(format!("powermetrics-gpu spawn: {e}")))?;

        if let Some(mut stdin) = child.stdin.take() {
            if let Some(token) = ctx.elevation.as_ref() {
                if token.kind() == ElevationKind::Sudo {
                    let _ = stdin.write_all(token.as_secret_bytes());
                    let _ = stdin.write_all(b"\n");
                }
            }
            drop(stdin);
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CollectorError::Spawn("powermetrics-gpu stdout missing".into()))?;
        let child_pid = child.id();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(AtomicU64::new(0));
        let csv_path_thread = csv_path.clone();
        let stop_flag_thread = stop_flag.clone();
        let samples_thread = samples.clone();

        let handle: JoinHandle<()> = std::thread::Builder::new()
            .name("macos.powermetrics.gpu".into())
            .spawn(move || {
                run_reader(stdout, csv_path_thread, stop_flag_thread, samples_thread);
            })
            .map_err(|e| CollectorError::Spawn(format!("reader thread: {e}")))?;

        Ok(Box::new(Running {
            id: COLLECTOR_ID.to_string(),
            child: Mutex::new(Some(child)),
            child_pid,
            reader: Mutex::new(Some(handle)),
            stop_flag,
            samples,
            csv_path,
            started_at: Instant::now(),
        }))
    }

    fn run_reader<R: Read>(
        mut reader: R,
        csv_path: PathBuf,
        stop_flag: Arc<AtomicBool>,
        samples: Arc<AtomicU64>,
    ) {
        let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
        let mut chunk = [0u8; 4096];
        let started = Instant::now();
        let terminator = b"</plist>";

        loop {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }
            let n = match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            buf.extend_from_slice(&chunk[..n]);
            while let Some(end) = find_subslice(&buf, terminator) {
                let doc_end = end + terminator.len();
                let doc = buf[..doc_end].to_vec();
                buf.drain(..doc_end);
                let trimmed = trim_leading_garbage(&doc);
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(value) = plist::Value::from_reader(std::io::Cursor::new(trimmed)) {
                    if let Some(frame) = extract_gpu_frame(&value) {
                        let ts_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&csv_path)
                        {
                            let _ = writeln!(
                                f,
                                "{ts_ns},0,{:.3},{:.3}",
                                frame.compute_pct, frame.power_w
                            );
                            samples.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }
        }
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || haystack.len() < needle.len() {
            return None;
        }
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    fn trim_leading_garbage(s: &[u8]) -> &[u8] {
        let mut i = 0;
        while i < s.len() {
            let b = s[i];
            if b == 0 || b == b'\n' || b == b'\r' || b == b' ' || b == b'\t' {
                i += 1;
            } else {
                break;
            }
        }
        &s[i..]
    }

    pub(super) struct Running {
        id: String,
        child: Mutex<Option<Child>>,
        child_pid: u32,
        reader: Mutex<Option<JoinHandle<()>>>,
        stop_flag: Arc<AtomicBool>,
        samples: Arc<AtomicU64>,
        csv_path: PathBuf,
        started_at: Instant,
    }

    impl RunningCollector for Running {
        fn collector_id(&self) -> &str {
            &self.id
        }

        fn stop(self: Box<Self>) -> Result<RawCapture, CollectorError> {
            self.stop_flag.store(true, Ordering::Relaxed);
            unsafe {
                libc::kill(self.child_pid as libc::pid_t, libc::SIGTERM);
            }
            let _ = Command::new("sudo")
                .args(["-n", "kill", "-TERM"])
                .arg(self.child_pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();

            if let Some(mut child) = self
                .child
                .lock()
                .map_err(|_| {
                    CollectorError::Custom("powermetrics-gpu child mutex poisoned".into())
                })?
                .take()
            {
                let _ = child.wait();
            }
            if let Some(handle) = self
                .reader
                .lock()
                .map_err(|_| {
                    CollectorError::Custom("powermetrics-gpu reader mutex poisoned".into())
                })?
                .take()
            {
                let _ = handle.join();
            }

            let mut metadata: HashMap<String, String> = HashMap::new();
            metadata.insert("source".into(), "powermetrics".into());
            metadata.insert("samplers".into(), "gpu_power".into());
            metadata.insert("interval_seconds".into(), "0.5".into());
            metadata.insert(
                "memory_metrics".into(),
                "unavailable (unified memory; powermetrics does not expose per-GPU mem_used)"
                    .into(),
            );
            metadata.insert(
                "duration_ns".into(),
                self.started_at.elapsed().as_nanos().to_string(),
            );

            let artifacts = if self.csv_path.exists() {
                vec![self.csv_path.clone()]
            } else {
                Vec::new()
            };
            let elapsed_ns = u64::try_from(self.started_at.elapsed().as_nanos()).unwrap_or(0);

            Ok(RawCapture {
                artifacts,
                metadata,
                clock_samples: ClockSamples {
                    collector_id: COLLECTOR_ID.to_string(),
                    pairs: vec![(0, 0), (elapsed_ns, elapsed_ns)],
                },
                samples_observed: self.samples.load(Ordering::Relaxed),
            })
        }

        fn abort(self: Box<Self>) {
            self.stop_flag.store(true, Ordering::Relaxed);
            unsafe {
                libc::kill(self.child_pid as libc::pid_t, libc::SIGKILL);
            }
            if let Ok(mut guard) = self.child.lock() {
                if let Some(mut c) = guard.take() {
                    let _ = c.wait();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powermetrics_gpu_default_id_and_capability() {
        let c = MacosPowermetricsGpuCollector::new();
        assert_eq!(c.id(), "macos.powermetrics.gpu");
        let cap = c.capability();
        assert_eq!(cap.elevation, ElevationRequirement::Sudo);
        assert!(cap.categories.contains(&EventCategory::GpuUtilSample));
        assert!(cap.categories.contains(&EventCategory::PowerSample));
        assert!(cap.platforms.contains(PlatformSet::MACOS_ARM64));
        // Apple Silicon only: x86_64 macOS is excluded.
        assert!(!cap.platforms.contains(PlatformSet::MACOS_X64));
        assert_eq!(cap.vendor, Some(GpuVendor::Apple));
    }

    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn powermetrics_gpu_probe_smoke_macos() {
        let c = MacosPowermetricsGpuCollector::new();
        match c.probe() {
            ProbeResult::Available { .. }
            | ProbeResult::Unavailable { .. }
            | ProbeResult::NeedsElevation {
                kind: ElevationKind::Sudo,
                ..
            } => {}
            ProbeResult::NeedsElevation { kind, .. } => {
                panic!("powermetrics gpu must require Sudo, got {kind:?}");
            }
        }
    }

    #[test]
    #[cfg(any(not(target_os = "macos"), not(target_arch = "aarch64")))]
    fn powermetrics_gpu_probe_returns_unavailable_on_non_apple_silicon() {
        let c = MacosPowermetricsGpuCollector::new();
        assert!(matches!(c.probe(), ProbeResult::Unavailable { .. }));
    }

    fn empty_ctx(dir: &std::path::Path) -> SessionCtx {
        SessionCtx {
            session_id: "s".into(),
            t0_monotonic_ns: 0,
            t0_wallclock_unix_ns: 0,
            output_dir: dir.to_path_buf(),
            scope: tentaflow_protocol::profiling::ProfileScope {
                sources: tentaflow_protocol::profiling::ProfileSourceFlags::empty(),
                gpu_targets: tentaflow_protocol::profiling::GpuTargets::None,
                cpu_sampling_hz: 99,
                target: tentaflow_protocol::profiling::ProfileTarget::OwnProcess,
                duration_seconds: 0,
                label: "t".into(),
            },
            target_pid: None,
            elevation: None,
            planned_duration_ns: 0,
        }
    }

    #[test]
    fn powermetrics_gpu_parser_handles_empty_csv() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("gpu.csv");
        std::fs::write(&csv, "timestamp_ns,device_id,compute_pct,power_w\n").unwrap();
        let raw = RawCapture {
            artifacts: vec![csv],
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.into(),
                pairs: vec![],
            },
            samples_observed: 0,
        };
        let ctx = empty_ctx(dir.path());
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let events = MacosPowermetricsGpuParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn powermetrics_gpu_parser_emits_events() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("gpu.csv");
        let body = "timestamp_ns,device_id,compute_pct,power_w\n\
                    1000,0,42.500,1.250\n\
                    2000,0,80.000,3.100\n";
        std::fs::write(&csv, body).unwrap();
        let raw = RawCapture {
            artifacts: vec![csv],
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.into(),
                pairs: vec![],
            },
            samples_observed: 2,
        };
        let ctx = empty_ctx(dir.path());
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let events = MacosPowermetricsGpuParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        // 2 rows -> 2 GpuUtilSample + 2 PowerSample = 4 events.
        assert_eq!(events.len(), 4);
        let util_count = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::GpuUtilSample { .. }))
            .count();
        let power_count = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::PowerSample { .. }))
            .count();
        assert_eq!(util_count, 2);
        assert_eq!(power_count, 2);
        // Verify mem_pct=0 and temp_c=0 (documented limitation).
        match &events[0].payload {
            EventPayload::GpuUtilSample {
                mem_pct,
                mem_used_bytes,
                temp_c,
                compute_pct,
                ..
            } => {
                assert_eq!(*mem_pct, 0.0);
                assert_eq!(*mem_used_bytes, 0);
                assert_eq!(*temp_c, 0.0);
                assert!((*compute_pct - 42.5).abs() < 1e-3);
            }
            other => panic!("unexpected payload {other:?}"),
        }
        match &events[1].payload {
            EventPayload::PowerSample { domain, watts } => {
                assert_eq!(*domain, PowerDomain::Gpu(0));
                assert!((*watts - 1.25).abs() < 1e-3);
            }
            other => panic!("unexpected payload {other:?}"),
        }
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn powermetrics_gpu_parses_plist_sample_idle_ratio() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>gpu</key>
    <dict>
        <key>idle_ratio</key> <real>0.25</real>
    </dict>
    <key>processor</key>
    <dict>
        <key>gpu_power</key>  <real>1.500</real>
    </dict>
</dict>
</plist>"#;
        let v = plist::Value::from_reader(std::io::Cursor::new(xml)).unwrap();
        let frame = extract_gpu_frame(&v).unwrap();
        // 1 - 0.25 = 0.75 -> 75 %.
        assert!((frame.compute_pct - 75.0).abs() < 1e-3);
        assert!((frame.power_w - 1.5).abs() < 1e-3);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn powermetrics_gpu_parses_plist_sample_busy_field_milliwatts() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>gpu</key>
    <dict>
        <key>busy</key>  <real>0.92</real>
        <key>power</key> <real>3500</real>
    </dict>
</dict>
</plist>"#;
        let v = plist::Value::from_reader(std::io::Cursor::new(xml)).unwrap();
        let frame = extract_gpu_frame(&v).unwrap();
        assert!((frame.compute_pct - 92.0).abs() < 1e-3);
        // 3500 mW -> 3.5 W.
        assert!((frame.power_w - 3.5).abs() < 1e-3);
    }
}
