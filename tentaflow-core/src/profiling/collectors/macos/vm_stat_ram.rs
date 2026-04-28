// =============================================================================
// File: collectors/macos/vm_stat_ram.rs — macOS RAM collector backed by
// `vm_stat 0.5`. Spawns the binary, parses the multi-line frames it emits,
// computes used/available bytes and page-fault rate per period, and appends
// the result to `ram.csv` for the parser stage.
// =============================================================================

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner, PlatformSet,
    ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "macos.vm_stat.ram";
const CSV_FILENAME: &str = "ram.csv";
const VM_STAT_BIN: &str = "/usr/bin/vm_stat";

/// Default Apple Silicon page size (16 KiB). Used as a fallback if the
/// `vm_stat` header cannot be parsed; the real value is harvested from the
/// header line `(page size of N bytes)` whenever the child emits it.
const DEFAULT_PAGE_SIZE: u64 = 16 * 1024;

pub struct MacosVmStatRamCollector {
    capability: CollectorCapability,
    id: String,
}

impl MacosVmStatRamCollector {
    pub fn new() -> Self {
        let capability = CollectorCapability {
            categories: vec![EventCategory::RamSample],
            elevation: ElevationRequirement::None,
            platforms: PlatformSet::from_flags(PlatformSet::MACOS_X64 | PlatformSet::MACOS_ARM64),
            vendor: None,
            description: "macOS RAM usage and page fault rate via vm_stat at 2 Hz.",
        };
        Self {
            capability,
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for MacosVmStatRamCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for MacosVmStatRamCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        #[cfg(target_os = "macos")]
        {
            if std::path::Path::new(VM_STAT_BIN).exists() {
                ProbeResult::Available { version: None }
            } else {
                ProbeResult::Unavailable {
                    reason: format!("vm_stat binary not found at {VM_STAT_BIN}"),
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            ProbeResult::Unavailable {
                reason: "vm_stat is macOS-only".into(),
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
                "macos.vm_stat.ram only runs on macOS".into(),
            ))
        }
    }
}

// =============================================================================
// Parser — converts the appended `ram.csv` rows into RamSample events.
// =============================================================================

pub struct MacosVmStatRamParser;

impl CollectorParser for MacosVmStatRamParser {
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
                continue; // header
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut cols = trimmed.split(',');
            let ts: u64 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let used: u64 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let avail: u64 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let faults: u64 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);

            let t_ns = ts.saturating_sub(ctx.t0_monotonic_ns);

            out.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: t_ns,
                t_end_ns: t_ns,
                category: EventCategory::RamSample,
                lane_hint: 0,
                payload: EventPayload::RamSample {
                    used_bytes: used,
                    available_bytes: avail,
                    page_faults_per_s: faults,
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
// Header parser — pulls the page size from `vm_stat`'s header line. Exposed
// to tests at module level (not gated on target_os) because the parser logic
// is pure string manipulation.
// =============================================================================

/// Parse `Mach Virtual Memory Statistics: (page size of N bytes)` and return
/// `N`. Returns `None` if the line does not contain the marker.
fn parse_page_size(header: &str) -> Option<u64> {
    let marker = "page size of ";
    let i = header.find(marker)?;
    let tail = &header[i + marker.len()..];
    let end = tail.find(' ')?;
    tail[..end].parse().ok()
}

/// Parse one line of the form `<key>: <value>.` and return `(key, value)`.
/// `vm_stat` separates the key with whitespace-padded colon and terminates the
/// number with a `.`. Returns `None` for header lines and unrecognised forms.
fn parse_kv_line(line: &str) -> Option<(String, u64)> {
    let (key, val) = line.split_once(':')?;
    let key = key.trim().to_string();
    let val = val.trim().trim_end_matches('.');
    let n: u64 = val.parse().ok()?;
    Some((key, n))
}

// =============================================================================
// macOS implementation — child process driver that runs in a std::thread.
// =============================================================================

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Child, Command, Stdio};
    use std::sync::Mutex;
    use std::thread::JoinHandle;
    use std::time::Instant;

    pub(super) fn start(ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        std::fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);

        // Initialise CSV with header (truncate any leftover file).
        {
            let mut f = std::fs::File::create(&csv_path)?;
            writeln!(
                f,
                "timestamp_ns,used_bytes,available_bytes,page_faults_per_s"
            )?;
        }

        let mut child = Command::new(VM_STAT_BIN)
            .arg("0.5")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| CollectorError::Spawn(format!("vm_stat spawn: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CollectorError::Spawn("vm_stat stdout missing".into()))?;
        let child_pid = child.id();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(AtomicU64::new(0));

        let csv_path_thread = csv_path.clone();
        let stop_flag_thread = stop_flag.clone();
        let samples_thread = samples.clone();

        let handle: JoinHandle<()> = std::thread::Builder::new()
            .name("macos.vm_stat".into())
            .spawn(move || {
                run_reader(
                    BufReader::new(stdout),
                    csv_path_thread,
                    stop_flag_thread,
                    samples_thread,
                );
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

    /// Aggregated counters parsed out of one `vm_stat` frame.
    #[derive(Default)]
    struct Frame {
        free: u64,
        active: u64,
        inactive: u64,
        wired: u64,
        speculative: u64,
        purgeable: u64,
        compressed: u64,
        faults: Option<u64>,
        pageins: u64,
        pageouts: u64,
    }

    impl Frame {
        fn assign(&mut self, key: &str, value: u64) {
            match key {
                "Pages free" => self.free = value,
                "Pages active" => self.active = value,
                "Pages inactive" => self.inactive = value,
                "Pages wired down" => self.wired = value,
                "Pages speculative" => self.speculative = value,
                "Pages purgeable" => self.purgeable = value,
                "Pages occupied by compressor" => self.compressed = value,
                "Faults" => self.faults = Some(value),
                "Pageins" => self.pageins = value,
                "Pageouts" => self.pageouts = value,
                _ => {}
            }
        }
    }

    fn run_reader<R: BufRead>(
        reader: R,
        csv_path: PathBuf,
        stop_flag: Arc<AtomicBool>,
        samples: Arc<AtomicU64>,
    ) {
        let mut page_size = DEFAULT_PAGE_SIZE;
        let mut current = Frame::default();
        let mut prev_fault_count: Option<u64> = None;
        let started = Instant::now();

        // vm_stat's interval mode prints a header once, then table rows on a
        // 0.5 s tick. We treat the input as a stream of `key: value.` lines and
        // emit one CSV row whenever we have collected the full set of fields
        // needed to compute used/avail.
        for line in reader.lines() {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }
            let Ok(line) = line else { break };

            // Look for the first-time header carrying the page size.
            if let Some(ps) = parse_page_size(&line) {
                page_size = ps;
                continue;
            }

            // Skip the secondary tabular header (the column names line).
            if line.trim_start().starts_with("free") {
                continue;
            }

            let Some((key, value)) = parse_kv_line(&line) else {
                continue;
            };

            current.assign(&key, value);

            // We commit a row when we see the last interesting key in vm_stat
            // output ("Pageouts") which appears near the end of every frame.
            if key == "Pageouts" {
                let used_pages = current
                    .active
                    .saturating_add(current.wired)
                    .saturating_add(current.compressed);
                let avail_pages = current
                    .free
                    .saturating_add(current.inactive)
                    .saturating_add(current.speculative)
                    .saturating_add(current.purgeable);
                let used_bytes = used_pages.saturating_mul(page_size);
                let available_bytes = avail_pages.saturating_mul(page_size);

                let fault_total = current
                    .faults
                    .unwrap_or_else(|| current.pageins.saturating_add(current.pageouts));
                // 0.5 s sample interval -> per-second rate is 2 * delta.
                let faults_rate = match prev_fault_count {
                    Some(prev) => fault_total.saturating_sub(prev).saturating_mul(2),
                    None => 0,
                };
                prev_fault_count = Some(fault_total);

                let ts_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&csv_path) {
                    let _ = writeln!(f, "{ts_ns},{used_bytes},{available_bytes},{faults_rate}");
                }
                samples.fetch_add(1, Ordering::Relaxed);
                current = Frame::default();
            }
        }
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
            // SIGTERM tells vm_stat to flush + exit cleanly.
            // SAFETY: libc::kill takes a pid_t; we own the child and have not
            // yet awaited it.
            unsafe {
                libc::kill(self.child_pid as libc::pid_t, libc::SIGTERM);
            }

            if let Some(mut child) = self
                .child
                .lock()
                .map_err(|_| CollectorError::Custom("vm_stat child mutex poisoned".into()))?
                .take()
            {
                let _ = child.wait();
            }
            if let Some(handle) = self
                .reader
                .lock()
                .map_err(|_| CollectorError::Custom("vm_stat reader mutex poisoned".into()))?
                .take()
            {
                let _ = handle.join();
            }

            let mut metadata: HashMap<String, String> = HashMap::new();
            metadata.insert("source".into(), "vm_stat".into());
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
    use tentaflow_protocol::profiling::GpuVendor;

    #[test]
    fn vm_stat_collector_default_id_and_capability() {
        let c = MacosVmStatRamCollector::new();
        assert_eq!(c.id(), "macos.vm_stat.ram");
        let cap = c.capability();
        assert_eq!(cap.elevation, ElevationRequirement::None);
        assert!(cap.categories.contains(&EventCategory::RamSample));
        assert!(cap.platforms.contains(PlatformSet::MACOS_X64));
        assert!(cap.platforms.contains(PlatformSet::MACOS_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::LINUX_X64));
        let _: Option<GpuVendor> = cap.vendor;
        assert!(cap.vendor.is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn vm_stat_probe_smoke_macos() {
        let c = MacosVmStatRamCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => panic!("vm_stat must not require elevation"),
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn vm_stat_probe_returns_unavailable_on_non_macos() {
        let c = MacosVmStatRamCollector::new();
        assert!(matches!(c.probe(), ProbeResult::Unavailable { .. }));
    }

    #[test]
    fn vm_stat_parses_page_size_from_header() {
        let header = "Mach Virtual Memory Statistics: (page size of 16384 bytes)";
        assert_eq!(parse_page_size(header), Some(16384));
        let header_intel = "Mach Virtual Memory Statistics: (page size of 4096 bytes)";
        assert_eq!(parse_page_size(header_intel), Some(4096));
        assert_eq!(parse_page_size("not a header"), None);
    }

    #[test]
    fn vm_stat_parses_kv_line() {
        let (k, v) = parse_kv_line("Pages free:                          12345.").unwrap();
        assert_eq!(k, "Pages free");
        assert_eq!(v, 12345);
        // Header / column line is rejected because there is no `: number.`.
        assert!(parse_kv_line("free active spec wired").is_none());
    }

    #[test]
    fn vm_stat_parser_handles_empty_csv() {
        use std::collections::HashMap;
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("ram.csv");
        std::fs::write(
            &csv,
            "timestamp_ns,used_bytes,available_bytes,page_faults_per_s\n",
        )
        .unwrap();
        let raw = RawCapture {
            artifacts: vec![csv],
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.into(),
                pairs: vec![],
            },
            samples_observed: 0,
        };
        let ctx = SessionCtx {
            session_id: "s".into(),
            t0_monotonic_ns: 0,
            t0_wallclock_unix_ns: 0,
            output_dir: dir.path().to_path_buf(),
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
        };
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let events = MacosVmStatRamParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn vm_stat_parser_emits_events() {
        use std::collections::HashMap;
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("ram.csv");
        let body = "timestamp_ns,used_bytes,available_bytes,page_faults_per_s\n\
                    1000,2048,4096,10\n\
                    2000,3072,3072,12\n";
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
        let ctx = SessionCtx {
            session_id: "s".into(),
            t0_monotonic_ns: 0,
            t0_wallclock_unix_ns: 0,
            output_dir: dir.path().to_path_buf(),
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
        };
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let events = MacosVmStatRamParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(events.len(), 2);
        match &events[0].payload {
            EventPayload::RamSample {
                used_bytes,
                available_bytes,
                page_faults_per_s,
            } => {
                assert_eq!(*used_bytes, 2048);
                assert_eq!(*available_bytes, 4096);
                assert_eq!(*page_faults_per_s, 10);
            }
            other => panic!("unexpected payload {other:?}"),
        }
    }
}
