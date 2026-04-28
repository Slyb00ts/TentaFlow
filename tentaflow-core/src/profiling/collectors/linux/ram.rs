#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux/ram.rs — RAM usage and page-fault rate collector
// reading /proc/meminfo and /proc/vmstat at 2 Hz. Produces TimelineEvent::
// RamSample rows on parse. Internal helpers are Linux-only by design.
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

const COLLECTOR_ID: &str = "linux.proc.ram";
const CSV_FILENAME: &str = "ram.csv";
#[cfg(target_os = "linux")]
const SAMPLE_PERIOD: Duration = Duration::from_millis(500);

/// RAM sampler driven by /proc/meminfo + /proc/vmstat.
pub struct LinuxProcRamCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxProcRamCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::RamSample],
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(
                    PlatformSet::LINUX_X64 | PlatformSet::LINUX_ARM64,
                ),
                vendor: None,
                description:
                    "RAM usage and page fault rate from /proc/meminfo + /proc/vmstat at 2 Hz.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxProcRamCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for LinuxProcRamCollector {
    fn id(&self) -> &str {
        &self.id
    }
    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        #[cfg(target_os = "linux")]
        {
            if fs::read_to_string("/proc/meminfo").is_ok() {
                ProbeResult::Available { version: None }
            } else {
                ProbeResult::Unavailable {
                    reason: "/proc/meminfo not readable".into(),
                }
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            ProbeResult::Unavailable {
                reason: "linux.proc.ram is Linux-only".into(),
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
            .name("tf-ram-collector".into())
            .spawn(move || {
                if let Err(e) = polling_loop(stop_t, samples_t, csv_t, started_t) {
                    eprintln!("linux.proc.ram polling loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("ram thread spawn: {e}")))?;

        Ok(Box::new(LinuxProcRamRunning {
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
            "linux.proc.ram is Linux-only".into(),
        ))
    }
}

pub struct LinuxProcRamRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
    started_at: Instant,
    start_clock_ns: u64,
}

impl RunningCollector for LinuxProcRamRunning {
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
        metadata.insert("source".into(), "/proc/meminfo+/proc/vmstat".into());
        metadata.insert("sample_period_ms".into(), "500".into());

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
    writeln!(file, "timestamp_ns,used_bytes,available_bytes,page_faults_per_s")?;

    let mut prev_pgfault: Option<u64> = read_pgfault_total().ok();
    let mut prev_at = Instant::now();

    while !stop_flag.load(Ordering::Relaxed) {
        thread::sleep(SAMPLE_PERIOD);
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        let mem = match read_meminfo() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let now_pgfault = read_pgfault_total().ok();
        let now = Instant::now();
        let dt = now.saturating_duration_since(prev_at).as_secs_f64().max(1e-3);
        let pf_per_s = match (prev_pgfault, now_pgfault) {
            (Some(p), Some(c)) => ((c.saturating_sub(p)) as f64 / dt) as u64,
            _ => 0,
        };
        prev_at = now;
        prev_pgfault = now_pgfault;

        let ts_ns = started_at.elapsed().as_nanos() as u64;
        writeln!(
            file,
            "{ts_ns},{},{},{pf_per_s}",
            mem.used_bytes, mem.available_bytes
        )?;
        samples_observed.fetch_add(1, Ordering::Relaxed);
    }
    file.flush()?;
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct MemSnapshot {
    used_bytes: u64,
    available_bytes: u64,
}

#[cfg(target_os = "linux")]
fn read_meminfo() -> Result<MemSnapshot, CollectorError> {
    let s = fs::read_to_string("/proc/meminfo")?;
    let mut total_kb: u64 = 0;
    let mut available_kb: u64 = 0;
    let mut free_kb: u64 = 0;
    for line in s.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next().unwrap_or("");
        let val: u64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        match key {
            "MemTotal:" => total_kb = val,
            "MemAvailable:" => available_kb = val,
            "MemFree:" => free_kb = val,
            _ => {}
        }
    }
    let avail = if available_kb > 0 { available_kb } else { free_kb };
    let used_kb = total_kb.saturating_sub(avail);
    Ok(MemSnapshot {
        used_bytes: used_kb * 1024,
        available_bytes: avail * 1024,
    })
}

#[cfg(target_os = "linux")]
fn read_pgfault_total() -> Result<u64, CollectorError> {
    let s = fs::read_to_string("/proc/vmstat")?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("pgfault ") {
            return rest
                .trim()
                .parse::<u64>()
                .map_err(|e| CollectorError::Parse(format!("vmstat pgfault: {e}")));
        }
    }
    Ok(0)
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

/// Parser implementation paired with `LinuxProcRamCollector`.
pub struct LinuxProcRamParser;

impl CollectorParser for LinuxProcRamParser {
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
            let cols: Vec<&str> = line.split(',').collect();
            if cols.len() < 4 {
                continue;
            }
            let ts: u64 = match cols[0].parse() {
                Ok(v) => v,
                Err(_) => continue,
            };
            let used: u64 = cols[1].parse().unwrap_or(0);
            let avail: u64 = cols[2].parse().unwrap_or(0);
            let pf: u64 = cols[3].parse().unwrap_or(0);
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::RamSample,
                lane_hint: 0,
                payload: EventPayload::RamSample {
                    used_bytes: used,
                    available_bytes: avail,
                    page_faults_per_s: pf,
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
                sources: ProfileSourceFlags(ProfileSourceFlags::RAM_USAGE),
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
    fn ram_collector_default_id_and_capability() {
        let c = LinuxProcRamCollector::new();
        assert_eq!(c.id(), "linux.proc.ram");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::RamSample));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
        assert!(cap.platforms.contains(PlatformSet::LINUX_ARM64));
        assert!(cap.vendor.is_none());
    }

    #[test]
    fn ram_probe_smoke() {
        let c = LinuxProcRamCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => panic!("must not request elevation"),
        }
    }

    #[test]
    fn ram_parser_handles_empty_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("ram.csv");
        fs::write(&csv, "timestamp_ns,used_bytes,available_bytes,page_faults_per_s\n").unwrap();
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
        let evs = LinuxProcRamParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn ram_parser_emits_events_from_sample_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("ram.csv");
        let body = "timestamp_ns,used_bytes,available_bytes,page_faults_per_s\n\
                    1000,1073741824,2147483648,500\n\
                    2000,1200000000,2000000000,750\n\
                    3000,1300000000,1900000000,820\n";
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
        let evs = LinuxProcRamParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(evs.len(), 3);
        assert_eq!(evs[0].category, EventCategory::RamSample);
        match &evs[0].payload {
            EventPayload::RamSample {
                used_bytes,
                available_bytes,
                page_faults_per_s,
            } => {
                assert_eq!(*used_bytes, 1073741824);
                assert_eq!(*available_bytes, 2147483648);
                assert_eq!(*page_faults_per_s, 500);
            }
            _ => panic!("wrong payload"),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn ram_polling_writes_csv_after_short_run() {
        let dir = TempDir::new().unwrap();
        let c = LinuxProcRamCollector::new();
        let ctx = ctx_with_dir(dir.path().to_path_buf());
        let running = c.start(ctx).expect("start");
        thread::sleep(Duration::from_millis(1100));
        let raw = running.stop().expect("stop");
        assert!(!raw.artifacts.is_empty());
        let content = fs::read_to_string(&raw.artifacts[0]).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert!(lines.len() > 1);
        assert_eq!(
            lines[0],
            "timestamp_ns,used_bytes,available_bytes,page_faults_per_s"
        );
    }
}
