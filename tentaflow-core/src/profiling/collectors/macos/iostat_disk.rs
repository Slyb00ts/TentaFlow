// =============================================================================
// File: collectors/macos/iostat_disk.rs — macOS disk throughput collector
// backed by `iostat -d -w 1 -K`. macOS iostat reports a unified KB/s plus
// transfers-per-second column; it does NOT split read vs write throughput nor
// provide per-IO latency, so we surface the unified number in `read_bps` and
// document the limit via metadata for the parser stage.
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

const COLLECTOR_ID: &str = "macos.iostat.disk";
const CSV_FILENAME: &str = "disk.csv";
const IOSTAT_BIN: &str = "/usr/sbin/iostat";

pub struct MacosIostatDiskCollector {
    capability: CollectorCapability,
    id: String,
}

impl MacosIostatDiskCollector {
    pub fn new() -> Self {
        let capability = CollectorCapability {
            categories: vec![EventCategory::DiskIoBurst],
            elevation: ElevationRequirement::None,
            platforms: PlatformSet::from_flags(
                PlatformSet::MACOS_X64 | PlatformSet::MACOS_ARM64,
            ),
            vendor: None,
            description: "macOS disk throughput and IOPS via iostat at 1 Hz. Read/write split unavailable on this platform.",
        };
        Self {
            capability,
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for MacosIostatDiskCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for MacosIostatDiskCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        #[cfg(target_os = "macos")]
        {
            if std::path::Path::new(IOSTAT_BIN).exists() {
                ProbeResult::Available { version: None }
            } else {
                ProbeResult::Unavailable {
                    reason: format!("iostat binary not found at {IOSTAT_BIN}"),
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            ProbeResult::Unavailable {
                reason: "iostat (BSD-style) is macOS-only".into(),
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
                "macos.iostat.disk only runs on macOS".into(),
            ))
        }
    }
}

// =============================================================================
// Parser.
// =============================================================================

pub struct MacosIostatDiskParser;

impl CollectorParser for MacosIostatDiskParser {
    fn parse(
        &self,
        raw: RawCapture,
        ctx: &SessionCtx,
        names: &mut NameInterner,
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

        // Cache device-name → interned id within this parse pass so identical
        // labels across rows resolve in O(1) without re-hashing into the global
        // interner per event.
        let mut device_ids: HashMap<String, u32> = HashMap::new();

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
            let device_label: &str = cols.next().unwrap_or("disk0");
            let read_bps: u64 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let write_bps: u64 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let iops_r: u32 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let iops_w: u32 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let await_ms: f32 = cols.next().and_then(|s| s.parse().ok()).unwrap_or(0.0);

            let device_name_id = match device_ids.get(device_label) {
                Some(id) => *id,
                None => {
                    let id = names.intern(device_label);
                    device_ids.insert(device_label.to_string(), id);
                    id
                }
            };

            let t_ns = ts.saturating_sub(ctx.t0_monotonic_ns);
            out.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: t_ns,
                t_end_ns: t_ns,
                category: EventCategory::DiskIoBurst,
                lane_hint: 0,
                payload: EventPayload::DiskIoBurst {
                    device_name_id,
                    read_bps,
                    write_bps,
                    iops_r,
                    iops_w,
                    await_ms_p99: await_ms,
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
// `iostat -d -w 1` line parsing — module-level so it is testable on any host.
// Header line shape:
//     "              disk0           disk1"
// Then a sub-header:
//     "    KB/t  tps  MB/s     KB/t  tps  MB/s"
// Then sample rows (one per interval):
//     "   16.32   45  0.72     8.10   12  0.10"
// =============================================================================

/// Devices learned from the top-level header. macOS `iostat -d` prints them
/// once at session start.
fn parse_device_header(line: &str) -> Vec<String> {
    line.split_whitespace()
        .filter(|t| t.starts_with("disk"))
        .map(|t| t.to_string())
        .collect()
}

/// Parse a sample row given the device list we observed in the header.
/// Returns one `(device, read_bps, write_bps_unused, iops, await_ms_unused)`
/// tuple per device. Per-device `iostat` row carries three numbers per
/// disk: KB/transfer, tps, MB/s — we keep tps as IOPS and convert MB/s
/// to bytes per second.
fn parse_sample_row(line: &str, devices: &[String]) -> Vec<(String, u64, u32)> {
    let nums: Vec<f64> = line
        .split_whitespace()
        .filter_map(|s| s.parse::<f64>().ok())
        .collect();
    let mut out = Vec::new();
    for (i, dev) in devices.iter().enumerate() {
        let base = i * 3;
        if base + 2 >= nums.len() {
            break;
        }
        let tps = nums[base + 1];
        let mb_s = nums[base + 2];
        let bps = (mb_s * 1_000_000.0) as u64; // MB == 10^6 in iostat output
        let iops = tps as u32;
        out.push((dev.clone(), bps, iops));
    }
    out
}

// =============================================================================
// macOS-only spawn.
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
        {
            let mut f = std::fs::File::create(&csv_path)?;
            writeln!(
                f,
                "timestamp_ns,device,read_bps,write_bps,iops_r,iops_w,await_ms"
            )?;
        }

        let mut child = Command::new(IOSTAT_BIN)
            .args(["-d", "-w", "1", "-K"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| CollectorError::Spawn(format!("iostat spawn: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CollectorError::Spawn("iostat stdout missing".into()))?;
        let child_pid = child.id();

        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples = Arc::new(AtomicU64::new(0));

        let csv_path_thread = csv_path.clone();
        let stop_flag_thread = stop_flag.clone();
        let samples_thread = samples.clone();

        let handle: JoinHandle<()> = std::thread::Builder::new()
            .name("macos.iostat".into())
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

    fn run_reader<R: BufRead>(
        reader: R,
        csv_path: PathBuf,
        stop_flag: Arc<AtomicBool>,
        samples: Arc<AtomicU64>,
    ) {
        let mut devices: Vec<String> = Vec::new();
        let started = Instant::now();
        let mut saw_subheader = false;

        for line in reader.lines() {
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }
            let Ok(line) = line else { break };

            // The top header carries device names ("disk0  disk1 ...").
            if line.contains("disk") && !line.contains("KB/t") {
                let devs = parse_device_header(&line);
                if !devs.is_empty() {
                    devices = devs;
                    saw_subheader = false;
                    continue;
                }
            }
            // Sub-header — column labels. We just remember we have crossed it.
            if line.contains("KB/t") && line.contains("tps") {
                saw_subheader = true;
                continue;
            }
            if !saw_subheader || devices.is_empty() {
                continue;
            }
            // Sample row.
            let rows = parse_sample_row(&line, &devices);
            if rows.is_empty() {
                continue;
            }
            let ts_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
            if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&csv_path) {
                for (dev, bps, iops) in rows {
                    let _ = writeln!(f, "{ts_ns},{dev},{bps},0,{iops},0,0");
                    samples.fetch_add(1, Ordering::Relaxed);
                }
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
            unsafe {
                libc::kill(self.child_pid as libc::pid_t, libc::SIGTERM);
            }
            if let Some(mut child) = self
                .child
                .lock()
                .map_err(|_| CollectorError::Custom("iostat child mutex poisoned".into()))?
                .take()
            {
                let _ = child.wait();
            }
            if let Some(handle) = self
                .reader
                .lock()
                .map_err(|_| CollectorError::Custom("iostat reader mutex poisoned".into()))?
                .take()
            {
                let _ = handle.join();
            }

            let mut metadata: HashMap<String, String> = HashMap::new();
            metadata.insert("source".into(), "iostat".into());
            metadata.insert("interval_seconds".into(), "1".into());
            metadata.insert(
                "iostat_kind".into(),
                "macos_unified (no read/write split, no per-iop latency)".into(),
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
    fn iostat_collector_default_id_and_capability() {
        let c = MacosIostatDiskCollector::new();
        assert_eq!(c.id(), "macos.iostat.disk");
        let cap = c.capability();
        assert_eq!(cap.elevation, ElevationRequirement::None);
        assert!(cap.categories.contains(&EventCategory::DiskIoBurst));
        assert!(cap.platforms.contains(PlatformSet::MACOS_X64));
        assert!(cap.platforms.contains(PlatformSet::MACOS_ARM64));
        assert!(cap.vendor.is_none());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn iostat_probe_smoke_macos() {
        let c = MacosIostatDiskCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => panic!("iostat must not require elevation"),
        }
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn iostat_probe_returns_unavailable_on_non_macos() {
        let c = MacosIostatDiskCollector::new();
        assert!(matches!(c.probe(), ProbeResult::Unavailable { .. }));
    }

    #[test]
    fn iostat_parses_device_header() {
        let h = "              disk0           disk1           disk2";
        let devs = parse_device_header(h);
        assert_eq!(devs, vec!["disk0", "disk1", "disk2"]);
        let no_devs = parse_device_header("    KB/t  tps  MB/s");
        assert!(no_devs.is_empty());
    }

    #[test]
    fn iostat_parses_sample_row() {
        // Two-disk row: 16.32 KB/t, 45 tps, 0.72 MB/s | 8.10, 12, 0.10.
        let row = "   16.32   45  0.72     8.10   12  0.10";
        let devs = vec!["disk0".to_string(), "disk1".to_string()];
        let parsed = parse_sample_row(row, &devs);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, "disk0");
        assert_eq!(parsed[0].1, 720_000); // 0.72 MB/s
        assert_eq!(parsed[0].2, 45);
        assert_eq!(parsed[1].0, "disk1");
        assert_eq!(parsed[1].1, 100_000);
        assert_eq!(parsed[1].2, 12);
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
    fn iostat_parser_handles_empty_csv() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("disk.csv");
        std::fs::write(
            &csv,
            "timestamp_ns,device,read_bps,write_bps,iops_r,iops_w,await_ms\n",
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
        let ctx = empty_ctx(dir.path());
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let events = MacosIostatDiskParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn iostat_parser_emits_events() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("disk.csv");
        let body = "timestamp_ns,device,read_bps,write_bps,iops_r,iops_w,await_ms\n\
                    1000,disk0,720000,0,45,0,0\n\
                    2000,disk1,100000,0,12,0,0\n";
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
        let events = MacosIostatDiskParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(events.len(), 2);
        let names_vec = names.into_vec();
        match &events[0].payload {
            EventPayload::DiskIoBurst {
                device_name_id,
                read_bps,
                write_bps,
                iops_r,
                iops_w,
                await_ms_p99,
            } => {
                assert_eq!(names_vec[*device_name_id as usize], "disk0");
                assert_eq!(*read_bps, 720_000);
                assert_eq!(*write_bps, 0);
                assert_eq!(*iops_r, 45);
                assert_eq!(*iops_w, 0);
                assert!((*await_ms_p99 - 0.0).abs() < f32::EPSILON);
            }
            other => panic!("unexpected payload {other:?}"),
        }
    }
}
