// =============================================================================
// File: collectors/windows/pdh_cpu_util.rs — Windows PDH CPU utilization &
// frequency collector at 2 Hz. Counters:
//   \Processor(N)\% Processor Time
//   \Processor Information(0,N)\Processor Frequency
// Per-core samples are written to `cpu_util.csv`; the parser converts that
// into TimelineEvent::CpuUtil rows.
// =============================================================================

use std::collections::HashMap;
use std::fs;
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

const COLLECTOR_ID: &str = "windows.pdh.cpu_util";
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const CSV_FILENAME: &str = "cpu_util.csv";

/// CPU utilization sampler driven by Windows PDH.
pub struct WindowsPdhCpuUtilCollector {
    capability: CollectorCapability,
    id: String,
}

impl WindowsPdhCpuUtilCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::CpuUtil],
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(
                    PlatformSet::WINDOWS_X64 | PlatformSet::WINDOWS_ARM64,
                ),
                vendor: None,
                description: "Windows CPU utilization and frequency per core via PDH at 2 Hz.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for WindowsPdhCpuUtilCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for WindowsPdhCpuUtilCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    #[cfg(target_os = "windows")]
    fn probe(&self) -> ProbeResult {
        match windows_impl::probe_open() {
            Ok(()) => ProbeResult::Available { version: None },
            Err(e) => ProbeResult::Unavailable {
                reason: format!("PDH unavailable: {e}"),
            },
        }
    }

    #[cfg(not(target_os = "windows"))]
    fn probe(&self) -> ProbeResult {
        ProbeResult::Unavailable {
            reason: "windows.pdh.cpu_util is Windows-only".into(),
        }
    }

    #[cfg(target_os = "windows")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        windows_impl::start_session(ctx)
    }

    #[cfg(not(target_os = "windows"))]
    fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        Err(CollectorError::Custom(
            "windows.pdh.cpu_util is Windows-only".into(),
        ))
    }
}

/// Live PDH CPU sampler. Cross-platform skeleton — the join handle and the
/// PDH-specific bits live inside `windows_impl` on Windows hosts; on other
/// hosts the struct is unreachable because `start` errors out before it is
/// constructed.
pub struct WindowsPdhCpuUtilRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    #[cfg(target_os = "windows")]
    handle: Option<std::thread::JoinHandle<()>>,
    started_session_ns: u64,
}

impl RunningCollector for WindowsPdhCpuUtilRunning {
    fn collector_id(&self) -> &str {
        &self.id
    }

    #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
    fn stop(mut self: Box<Self>) -> Result<RawCapture, CollectorError> {
        self.stop_flag.store(true, Ordering::Relaxed);
        #[cfg(target_os = "windows")]
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }

        let mut metadata: HashMap<String, String> = HashMap::new();
        metadata.insert("source".into(), "PDH \\Processor(*)".into());
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
                pairs: vec![(self.started_session_ns, 0)],
            },
            samples_observed: self.samples_observed.load(Ordering::Relaxed),
        })
    }

    #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
    fn abort(mut self: Box<Self>) {
        self.stop_flag.store(true, Ordering::Relaxed);
        #[cfg(target_os = "windows")]
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        let _ = fs::remove_dir_all(&self.output_dir);
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use super::*;
    use crate::profiling::collectors::windows::pdh_sys::{PdhError, PdhQuery};
    use std::io::Write;
    use std::thread;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

    const SAMPLE_PERIOD: Duration = Duration::from_millis(500);

    pub fn probe_open() -> Result<(), PdhError> {
        let _q = PdhQuery::open()?;
        Ok(())
    }

    fn logical_processor_count() -> u32 {
        let mut info: SYSTEM_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: writes into a fully-owned local SYSTEM_INFO.
        unsafe {
            GetSystemInfo(&mut info);
        }
        info.dwNumberOfProcessors.max(1)
    }

    pub fn start_session(ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples_observed = Arc::new(AtomicU64::new(0));

        let stop_t = stop_flag.clone();
        let samples_t = samples_observed.clone();
        let csv_t = csv_path.clone();
        let started_at = Instant::now();

        let handle = thread::Builder::new()
            .name("tf-windows-pdh-cpu-util".into())
            .spawn(move || {
                if let Err(e) = polling_loop(stop_t, samples_t, csv_t, started_at) {
                    eprintln!("windows.pdh.cpu_util polling loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("cpu_util thread spawn: {e}")))?;

        Ok(Box::new(WindowsPdhCpuUtilRunning {
            id: COLLECTOR_ID.to_string(),
            output_dir: ctx.output_dir.clone(),
            csv_path,
            stop_flag,
            samples_observed,
            handle: Some(handle),
            started_session_ns: 0,
        }))
    }

    fn polling_loop(
        stop_flag: Arc<AtomicBool>,
        samples_observed: Arc<AtomicU64>,
        csv_path: PathBuf,
        started_at: Instant,
    ) -> Result<(), CollectorError> {
        let mut file = fs::File::create(&csv_path)?;
        writeln!(file, "timestamp_ns,cpu,util_pct,freq_mhz")?;

        let cores = logical_processor_count();
        let query =
            PdhQuery::open().map_err(|e| CollectorError::Custom(format!("PdhOpenQueryW: {e}")))?;

        // Per-core util counters; freq counters may fail on older Windows so
        // we keep them as Option and fall back to 0.
        let mut util_counters = Vec::with_capacity(cores as usize);
        let mut freq_counters = Vec::with_capacity(cores as usize);
        for i in 0..cores {
            let util_path = format!("\\Processor({i})\\% Processor Time");
            match query.add_counter(&util_path) {
                Ok(c) => util_counters.push((i as u16, c)),
                Err(e) => {
                    eprintln!("PdhAddCounter util cpu{i}: {e}");
                }
            }
            let freq_path = format!("\\Processor Information(0,{i})\\Processor Frequency");
            freq_counters.push((i as u16, query.add_counter(&freq_path).ok()));
        }

        // First collect primes the deltas; values are valid only after the
        // second sample tick.
        query
            .collect()
            .map_err(|e| CollectorError::Custom(format!("first collect: {e}")))?;

        while !stop_flag.load(Ordering::Relaxed) {
            thread::sleep(SAMPLE_PERIOD);
            if stop_flag.load(Ordering::Relaxed) {
                break;
            }
            if let Err(e) = query.collect() {
                eprintln!("PdhCollectQueryData: {e}");
                continue;
            }
            let ts_ns = started_at.elapsed().as_nanos() as u64;
            for (cpu, counter) in &util_counters {
                let util = counter.value_double().unwrap_or(0.0).clamp(0.0, 100.0);
                let freq_mhz = freq_counters
                    .iter()
                    .find(|(c, _)| c == cpu)
                    .and_then(|(_, opt)| opt.as_ref())
                    .and_then(|c| c.value_double())
                    .map(|v| v as u32)
                    .unwrap_or(0);
                writeln!(file, "{ts_ns},{cpu},{util:.4},{freq_mhz}")?;
                samples_observed.fetch_add(1, Ordering::Relaxed);
            }
        }
        file.flush()?;
        Ok(())
    }
}

/// Parser implementation paired with `WindowsPdhCpuUtilCollector`.
pub struct WindowsPdhCpuUtilParser;

impl CollectorParser for WindowsPdhCpuUtilParser {
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
            let Ok(ts) = cols[0].parse::<u64>() else {
                continue;
            };
            let Ok(cpu) = cols[1].parse::<u16>() else {
                continue;
            };
            let Ok(util_pct) = cols[2].parse::<f32>() else {
                continue;
            };
            let Ok(freq_mhz) = cols[3].parse::<u32>() else {
                continue;
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
    fn cpu_util_default_id_and_capability() {
        let c = WindowsPdhCpuUtilCollector::new();
        assert_eq!(c.id(), "windows.pdh.cpu_util");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::CpuUtil));
        assert!(cap.platforms.contains(PlatformSet::WINDOWS_X64));
        assert!(cap.platforms.contains(PlatformSet::WINDOWS_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::LINUX_X64));
        assert_eq!(cap.elevation, ElevationRequirement::None);
        assert!(cap.vendor.is_none());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn cpu_util_probe_unavailable_on_non_windows() {
        let c = WindowsPdhCpuUtilCollector::new();
        match c.probe() {
            ProbeResult::Unavailable { .. } => {}
            _ => panic!("expected Unavailable on non-Windows"),
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn cpu_util_probe_smoke_windows() {
        let c = WindowsPdhCpuUtilCollector::new();
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
        let evs = WindowsPdhCpuUtilParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn cpu_util_parser_emits_events() {
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
        let evs = WindowsPdhCpuUtilParser
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
}
