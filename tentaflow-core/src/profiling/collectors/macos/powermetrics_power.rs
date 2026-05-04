#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]
// =============================================================================
// File: collectors/macos/powermetrics_power.rs — macOS CPU/DRAM/ANE/SoC power
// collector backed by `sudo powermetrics --samplers cpu_power -i 500 -n 0
// --format plist`. Parses the streaming plist frames, extracts per-domain
// power readings in watts and persists them to `power.csv`.
// =============================================================================

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, PowerDomain, TimelineEvent,
};

use crate::profiling::collectors::elevation::ElevationKind;
use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner, PlatformSet,
    ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "macos.powermetrics.power";
const CSV_FILENAME: &str = "power.csv";
const POWERMETRICS_BIN: &str = "/usr/bin/powermetrics";

pub struct MacosPowermetricsPowerCollector {
    capability: CollectorCapability,
    id: String,
}

impl MacosPowermetricsPowerCollector {
    pub fn new() -> Self {
        let capability = CollectorCapability {
            categories: vec![EventCategory::PowerSample],
            elevation: ElevationRequirement::Sudo,
            platforms: PlatformSet::from_flags(
                PlatformSet::MACOS_X64 | PlatformSet::MACOS_ARM64,
            ),
            vendor: None,
            description: "macOS CPU package, DRAM, ANE and SoC total power via powermetrics at 2 Hz. Requires sudo.",
        };
        Self {
            capability,
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for MacosPowermetricsPowerCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for MacosPowermetricsPowerCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        #[cfg(target_os = "macos")]
        {
            if !std::path::Path::new(POWERMETRICS_BIN).exists() {
                return ProbeResult::Unavailable {
                    reason: format!("powermetrics binary not found at {POWERMETRICS_BIN}"),
                };
            }
            // SAFETY: getuid is reentrant and side-effect-free.
            let uid = unsafe { libc::getuid() };
            if uid == 0 {
                ProbeResult::Available { version: None }
            } else {
                ProbeResult::NeedsElevation {
                    kind: ElevationKind::Sudo,
                    reason: "powermetrics requires root for hardware counters".into(),
                }
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
        #[cfg(target_os = "macos")]
        {
            macos_impl::start(ctx)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = ctx;
            Err(CollectorError::Custom(
                "macos.powermetrics.power only runs on macOS".into(),
            ))
        }
    }
}

// =============================================================================
// Parser.
// =============================================================================

pub struct MacosPowermetricsPowerParser;

impl CollectorParser for MacosPowermetricsPowerParser {
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
            let domain = cols.next().unwrap_or("Other");
            let domain_idx: u32 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let watts: f32 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);

            let dom = match domain {
                "CpuPkg" => PowerDomain::CpuPkg,
                "CpuCore" => PowerDomain::CpuCore,
                "Dram" => PowerDomain::Dram,
                "Gpu" => PowerDomain::Gpu(domain_idx),
                "Ane" => PowerDomain::Ane,
                "Soc" => PowerDomain::Soc,
                _ => PowerDomain::Other,
            };
            let t_ns = ts.saturating_sub(ctx.t0_monotonic_ns);
            out.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: t_ns,
                t_end_ns: t_ns,
                category: EventCategory::PowerSample,
                lane_hint: 0,
                payload: EventPayload::PowerSample { domain: dom, watts },
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
// Plist sample parsing — module-level so it is reachable from tests.
// =============================================================================

/// One extracted reading from a powermetrics plist frame.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PowerReading {
    pub domain: PowerDomain,
    pub watts: f32,
}

#[cfg(target_os = "macos")]
fn extract_f64(v: &plist::Value, path: &[&str]) -> Option<f64> {
    let mut cur = v;
    for key in path {
        let dict = cur.as_dictionary()?;
        cur = dict.get(key)?;
    }
    cur.as_real()
        .or_else(|| cur.as_signed_integer().map(|i| i as f64))
}

/// Convert one parsed plist frame into the readings we surface as
/// `PowerSample` events. We intentionally skip GPU here — the GPU collector
/// owns that domain to avoid duplicate events when both samplers are active.
///
/// Field name landscape:
/// - `processor.cpu_power` -> CPU package (mW on most builds, W on others).
/// - `processor.dram_power` -> DRAM rail.
/// - `processor.ane_power` -> Apple Neural Engine (Apple Silicon only).
/// - `processor.combined_power` -> total SoC power.
///
/// Newer powermetrics builds emit values in **mW** (milli-watts). The
/// historical builds used watts. We disambiguate by magnitude: any reading
/// above 200 W is assumed to be mW and divided by 1000.
#[cfg(target_os = "macos")]
fn extract_power_readings(plist_root: &plist::Value) -> Vec<PowerReading> {
    let mut out = Vec::new();
    let pairs: &[(&str, PowerDomain)] = &[
        ("cpu_power", PowerDomain::CpuPkg),
        ("dram_power", PowerDomain::Dram),
        ("ane_power", PowerDomain::Ane),
        ("combined_power", PowerDomain::Soc),
    ];
    for (field, domain) in pairs {
        if let Some(raw) = extract_f64(plist_root, &["processor", field]) {
            let watts = if raw > 200.0 { raw / 1000.0 } else { raw };
            out.push(PowerReading {
                domain: domain.clone(),
                watts: watts as f32,
            });
        }
    }
    out
}

// =============================================================================
// macOS spawn / reader.
// =============================================================================

#[cfg(target_os = "macos")]
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
            writeln!(f, "timestamp_ns,domain,domain_idx,watts")?;
        }

        // Build either a direct or a `sudo -S` invocation depending on whether
        // we already run as root.
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
            "cpu_power",
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
            .map_err(|e| CollectorError::Spawn(format!("powermetrics spawn: {e}")))?;

        // Feed sudo password (if any) on stdin and close it so sudo proceeds.
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(token) = ctx.elevation.as_ref() {
                if token.kind() == ElevationKind::Sudo {
                    // Best-effort write — failure means sudo will prompt again
                    // on the TTY; we still return the child and let the reader
                    // surface the error path through stop().
                    let _ = stdin.write_all(token.as_secret_bytes());
                    let _ = stdin.write_all(b"\n");
                }
            }
            // Drop closes the pipe.
            drop(stdin);
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CollectorError::Spawn("powermetrics stdout missing".into()))?;
        let child_pid = child.id();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(AtomicU64::new(0));
        let csv_path_thread = csv_path.clone();
        let stop_flag_thread = stop_flag.clone();
        let samples_thread = samples.clone();

        let handle: JoinHandle<()> = std::thread::Builder::new()
            .name("macos.powermetrics.power".into())
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
        // Streaming buffer: powermetrics emits one full plist document per
        // sample, separated by `\0` bytes (NUL) historically, or by repeated
        // `<?xml ...?>` headers. We split on `</plist>\n` which is universal.
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

            // Drain every complete `</plist>` we have buffered.
            while let Some(end) = find_subslice(&buf, terminator) {
                let doc_end = end + terminator.len();
                let doc = buf[..doc_end].to_vec();
                buf.drain(..doc_end);
                // Skip leading garbage (NULs, stray `\n`).
                let trimmed = trim_leading_garbage(&doc);
                if trimmed.is_empty() {
                    continue;
                }
                if let Ok(value) = plist::Value::from_reader(std::io::Cursor::new(trimmed)) {
                    let readings = extract_power_readings(&value);
                    if readings.is_empty() {
                        continue;
                    }
                    let ts_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&csv_path) {
                        for r in &readings {
                            let (name, idx) = match r.domain {
                                PowerDomain::CpuPkg => ("CpuPkg", 0),
                                PowerDomain::CpuCore => ("CpuCore", 0),
                                PowerDomain::Dram => ("Dram", 0),
                                PowerDomain::Gpu(i) => ("Gpu", i),
                                PowerDomain::Ane => ("Ane", 0),
                                PowerDomain::Soc => ("Soc", 0),
                                PowerDomain::Other => ("Other", 0),
                            };
                            let _ = writeln!(f, "{ts_ns},{name},{idx},{:.3}", r.watts);
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
            // Need to use SIGTERM via sudo when we spawned through sudo —
            // direct SIGTERM is rejected because the child's effective uid is
            // root. We send SIGTERM via `sudo -n kill` as a best effort, then
            // fall back to killing the launching `sudo` shim.
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
                .map_err(|_| CollectorError::Custom("powermetrics child mutex poisoned".into()))?
                .take()
            {
                let _ = child.wait();
            }
            if let Some(handle) = self
                .reader
                .lock()
                .map_err(|_| CollectorError::Custom("powermetrics reader mutex poisoned".into()))?
                .take()
            {
                let _ = handle.join();
            }

            let mut metadata: HashMap<String, String> = HashMap::new();
            metadata.insert("source".into(), "powermetrics".into());
            metadata.insert("samplers".into(), "cpu_power".into());
            metadata.insert("interval_seconds".into(), "0.5".into());
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
    fn powermetrics_power_default_id_and_capability() {
        let c = MacosPowermetricsPowerCollector::new();
        assert_eq!(c.id(), "macos.powermetrics.power");
        let cap = c.capability();
        assert_eq!(cap.elevation, ElevationRequirement::Sudo);
        assert!(cap.categories.contains(&EventCategory::PowerSample));
        assert!(cap.platforms.contains(PlatformSet::MACOS_X64));
        assert!(cap.platforms.contains(PlatformSet::MACOS_ARM64));
        assert!(cap.vendor.is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn powermetrics_power_probe_smoke_macos() {
        let c = MacosPowermetricsPowerCollector::new();
        match c.probe() {
            ProbeResult::Available { .. }
            | ProbeResult::Unavailable { .. }
            | ProbeResult::NeedsElevation {
                kind: ElevationKind::Sudo,
                ..
            } => {}
            ProbeResult::NeedsElevation { kind, .. } => {
                panic!("powermetrics power must require Sudo, got {kind:?}");
            }
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn powermetrics_power_probe_returns_unavailable_on_non_macos() {
        let c = MacosPowermetricsPowerCollector::new();
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
    fn powermetrics_power_parser_handles_empty_csv() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("power.csv");
        std::fs::write(&csv, "timestamp_ns,domain,domain_idx,watts\n").unwrap();
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
        let events = MacosPowermetricsPowerParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn powermetrics_power_parser_emits_events() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("power.csv");
        let body = "timestamp_ns,domain,domain_idx,watts\n\
                    1000,CpuPkg,0,3.250\n\
                    1000,Dram,0,1.100\n\
                    1000,Ane,0,0.050\n\
                    1000,Soc,0,5.500\n";
        std::fs::write(&csv, body).unwrap();
        let raw = RawCapture {
            artifacts: vec![csv],
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.into(),
                pairs: vec![],
            },
            samples_observed: 4,
        };
        let ctx = empty_ctx(dir.path());
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let events = MacosPowermetricsPowerParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(events.len(), 4);
        let domains: Vec<PowerDomain> = events
            .iter()
            .map(|e| match &e.payload {
                EventPayload::PowerSample { domain, .. } => domain.clone(),
                _ => unreachable!(),
            })
            .collect();
        assert!(domains.contains(&PowerDomain::CpuPkg));
        assert!(domains.contains(&PowerDomain::Dram));
        assert!(domains.contains(&PowerDomain::Ane));
        assert!(domains.contains(&PowerDomain::Soc));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn powermetrics_power_parses_plist_sample() {
        // Synthetic plist mirrored on the structure powermetrics emits with
        // `--samplers cpu_power -f plist`. Values are in watts (legacy build
        // shape); the parser's mW/W heuristic leaves them untouched.
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>processor</key>
    <dict>
        <key>cpu_power</key>     <real>2.500</real>
        <key>dram_power</key>    <real>0.750</real>
        <key>ane_power</key>     <real>0.050</real>
        <key>combined_power</key><real>4.200</real>
        <key>gpu_power</key>     <real>0.900</real>
    </dict>
</dict>
</plist>"#;
        let v = plist::Value::from_reader(std::io::Cursor::new(xml)).unwrap();
        let readings = extract_power_readings(&v);
        // Must NOT contain GPU — that is the gpu collector's responsibility.
        assert!(readings
            .iter()
            .all(|r| !matches!(r.domain, PowerDomain::Gpu(_))));
        let cpu = readings
            .iter()
            .find(|r| matches!(r.domain, PowerDomain::CpuPkg))
            .unwrap();
        assert!((cpu.watts - 2.5).abs() < 1e-3);
        let dram = readings
            .iter()
            .find(|r| matches!(r.domain, PowerDomain::Dram))
            .unwrap();
        assert!((dram.watts - 0.75).abs() < 1e-3);
        let ane = readings
            .iter()
            .find(|r| matches!(r.domain, PowerDomain::Ane))
            .unwrap();
        assert!((ane.watts - 0.05).abs() < 1e-3);
        let soc = readings
            .iter()
            .find(|r| matches!(r.domain, PowerDomain::Soc))
            .unwrap();
        assert!((soc.watts - 4.2).abs() < 1e-3);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn powermetrics_power_plist_handles_milliwatts() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>processor</key>
    <dict>
        <key>cpu_power</key>     <real>2500</real>
        <key>combined_power</key><real>5500</real>
    </dict>
</dict>
</plist>"#;
        let v = plist::Value::from_reader(std::io::Cursor::new(xml)).unwrap();
        let readings = extract_power_readings(&v);
        let cpu = readings
            .iter()
            .find(|r| matches!(r.domain, PowerDomain::CpuPkg))
            .unwrap();
        // 2500 mW => 2.5 W
        assert!((cpu.watts - 2.5).abs() < 1e-3);
        let soc = readings
            .iter()
            .find(|r| matches!(r.domain, PowerDomain::Soc))
            .unwrap();
        assert!((soc.watts - 5.5).abs() < 1e-3);
    }
}
