// =============================================================================
// File: collectors/windows/pdh_gpu.rs — Vendor-neutral Windows PDH GPU
// collector at 1 Hz. Counters:
//   \GPU Engine(*)\Utilization Percentage      (sum per LUID, capped at 100%)
//   \GPU Process Memory(*)\Local Usage         (sum per LUID -> mem_used_bytes)
//   \GPU Adapter Memory(*)\Total Committed     (per LUID -> denominator for mem_pct)
// PDH does not expose GPU power on Windows; power must come from vendor SDK
// collectors (e.g. NVML, ROCm-SMI), so this collector emits only GpuUtilSample
// and GpuMemSample.
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

const COLLECTOR_ID: &str = "windows.pdh.gpu";
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const CSV_FILENAME: &str = "gpu.csv";

pub struct WindowsPdhGpuCollector {
    capability: CollectorCapability,
    id: String,
}

impl WindowsPdhGpuCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::GpuUtilSample, EventCategory::GpuMemSample],
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(
                    PlatformSet::WINDOWS_X64 | PlatformSet::WINDOWS_ARM64,
                ),
                vendor: None,
                description:
                    "Windows GPU utilization and memory via PDH GPU Engine + GPU Process Memory counters at 1 Hz. Vendor-neutral, no kernel-level detail.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for WindowsPdhGpuCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for WindowsPdhGpuCollector {
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
            reason: "windows.pdh.gpu is Windows-only".into(),
        }
    }

    #[cfg(target_os = "windows")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        windows_impl::start_session(ctx)
    }

    #[cfg(not(target_os = "windows"))]
    fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        Err(CollectorError::Custom(
            "windows.pdh.gpu is Windows-only".into(),
        ))
    }
}

pub struct WindowsPdhGpuRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    #[cfg(target_os = "windows")]
    handle: Option<std::thread::JoinHandle<()>>,
}

impl RunningCollector for WindowsPdhGpuRunning {
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
        metadata.insert("source".into(), "PDH \\GPU Engine(*) + \\GPU Process Memory(*)".into());
        metadata.insert("sample_period_ms".into(), "1000".into());
        metadata.insert("power".into(), "not available via PDH".into());

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
    use crate::profiling::collectors::windows::pdh_sys::{
        enum_instances, PdhCounter, PdhError, PdhQuery,
    };
    use std::collections::BTreeMap;
    use std::io::Write;
    use std::thread;
    use std::time::{Duration, Instant};

    const SAMPLE_PERIOD: Duration = Duration::from_millis(1000);

    pub fn probe_open() -> Result<(), PdhError> {
        let _q = PdhQuery::open()?;
        Ok(())
    }

    /// Extract the LUID portion from an instance name like
    /// `pid_1234_luid_0x00000000_0x0000C0FE_phys_0_eng_0_engtype_3D`.
    /// Returns the `luid_*` substring (stable per adapter) or `None`.
    fn extract_luid(instance: &str) -> Option<String> {
        let idx = instance.find("luid_")?;
        let tail = &instance[idx + "luid_".len()..];
        // LUID is two hex words separated by `_`. Capture up to the next
        // `_phys` or end of string.
        let end = tail.find("_phys").unwrap_or(tail.len());
        Some(tail[..end].to_string())
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
            .name("tf-windows-pdh-gpu".into())
            .spawn(move || {
                if let Err(e) = polling_loop(stop_t, samples_t, csv_t, started_at) {
                    eprintln!("windows.pdh.gpu polling loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("gpu thread spawn: {e}")))?;

        Ok(Box::new(WindowsPdhGpuRunning {
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
            "timestamp_ns,device_id,compute_pct,mem_pct,mem_used_bytes"
        )?;

        let query = PdhQuery::open()
            .map_err(|e| CollectorError::Custom(format!("PdhOpenQueryW: {e}")))?;

        // Engine util counters (one per engine instance), grouped by LUID.
        let engine_instances = enum_instances("GPU Engine").unwrap_or_default();
        let mut util_by_luid: BTreeMap<String, Vec<PdhCounter>> = BTreeMap::new();
        for inst in &engine_instances {
            let Some(luid) = extract_luid(inst) else {
                continue;
            };
            let path = format!("\\GPU Engine({inst})\\Utilization Percentage");
            if let Ok(c) = query.add_counter(&path) {
                util_by_luid.entry(luid).or_default().push(c);
            }
        }

        // Process memory (Local Usage) — sum per LUID.
        let mem_instances = enum_instances("GPU Process Memory").unwrap_or_default();
        let mut mem_by_luid: BTreeMap<String, Vec<PdhCounter>> = BTreeMap::new();
        for inst in &mem_instances {
            let Some(luid) = extract_luid(inst) else {
                continue;
            };
            let path = format!("\\GPU Process Memory({inst})\\Local Usage");
            if let Ok(c) = query.add_counter(&path) {
                mem_by_luid.entry(luid).or_default().push(c);
            }
        }

        // Adapter total committed — denominator for mem_pct, one per adapter.
        let adapter_instances = enum_instances("GPU Adapter Memory").unwrap_or_default();
        let mut adapter_total_by_luid: BTreeMap<String, PdhCounter> = BTreeMap::new();
        for inst in &adapter_instances {
            let Some(luid) = extract_luid(inst) else {
                continue;
            };
            let path = format!("\\GPU Adapter Memory({inst})\\Total Committed");
            if let Ok(c) = query.add_counter(&path) {
                adapter_total_by_luid.insert(luid, c);
            }
        }

        // Stable mapping LUID -> u32 device_id assigned in iteration order.
        let mut device_ids: BTreeMap<String, u32> = BTreeMap::new();
        let mut next_id: u32 = 0;
        for luid in util_by_luid
            .keys()
            .chain(mem_by_luid.keys())
            .chain(adapter_total_by_luid.keys())
        {
            if !device_ids.contains_key(luid) {
                device_ids.insert(luid.clone(), next_id);
                next_id += 1;
            }
        }

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
            for (luid, dev_id) in &device_ids {
                let compute_pct = util_by_luid
                    .get(luid)
                    .map(|cs| {
                        cs.iter()
                            .filter_map(|c| c.value_double())
                            .sum::<f64>()
                            .clamp(0.0, 100.0) as f32
                    })
                    .unwrap_or(0.0);
                let mem_used = mem_by_luid
                    .get(luid)
                    .map(|cs| {
                        cs.iter()
                            .filter_map(|c| c.value_double())
                            .map(|v| v.max(0.0))
                            .sum::<f64>() as u64
                    })
                    .unwrap_or(0);
                let mem_total = adapter_total_by_luid
                    .get(luid)
                    .and_then(|c| c.value_double())
                    .map(|v| v.max(0.0) as u64)
                    .unwrap_or(0);
                let mem_pct = if mem_total > 0 {
                    ((mem_used as f64 / mem_total as f64) * 100.0).clamp(0.0, 100.0) as f32
                } else {
                    0.0
                };
                writeln!(
                    file,
                    "{ts_ns},{dev_id},{compute_pct:.4},{mem_pct:.4},{mem_used}"
                )?;
                samples_observed.fetch_add(1, Ordering::Relaxed);
            }
        }
        file.flush()?;
        Ok(())
    }

    #[cfg(test)]
    mod inner_tests {
        use super::*;

        #[test]
        fn extract_luid_parses_typical_instance() {
            let inst = "pid_1234_luid_0x00000000_0x0000C0FE_phys_0_eng_0_engtype_3D";
            assert_eq!(
                extract_luid(inst).as_deref(),
                Some("0x00000000_0x0000C0FE")
            );
        }

        #[test]
        fn extract_luid_returns_none_when_missing() {
            assert!(extract_luid("nothing_here").is_none());
        }
    }
}

pub struct WindowsPdhGpuParser;

impl CollectorParser for WindowsPdhGpuParser {
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
            if cols.len() < 5 {
                continue;
            }
            let Ok(ts) = cols[0].parse::<u64>() else {
                continue;
            };
            let Ok(device_id) = cols[1].parse::<u32>() else {
                continue;
            };
            let Ok(compute_pct) = cols[2].parse::<f32>() else {
                continue;
            };
            let Ok(mem_pct) = cols[3].parse::<f32>() else {
                continue;
            };
            let Ok(mem_used_bytes) = cols[4].parse::<u64>() else {
                continue;
            };
            let lane = device_id as u16;
            // GpuUtilSample: temp_c unknown via PDH -> 0.0.
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::GpuUtilSample,
                lane_hint: lane,
                payload: EventPayload::GpuUtilSample {
                    device_id,
                    compute_pct,
                    mem_pct,
                    mem_used_bytes,
                    temp_c: 0.0,
                },
            });
            // GpuMemSample: PDH does not surface free bytes directly; we
            // record allocated_bytes = mem_used_bytes and free_bytes = 0
            // (consumer derives total from adapter info if needed).
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::GpuMemSample,
                lane_hint: lane,
                payload: EventPayload::GpuMemSample {
                    device_id,
                    allocated_bytes: mem_used_bytes,
                    free_bytes: 0,
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
    fn gpu_default_id_and_capability() {
        let c = WindowsPdhGpuCollector::new();
        assert_eq!(c.id(), "windows.pdh.gpu");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::GpuUtilSample));
        assert!(cap.categories.contains(&EventCategory::GpuMemSample));
        assert!(cap.platforms.contains(PlatformSet::WINDOWS_X64));
        assert!(cap.platforms.contains(PlatformSet::WINDOWS_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::LINUX_X64));
        assert_eq!(cap.elevation, ElevationRequirement::None);
        assert!(cap.vendor.is_none());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn gpu_probe_unavailable_on_non_windows() {
        let c = WindowsPdhGpuCollector::new();
        match c.probe() {
            ProbeResult::Unavailable { .. } => {}
            _ => panic!("expected Unavailable on non-Windows"),
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn gpu_probe_smoke_windows() {
        let c = WindowsPdhGpuCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => panic!("must not request elevation"),
        }
    }

    #[test]
    fn gpu_parser_handles_empty_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("gpu.csv");
        fs::write(&csv, "timestamp_ns,device_id,compute_pct,mem_pct,mem_used_bytes\n").unwrap();
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
        let evs = WindowsPdhGpuParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn gpu_parser_emits_events() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("gpu.csv");
        let body = "timestamp_ns,device_id,compute_pct,mem_pct,mem_used_bytes\n\
                    1000,0,42.5000,60.0000,6442450944\n\
                    1000,1,10.0000,15.0000,2147483648\n";
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
        let evs = WindowsPdhGpuParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        // Each CSV row produces 2 events (util + mem sample).
        assert_eq!(evs.len(), 4);
        assert_eq!(evs[0].category, EventCategory::GpuUtilSample);
        assert_eq!(evs[1].category, EventCategory::GpuMemSample);
        match &evs[0].payload {
            EventPayload::GpuUtilSample {
                device_id,
                compute_pct,
                mem_pct,
                mem_used_bytes,
                temp_c,
            } => {
                assert_eq!(*device_id, 0);
                assert!((*compute_pct - 42.5).abs() < 0.01);
                assert!((*mem_pct - 60.0).abs() < 0.01);
                assert_eq!(*mem_used_bytes, 6_442_450_944);
                assert_eq!(*temp_c, 0.0);
            }
            _ => panic!("wrong payload"),
        }
        match &evs[1].payload {
            EventPayload::GpuMemSample {
                device_id,
                allocated_bytes,
                free_bytes,
            } => {
                assert_eq!(*device_id, 0);
                assert_eq!(*allocated_bytes, 6_442_450_944);
                assert_eq!(*free_bytes, 0);
            }
            _ => panic!("wrong payload"),
        }
    }
}
