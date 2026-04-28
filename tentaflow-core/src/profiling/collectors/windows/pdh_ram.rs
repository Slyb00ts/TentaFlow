// =============================================================================
// File: collectors/windows/pdh_ram.rs — Windows PDH memory pressure collector.
// Counters polled at 2 Hz:
//   \Memory\Available Bytes
//   \Memory\Committed Bytes      (commit-charge proxy for "used")
//   \Memory\Page Faults/sec
// Samples are written to `ram.csv`; the parser converts the CSV into
// TimelineEvent::RamSample rows.
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
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner,
    PlatformSet, ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "windows.pdh.ram";
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const CSV_FILENAME: &str = "ram.csv";

pub struct WindowsPdhRamCollector {
    capability: CollectorCapability,
    id: String,
}

impl WindowsPdhRamCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::RamSample],
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(
                    PlatformSet::WINDOWS_X64 | PlatformSet::WINDOWS_ARM64,
                ),
                vendor: None,
                description:
                    "Windows memory pressure (committed, available, page faults) via PDH at 2 Hz.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for WindowsPdhRamCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for WindowsPdhRamCollector {
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
            reason: "windows.pdh.ram is Windows-only".into(),
        }
    }

    #[cfg(target_os = "windows")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        windows_impl::start_session(ctx)
    }

    #[cfg(not(target_os = "windows"))]
    fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        Err(CollectorError::Custom(
            "windows.pdh.ram is Windows-only".into(),
        ))
    }
}

pub struct WindowsPdhRamRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    #[cfg(target_os = "windows")]
    handle: Option<std::thread::JoinHandle<()>>,
}

impl RunningCollector for WindowsPdhRamRunning {
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
        metadata.insert("source".into(), "PDH \\Memory".into());
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
                pairs: vec![(0, 0)],
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

    const SAMPLE_PERIOD: Duration = Duration::from_millis(500);

    pub fn probe_open() -> Result<(), PdhError> {
        let _q = PdhQuery::open()?;
        Ok(())
    }

    pub fn start_session(
        ctx: SessionCtx,
    ) -> Result<Box<dyn RunningCollector>, CollectorError> {
        fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);
        let stop_flag = Arc::new(AtomicBool::new(false));
        let samples_observed = Arc::new(AtomicU64::new(0));

        let stop_t = stop_flag.clone();
        let samples_t = samples_observed.clone();
        let csv_t = csv_path.clone();
        let started_at = Instant::now();

        let handle = thread::Builder::new()
            .name("tf-windows-pdh-ram".into())
            .spawn(move || {
                if let Err(e) = polling_loop(stop_t, samples_t, csv_t, started_at) {
                    eprintln!("windows.pdh.ram polling loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("ram thread spawn: {e}")))?;

        Ok(Box::new(WindowsPdhRamRunning {
            id: COLLECTOR_ID.to_string(),
            output_dir: ctx.output_dir.clone(),
            csv_path,
            stop_flag,
            samples_observed,
            handle: Some(handle),
        }))
    }

    fn polling_loop(
        stop_flag: Arc<AtomicBool>,
        samples_observed: Arc<AtomicU64>,
        csv_path: PathBuf,
        started_at: Instant,
    ) -> Result<(), CollectorError> {
        let mut file = fs::File::create(&csv_path)?;
        writeln!(
            file,
            "timestamp_ns,used_bytes,available_bytes,page_faults_per_s"
        )?;

        let query = PdhQuery::open()
            .map_err(|e| CollectorError::Custom(format!("PdhOpenQueryW: {e}")))?;

        let avail = query
            .add_counter("\\Memory\\Available Bytes")
            .map_err(|e| CollectorError::Custom(format!("Available Bytes: {e}")))?;
        let committed = query
            .add_counter("\\Memory\\Committed Bytes")
            .map_err(|e| CollectorError::Custom(format!("Committed Bytes: {e}")))?;
        let faults = query.add_counter("\\Memory\\Page Faults/sec").ok();

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
            let avail_b = avail.value_double().unwrap_or(0.0).max(0.0) as u64;
            let used_b = committed.value_double().unwrap_or(0.0).max(0.0) as u64;
            let faults_pps = faults
                .as_ref()
                .and_then(|c| c.value_double())
                .map(|v| v.max(0.0) as u64)
                .unwrap_or(0);
            writeln!(file, "{ts_ns},{used_b},{avail_b},{faults_pps}")?;
            samples_observed.fetch_add(1, Ordering::Relaxed);
        }
        file.flush()?;
        Ok(())
    }
}

pub struct WindowsPdhRamParser;

impl CollectorParser for WindowsPdhRamParser {
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
            let Ok(used_bytes) = cols[1].parse::<u64>() else {
                continue;
            };
            let Ok(available_bytes) = cols[2].parse::<u64>() else {
                continue;
            };
            let Ok(page_faults_per_s) = cols[3].parse::<u64>() else {
                continue;
            };
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::RamSample,
                lane_hint: 0,
                payload: EventPayload::RamSample {
                    used_bytes,
                    available_bytes,
                    page_faults_per_s,
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
    fn ram_default_id_and_capability() {
        let c = WindowsPdhRamCollector::new();
        assert_eq!(c.id(), "windows.pdh.ram");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::RamSample));
        assert!(cap.platforms.contains(PlatformSet::WINDOWS_X64));
        assert!(cap.platforms.contains(PlatformSet::WINDOWS_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::LINUX_X64));
        assert_eq!(cap.elevation, ElevationRequirement::None);
        assert!(cap.vendor.is_none());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn ram_probe_unavailable_on_non_windows() {
        let c = WindowsPdhRamCollector::new();
        match c.probe() {
            ProbeResult::Unavailable { .. } => {}
            _ => panic!("expected Unavailable on non-Windows"),
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn ram_probe_smoke_windows() {
        let c = WindowsPdhRamCollector::new();
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
        let evs = WindowsPdhRamParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn ram_parser_emits_events() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("ram.csv");
        let body = "timestamp_ns,used_bytes,available_bytes,page_faults_per_s\n\
                    1000,2147483648,1073741824,150\n\
                    2000,2200000000,1000000000,200\n";
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
        let evs = WindowsPdhRamParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(evs.len(), 2);
        match &evs[0].payload {
            EventPayload::RamSample {
                used_bytes,
                available_bytes,
                page_faults_per_s,
            } => {
                assert_eq!(*used_bytes, 2_147_483_648);
                assert_eq!(*available_bytes, 1_073_741_824);
                assert_eq!(*page_faults_per_s, 150);
            }
            _ => panic!("wrong payload"),
        }
        assert_eq!(evs[0].category, EventCategory::RamSample);
    }
}
