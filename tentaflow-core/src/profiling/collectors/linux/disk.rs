#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux/disk.rs — Per-device disk I/O collector reading
// /proc/diskstats at 2 Hz. Emits TimelineEvent::DiskIoBurst rows. Note: the
// `await_ms_p99` field carries the *average* await (delta_time / delta_ops);
// the kernel does not expose a true p99 without eBPF instrumentation, so we
// report avg honestly and document this in RawCapture metadata. Internal
// helpers are Linux-only by design.
// =============================================================================

use std::collections::HashMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::thread;
use std::thread::JoinHandle;
#[cfg(target_os = "linux")]
use std::time::Duration;
use std::time::Instant;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner,
    PlatformSet, ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "linux.iostat.disk";
const CSV_FILENAME: &str = "disk.csv";
#[cfg(target_os = "linux")]
const SAMPLE_PERIOD: Duration = Duration::from_millis(500);
const SECTOR_BYTES: u64 = 512;

/// Disk I/O sampler driven by /proc/diskstats.
pub struct LinuxIostatDiskCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxIostatDiskCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::DiskIoBurst],
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(
                    PlatformSet::LINUX_X64 | PlatformSet::LINUX_ARM64,
                ),
                vendor: None,
                description:
                    "Disk I/O throughput, IOPS and average await per physical device from /proc/diskstats at 2 Hz.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxIostatDiskCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for LinuxIostatDiskCollector {
    fn id(&self) -> &str {
        &self.id
    }
    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        #[cfg(target_os = "linux")]
        {
            if fs::read_to_string("/proc/diskstats").is_ok() {
                ProbeResult::Available { version: None }
            } else {
                ProbeResult::Unavailable {
                    reason: "/proc/diskstats not readable".into(),
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            ProbeResult::Unavailable {
                reason: "linux.iostat.disk is Linux-only".into(),
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples_observed = Arc::new(AtomicU64::new(0));
        let started_at = Instant::now();
        let start_clock_ns = read_monotonic_ns();

        let stop_t = stop_flag.clone();
        let samples_t = samples_observed.clone();
        let csv_t = csv_path.clone();
        let started_t = started_at;

        let handle = thread::Builder::new()
            .name("tf-disk-collector".into())
            .spawn(move || {
                if let Err(e) = polling_loop(stop_t, samples_t, csv_t, started_t) {
                    eprintln!("linux.iostat.disk polling loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("disk thread spawn: {e}")))?;

        Ok(Box::new(LinuxIostatDiskRunning {
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
            "linux.iostat.disk is Linux-only".into(),
        ))
    }
}

pub struct LinuxIostatDiskRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
    started_at: Instant,
    start_clock_ns: u64,
}

impl RunningCollector for LinuxIostatDiskRunning {
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
        metadata.insert("source".into(), "/proc/diskstats".into());
        metadata.insert("sample_period_ms".into(), "500".into());
        // Honesty about what await_ms_p99 actually contains — operators reading
        // the manifest must know the kernel does not expose true p99 percentiles.
        metadata.insert(
            "await_kind".into(),
            "avg_ms (true p99 not available without eBPF)".into(),
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

/// Decide whether a /proc/diskstats device entry should be sampled. We keep
/// only physical devices and discard loop / ram / device-mapper to reduce CSV
/// noise; LVM users can extend this list later if needed.
fn is_physical_device(name: &str) -> bool {
    if name.starts_with("loop")
        || name.starts_with("ram")
        || name.starts_with("dm-")
        || name.starts_with("zram")
    {
        return false;
    }
    name.starts_with("nvme")
        || name.starts_with("sd")
        || name.starts_with("vd")
        || name.starts_with("mmcblk")
        || name.starts_with("nbd")
        || name.starts_with("hd")
        || name.starts_with("xvd")
}

#[cfg(target_os = "linux")]
fn polling_loop(
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    csv_path: PathBuf,
    started_at: Instant,
) -> Result<(), CollectorError> {
    let mut file = fs::File::create(&csv_path)?;
    writeln!(
        file,
        "timestamp_ns,device,read_bps,write_bps,iops_r,iops_w,await_ms"
    )?;

    let mut prev: HashMap<String, DiskSnapshot> = read_diskstats()?;
    let mut prev_at = Instant::now();
    while !stop_flag.load(Ordering::Relaxed) {
        thread::sleep(SAMPLE_PERIOD);
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        let now = match read_diskstats() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let now_at = Instant::now();
        let dt = now_at.saturating_duration_since(prev_at).as_secs_f64().max(1e-3);
        prev_at = now_at;

        let ts_ns = started_at.elapsed().as_nanos() as u64;
        for (dev, cur) in &now {
            let Some(p) = prev.get(dev) else { continue };
            let burst = compute_burst(p, cur, dt);
            writeln!(
                file,
                "{ts_ns},{dev},{},{},{},{},{:.4}",
                burst.read_bps, burst.write_bps, burst.iops_r, burst.iops_w, burst.await_ms
            )?;
            samples_observed.fetch_add(1, Ordering::Relaxed);
        }
        prev = now;
    }
    file.flush()?;
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct DiskSnapshot {
    reads_completed: u64,
    sectors_read: u64,
    time_reading_ms: u64,
    writes_completed: u64,
    sectors_written: u64,
    time_writing_ms: u64,
}

#[derive(Clone, Copy, Default)]
struct DiskBurst {
    read_bps: u64,
    write_bps: u64,
    iops_r: u32,
    iops_w: u32,
    await_ms: f32,
}

fn compute_burst(prev: &DiskSnapshot, cur: &DiskSnapshot, dt_secs: f64) -> DiskBurst {
    let dt = dt_secs.max(1e-3);
    let dread_sectors = cur.sectors_read.saturating_sub(prev.sectors_read);
    let dwrite_sectors = cur.sectors_written.saturating_sub(prev.sectors_written);
    let dreads = cur.reads_completed.saturating_sub(prev.reads_completed);
    let dwrites = cur.writes_completed.saturating_sub(prev.writes_completed);
    let dt_ms_read = cur.time_reading_ms.saturating_sub(prev.time_reading_ms);
    let dt_ms_write = cur.time_writing_ms.saturating_sub(prev.time_writing_ms);
    let total_ops = dreads + dwrites;
    let avg_await_ms = if total_ops > 0 {
        ((dt_ms_read + dt_ms_write) as f64 / total_ops as f64) as f32
    } else {
        0.0
    };
    DiskBurst {
        read_bps: ((dread_sectors * SECTOR_BYTES) as f64 / dt) as u64,
        write_bps: ((dwrite_sectors * SECTOR_BYTES) as f64 / dt) as u64,
        iops_r: (dreads as f64 / dt).min(u32::MAX as f64) as u32,
        iops_w: (dwrites as f64 / dt).min(u32::MAX as f64) as u32,
        await_ms: avg_await_ms,
    }
}

#[cfg(target_os = "linux")]
fn read_diskstats() -> Result<HashMap<String, DiskSnapshot>, CollectorError> {
    let s = fs::read_to_string("/proc/diskstats")?;
    let mut out = HashMap::new();
    for line in s.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        // Format: major minor name reads_completed reads_merged sectors_read time_reading
        // writes_completed writes_merged sectors_written time_writing ...
        if parts.len() < 11 {
            continue;
        }
        let name = parts[2];
        if !is_physical_device(name) {
            continue;
        }
        let snap = DiskSnapshot {
            reads_completed: parts[3].parse().unwrap_or(0),
            sectors_read: parts[5].parse().unwrap_or(0),
            time_reading_ms: parts[6].parse().unwrap_or(0),
            writes_completed: parts[7].parse().unwrap_or(0),
            sectors_written: parts[9].parse().unwrap_or(0),
            time_writing_ms: parts[10].parse().unwrap_or(0),
        };
        out.insert(name.to_string(), snap);
    }
    Ok(out)
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

/// Parser implementation paired with `LinuxIostatDiskCollector`.
pub struct LinuxIostatDiskParser;

impl CollectorParser for LinuxIostatDiskParser {
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

        // First pass — collect unique device names sorted lexicographically so
        // lane_hint stays stable across sessions on the same host.
        let mut device_names: Vec<String> = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            if idx == 0 || line.is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split(',').collect();
            if cols.len() < 7 {
                continue;
            }
            if !device_names.iter().any(|d| d == cols[1]) {
                device_names.push(cols[1].to_string());
            }
        }
        device_names.sort();
        let lane_index: HashMap<&str, u16> = device_names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i.min(u16::MAX as usize) as u16))
            .collect();

        let mut events = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            if idx == 0 || line.is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split(',').collect();
            if cols.len() < 7 {
                continue;
            }
            let ts: u64 = match cols[0].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let device = cols[1].to_string();
            let read_bps: u64 = cols[2].parse().unwrap_or(0);
            let write_bps: u64 = cols[3].parse().unwrap_or(0);
            let iops_r: u32 = cols[4].parse().unwrap_or(0);
            let iops_w: u32 = cols[5].parse().unwrap_or(0);
            let await_ms: f32 = cols[6].parse().unwrap_or(0.0);
            let lane = *lane_index.get(device.as_str()).unwrap_or(&0);
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::DiskIoBurst,
                lane_hint: lane,
                payload: EventPayload::DiskIoBurst {
                    device,
                    read_bps,
                    write_bps,
                    iops_r,
                    iops_w,
                    await_ms_p99: await_ms,
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
                sources: ProfileSourceFlags(ProfileSourceFlags::DISK_IO),
                gpu_targets: GpuTargets::None,
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
    fn disk_collector_default_id_and_capability() {
        let c = LinuxIostatDiskCollector::new();
        assert_eq!(c.id(), "linux.iostat.disk");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::DiskIoBurst));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
        assert!(cap.platforms.contains(PlatformSet::LINUX_ARM64));
        assert!(cap.vendor.is_none());
    }

    #[test]
    fn disk_probe_smoke() {
        let c = LinuxIostatDiskCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => panic!("must not request elevation"),
        }
    }

    #[test]
    fn disk_filter_keeps_physical_drops_virtual() {
        assert!(is_physical_device("nvme0n1"));
        assert!(is_physical_device("sda"));
        assert!(is_physical_device("sdb1"));
        assert!(is_physical_device("vda"));
        assert!(is_physical_device("mmcblk0"));
        assert!(!is_physical_device("loop0"));
        assert!(!is_physical_device("ram1"));
        assert!(!is_physical_device("dm-0"));
        assert!(!is_physical_device("zram0"));
    }

    #[test]
    fn disk_compute_burst_basic() {
        // Over 0.5s: +2048 sectors read (=1MB) -> 2 MB/s; +10 reads -> 20 IOPS;
        // dt_ms_read = 50 -> avg await = 50/10 = 5 ms.
        let p = DiskSnapshot::default();
        let c = DiskSnapshot {
            reads_completed: 10,
            sectors_read: 2048,
            time_reading_ms: 50,
            writes_completed: 0,
            sectors_written: 0,
            time_writing_ms: 0,
        };
        let b = compute_burst(&p, &c, 0.5);
        assert_eq!(b.read_bps, 2 * 1024 * 1024);
        assert_eq!(b.iops_r, 20);
        assert_eq!(b.iops_w, 0);
        assert!((b.await_ms - 5.0).abs() < 0.01, "got {}", b.await_ms);
    }

    #[test]
    fn disk_parser_handles_empty_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("disk.csv");
        fs::write(
            &csv,
            "timestamp_ns,device,read_bps,write_bps,iops_r,iops_w,await_ms\n",
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
        let evs = LinuxIostatDiskParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn disk_parser_emits_events_from_sample_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("disk.csv");
        let body = "timestamp_ns,device,read_bps,write_bps,iops_r,iops_w,await_ms\n\
                    1000,sda,1048576,524288,10,5,2.5000\n\
                    1000,nvme0n1,2097152,1048576,40,20,1.0000\n\
                    2000,sda,2097152,524288,20,5,2.0000\n";
        fs::write(&csv, body).unwrap();
        let raw = RawCapture {
            artifacts: vec![csv],
            metadata: HashMap::new(),
            clock_samples: ClockSamples {
                collector_id: COLLECTOR_ID.into(),
                pairs: Vec::new(),
            },
            samples_observed: 3,
        };
        let mut names = NameInterner::new();
        let mut frames = FrameInterner::new();
        let ctx = ctx_with_dir(dir.path().to_path_buf());
        let evs = LinuxIostatDiskParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(evs.len(), 3);
        // Devices sorted alphabetically: nvme0n1=0, sda=1.
        let mut nvme_lane = None;
        let mut sda_lane = None;
        for e in &evs {
            if let EventPayload::DiskIoBurst { device, .. } = &e.payload {
                if device == "nvme0n1" {
                    nvme_lane = Some(e.lane_hint);
                } else if device == "sda" {
                    sda_lane = Some(e.lane_hint);
                }
            }
        }
        assert_eq!(nvme_lane, Some(0));
        assert_eq!(sda_lane, Some(1));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_polling_writes_csv_after_short_run() {
        let dir = TempDir::new().unwrap();
        let c = LinuxIostatDiskCollector::new();
        let ctx = ctx_with_dir(dir.path().to_path_buf());
        let running = c.start(ctx).expect("start");
        thread::sleep(Duration::from_millis(1100));
        let raw = running.stop().expect("stop");
        assert!(!raw.artifacts.is_empty());
        let content = fs::read_to_string(&raw.artifacts[0]).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(
            lines[0],
            "timestamp_ns,device,read_bps,write_bps,iops_r,iops_w,await_ms"
        );
    }
}
