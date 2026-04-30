#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux/cpu_util.rs — CPU utilization & frequency collector
// reading /proc/stat at 10 Hz and writing per-core samples to a CSV artifact.
// Parser converts the CSV into TimelineEvent::CpuUtil rows.
//
// Items below the trait impl are exercised only by the Linux polling thread
// and by the unit tests; on non-Linux library builds they are dead code by
// design (start() returns Custom("Linux only") and never invokes them).
// =============================================================================

use std::collections::HashMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner, PlatformSet,
    ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "linux.proc.cpu_util";
const CSV_FILENAME: &str = "cpu_util.csv";
const SAMPLE_PERIOD: Duration = Duration::from_millis(100);

/// CPU utilization sampler driven by /proc/stat.
pub struct LinuxProcCpuUtilCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxProcCpuUtilCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::CpuUtil],
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(
                    PlatformSet::LINUX_X64 | PlatformSet::LINUX_ARM64,
                ),
                vendor: None,
                description:
                    "CPU utilization and frequency per core, sampled from /proc/stat at 10 Hz.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxProcCpuUtilCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for LinuxProcCpuUtilCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        #[cfg(target_os = "linux")]
        {
            match fs::metadata("/proc/stat") {
                Ok(_) => match fs::read_to_string("/proc/stat") {
                    Ok(_) => ProbeResult::Available { version: None },
                    Err(e) => ProbeResult::Unavailable {
                        reason: format!("/proc/stat not readable: {e}"),
                    },
                },
                Err(e) => ProbeResult::Unavailable {
                    reason: format!("/proc/stat missing: {e}"),
                },
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            ProbeResult::Unavailable {
                reason: "linux.proc.cpu_util is Linux-only".into(),
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

        let stop_flag_t = stop_flag.clone();
        let samples_t = samples_observed.clone();
        let csv_path_t = csv_path.clone();
        let started_at_t = started_at;

        let handle = thread::Builder::new()
            .name("tf-cpu-util-collector".into())
            .spawn(move || {
                if let Err(e) = polling_loop(stop_flag_t, samples_t, csv_path_t, started_at_t) {
                    eprintln!("linux.proc.cpu_util polling loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("cpu_util thread spawn: {e}")))?;

        Ok(Box::new(LinuxProcCpuUtilRunning {
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
            "linux.proc.cpu_util is Linux-only".into(),
        ))
    }
}

/// Live CPU utilization sampler.
pub struct LinuxProcCpuUtilRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
    started_at: Instant,
    start_clock_ns: u64,
}

impl RunningCollector for LinuxProcCpuUtilRunning {
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
        metadata.insert("source".into(), "/proc/stat".into());
        metadata.insert("sample_period_ms".into(), "100".into());

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
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    csv_path: PathBuf,
    started_at: Instant,
) -> Result<(), CollectorError> {
    let mut file = fs::File::create(&csv_path)?;
    writeln!(file, "timestamp_ns,cpu,util_pct,freq_mhz")?;

    let mut prev: HashMap<u16, CpuJiffies> = read_proc_stat()?;
    while !stop_flag.load(Ordering::Relaxed) {
        thread::sleep(SAMPLE_PERIOD);
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        let now = read_proc_stat()?;
        let ts_ns = started_at.elapsed().as_nanos() as u64;
        for (cpu, cur) in &now {
            let Some(p) = prev.get(cpu) else { continue };
            let util = compute_util(p, cur);
            let freq_mhz = read_cpu_freq_mhz(*cpu);
            writeln!(file, "{ts_ns},{cpu},{util:.4},{freq_mhz}")?;
            samples_observed.fetch_add(1, Ordering::Relaxed);
        }
        prev = now;
    }
    file.flush()?;
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct CpuJiffies {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
}

impl CpuJiffies {
    fn busy(&self) -> u64 {
        self.user + self.nice + self.system + self.irq + self.softirq + self.steal
    }
    fn idle_total(&self) -> u64 {
        self.idle + self.iowait
    }
    fn total(&self) -> u64 {
        self.busy() + self.idle_total()
    }
}

#[cfg(target_os = "linux")]
fn read_proc_stat() -> Result<HashMap<u16, CpuJiffies>, CollectorError> {
    let f = fs::File::open("/proc/stat")?;
    let reader = BufReader::new(f);
    let mut out = HashMap::new();
    for line in reader.lines() {
        let line = line?;
        if !line.starts_with("cpu") {
            continue;
        }
        // Skip aggregate "cpu " line (no digit suffix); we want per-core only.
        let mut parts = line.split_whitespace();
        let head = parts.next().unwrap_or("");
        if head == "cpu" {
            continue;
        }
        let Some(rest) = head.strip_prefix("cpu") else {
            continue;
        };
        let Ok(idx) = rest.parse::<u16>() else {
            continue;
        };
        let nums: Vec<u64> = parts.filter_map(|s| s.parse().ok()).collect();
        if nums.len() < 4 {
            continue;
        }
        let j = CpuJiffies {
            user: nums.first().copied().unwrap_or(0),
            nice: nums.get(1).copied().unwrap_or(0),
            system: nums.get(2).copied().unwrap_or(0),
            idle: nums.get(3).copied().unwrap_or(0),
            iowait: nums.get(4).copied().unwrap_or(0),
            irq: nums.get(5).copied().unwrap_or(0),
            softirq: nums.get(6).copied().unwrap_or(0),
            steal: nums.get(7).copied().unwrap_or(0),
        };
        out.insert(idx, j);
    }
    Ok(out)
}

fn compute_util(prev: &CpuJiffies, cur: &CpuJiffies) -> f32 {
    let dt = cur.total().saturating_sub(prev.total()) as f64;
    if dt <= 0.0 {
        return 0.0;
    }
    let dbusy = cur.busy().saturating_sub(prev.busy()) as f64;
    ((dbusy / dt) * 100.0).clamp(0.0, 100.0) as f32
}

#[cfg(target_os = "linux")]
fn read_cpu_freq_mhz(cpu: u16) -> u32 {
    let p = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq");
    match fs::read_to_string(&p) {
        Ok(s) => s
            .trim()
            .parse::<u64>()
            .map(|khz| (khz / 1000) as u32)
            .unwrap_or(0),
        Err(_) => 0,
    }
}

fn read_monotonic_ns() -> u64 {
    let mut ts: libc::timespec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime with a valid clock id and a stack-allocated timespec.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

/// Parser implementation paired with `LinuxProcCpuUtilCollector`.
pub struct LinuxProcCpuUtilParser;

impl CollectorParser for LinuxProcCpuUtilParser {
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
                continue; // header
            }
            let cols: Vec<&str> = line.split(',').collect();
            if cols.len() < 4 {
                continue;
            }
            let ts: u64 = match cols[0].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let cpu: u16 = match cols[1].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let util_pct: f32 = match cols[2].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let freq_mhz: u32 = match cols[3].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::CpuUtil,
                lane_hint: cpu,
                payload: EventPayload::CpuUtil {
                    core: cpu,
                    util_pct,
                    freq_mhz,
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
                sources: ProfileSourceFlags(ProfileSourceFlags::CPU_UTIL),
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
    fn cpu_util_collector_default_id_and_capability() {
        let c = LinuxProcCpuUtilCollector::new();
        assert_eq!(c.id(), "linux.proc.cpu_util");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::CpuUtil));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
        assert!(cap.platforms.contains(PlatformSet::LINUX_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::MACOS_ARM64));
        assert_eq!(cap.elevation, ElevationRequirement::None);
        assert!(cap.vendor.is_none());
    }

    #[test]
    fn cpu_util_probe_smoke() {
        let c = LinuxProcCpuUtilCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => panic!("must not request elevation"),
        }
    }

    #[test]
    fn cpu_util_parser_handles_empty_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("cpu_util.csv");
        fs::write(&csv, "timestamp_ns,cpu,util_pct,freq_mhz\n").unwrap();
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
        let evs = LinuxProcCpuUtilParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn cpu_util_parser_emits_events_from_sample_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("cpu_util.csv");
        let body = "timestamp_ns,cpu,util_pct,freq_mhz\n\
                    1000,0,12.5000,2400\n\
                    1000,1,75.0000,3200\n\
                    2000,0,42.0000,2800\n";
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
        let evs = LinuxProcCpuUtilParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(evs.len(), 3);
        assert_eq!(evs[0].category, EventCategory::CpuUtil);
        assert_eq!(evs[0].lane_hint, 0);
        match &evs[0].payload {
            EventPayload::CpuUtil {
                core,
                util_pct,
                freq_mhz,
            } => {
                assert_eq!(*core, 0);
                assert!((*util_pct - 12.5).abs() < 0.01);
                assert_eq!(*freq_mhz, 2400);
            }
            _ => panic!("wrong payload"),
        }
        assert_eq!(evs[1].lane_hint, 1);
        assert_eq!(evs[2].lane_hint, 0);
    }

    #[test]
    fn compute_util_basic() {
        let p = CpuJiffies {
            user: 100,
            nice: 0,
            system: 50,
            idle: 850,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        };
        let c = CpuJiffies {
            user: 200,
            nice: 0,
            system: 100,
            idle: 1700,
            iowait: 0,
            irq: 0,
            softirq: 0,
            steal: 0,
        };
        // dbusy = 150, dt = 1000 -> 15%
        let u = compute_util(&p, &c);
        assert!((u - 15.0).abs() < 0.01, "got {u}");
    }

    #[test]
    fn compute_util_zero_when_no_progress() {
        let p = CpuJiffies::default();
        let c = CpuJiffies::default();
        assert_eq!(compute_util(&p, &c), 0.0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_util_polling_writes_csv_after_short_run() {
        let dir = TempDir::new().unwrap();
        let c = LinuxProcCpuUtilCollector::new();
        let ctx = ctx_with_dir(dir.path().to_path_buf());
        let running = c.start(ctx).expect("start");
        thread::sleep(Duration::from_millis(350));
        let raw = running.stop().expect("stop");
        assert!(!raw.artifacts.is_empty());
        let content = fs::read_to_string(&raw.artifacts[0]).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert!(
            lines.len() > 1,
            "expected at least header + 1 row, got {}",
            lines.len()
        );
        assert_eq!(lines[0], "timestamp_ns,cpu,util_pct,freq_mhz");
    }
}
