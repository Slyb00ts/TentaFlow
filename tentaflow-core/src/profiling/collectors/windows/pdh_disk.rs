// =============================================================================
// File: collectors/windows/pdh_disk.rs — Windows PDH PhysicalDisk I/O collector
// at 2 Hz. Counters per discovered instance (excluding `_Total`):
//   \PhysicalDisk(N)\Disk Read Bytes/sec
//   \PhysicalDisk(N)\Disk Write Bytes/sec
//   \PhysicalDisk(N)\Disk Reads/sec
//   \PhysicalDisk(N)\Disk Writes/sec
//   \PhysicalDisk(N)\Avg. Disk sec/Transfer   (multiplied by 1000 to ms)
// Note: PDH exposes only an average await — true p99 needs ETW DiskIo and is
// out of scope for the no-Admin path; we surface the average in `await_ms_p99`
// and document the substitution in the RawCapture metadata.
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

const COLLECTOR_ID: &str = "windows.pdh.disk";
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const CSV_FILENAME: &str = "disk.csv";

pub struct WindowsPdhDiskCollector {
    capability: CollectorCapability,
    id: String,
}

impl WindowsPdhDiskCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::DiskIoBurst],
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(
                    PlatformSet::WINDOWS_X64 | PlatformSet::WINDOWS_ARM64,
                ),
                vendor: None,
                description:
                    "Windows physical-disk throughput, IOPS and average await via PDH at 2 Hz.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for WindowsPdhDiskCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for WindowsPdhDiskCollector {
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
            reason: "windows.pdh.disk is Windows-only".into(),
        }
    }

    #[cfg(target_os = "windows")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        windows_impl::start_session(ctx)
    }

    #[cfg(not(target_os = "windows"))]
    fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        Err(CollectorError::Custom(
            "windows.pdh.disk is Windows-only".into(),
        ))
    }
}

pub struct WindowsPdhDiskRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    #[cfg(target_os = "windows")]
    handle: Option<std::thread::JoinHandle<()>>,
}

impl RunningCollector for WindowsPdhDiskRunning {
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
        metadata.insert("source".into(), "PDH \\PhysicalDisk(*)".into());
        metadata.insert("sample_period_ms".into(), "500".into());
        metadata.insert(
            "await_kind".into(),
            "avg_ms (p99 not available without ETW DiskIo trace)".into(),
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
    use std::io::Write;
    use std::thread;
    use std::time::{Duration, Instant};

    const SAMPLE_PERIOD: Duration = Duration::from_millis(500);

    pub fn probe_open() -> Result<(), PdhError> {
        let _q = PdhQuery::open()?;
        Ok(())
    }

    struct DiskCounters {
        device: String,
        read_bps: PdhCounter,
        write_bps: PdhCounter,
        iops_r: PdhCounter,
        iops_w: PdhCounter,
        await_s: Option<PdhCounter>,
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
            .name("tf-windows-pdh-disk".into())
            .spawn(move || {
                if let Err(e) = polling_loop(stop_t, samples_t, csv_t, started_at) {
                    eprintln!("windows.pdh.disk polling loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("disk thread spawn: {e}")))?;

        Ok(Box::new(WindowsPdhDiskRunning {
            id: COLLECTOR_ID.to_string(),
            output_dir: ctx.output_dir.clone(),
            csv_path,
            stop_flag,
            samples_observed,
            handle: Some(handle),
        }))
    }

    fn sanitize_device(name: &str) -> String {
        // CSV-safe representation: drop commas and trim whitespace.
        name.replace(',', ";").trim().to_string()
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
            "timestamp_ns,device,read_bps,write_bps,iops_r,iops_w,await_ms"
        )?;

        let instances = enum_instances("PhysicalDisk").unwrap_or_default();
        let query =
            PdhQuery::open().map_err(|e| CollectorError::Custom(format!("PdhOpenQueryW: {e}")))?;

        let mut disks: Vec<DiskCounters> = Vec::new();
        for inst in instances {
            if inst == "_Total" {
                continue;
            }
            let read_bps =
                match query.add_counter(&format!("\\PhysicalDisk({inst})\\Disk Read Bytes/sec")) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
            let write_bps =
                match query.add_counter(&format!("\\PhysicalDisk({inst})\\Disk Write Bytes/sec")) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
            let iops_r = match query.add_counter(&format!("\\PhysicalDisk({inst})\\Disk Reads/sec"))
            {
                Ok(c) => c,
                Err(_) => continue,
            };
            let iops_w =
                match query.add_counter(&format!("\\PhysicalDisk({inst})\\Disk Writes/sec")) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
            let await_s = query
                .add_counter(&format!("\\PhysicalDisk({inst})\\Avg. Disk sec/Transfer"))
                .ok();
            disks.push(DiskCounters {
                device: sanitize_device(&inst),
                read_bps,
                write_bps,
                iops_r,
                iops_w,
                await_s,
            });
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
            for d in &disks {
                let r_bps = d.read_bps.value_double().unwrap_or(0.0).max(0.0) as u64;
                let w_bps = d.write_bps.value_double().unwrap_or(0.0).max(0.0) as u64;
                let r_iops = d.iops_r.value_double().unwrap_or(0.0).max(0.0) as u32;
                let w_iops = d.iops_w.value_double().unwrap_or(0.0).max(0.0) as u32;
                let await_ms = d
                    .await_s
                    .as_ref()
                    .and_then(|c| c.value_double())
                    .map(|v| (v.max(0.0) * 1000.0) as f32)
                    .unwrap_or(0.0);
                writeln!(
                    file,
                    "{ts_ns},{},{r_bps},{w_bps},{r_iops},{w_iops},{await_ms:.4}",
                    d.device
                )?;
                samples_observed.fetch_add(1, Ordering::Relaxed);
            }
        }
        file.flush()?;
        Ok(())
    }
}

pub struct WindowsPdhDiskParser;

impl CollectorParser for WindowsPdhDiskParser {
    fn parse(
        &self,
        raw: RawCapture,
        _ctx: &SessionCtx,
        names: &mut NameInterner,
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
        // (lane, interned name id) per unique device label.
        let mut device_meta: HashMap<String, (u16, u32)> = HashMap::new();
        let mut next_lane: u16 = 0;
        for (idx, line) in content.lines().enumerate() {
            if idx == 0 || line.is_empty() {
                continue;
            }
            let cols: Vec<&str> = line.split(',').collect();
            if cols.len() < 7 {
                continue;
            }
            let Ok(ts) = cols[0].parse::<u64>() else {
                continue;
            };
            let device_label = cols[1];
            let Ok(read_bps) = cols[2].parse::<u64>() else {
                continue;
            };
            let Ok(write_bps) = cols[3].parse::<u64>() else {
                continue;
            };
            let Ok(iops_r) = cols[4].parse::<u32>() else {
                continue;
            };
            let Ok(iops_w) = cols[5].parse::<u32>() else {
                continue;
            };
            let Ok(await_ms_p99) = cols[6].parse::<f32>() else {
                continue;
            };
            let (lane, device_name_id) = match device_meta.get(device_label) {
                Some(v) => *v,
                None => {
                    let l = next_lane;
                    next_lane = next_lane.wrapping_add(1);
                    let id = names.intern(device_label);
                    device_meta.insert(device_label.to_string(), (l, id));
                    (l, id)
                }
            };
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::DiskIoBurst,
                lane_hint: lane,
                payload: EventPayload::DiskIoBurst {
                    device_name_id,
                    read_bps,
                    write_bps,
                    iops_r,
                    iops_w,
                    await_ms_p99,
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
    fn disk_default_id_and_capability() {
        let c = WindowsPdhDiskCollector::new();
        assert_eq!(c.id(), "windows.pdh.disk");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::DiskIoBurst));
        assert!(cap.platforms.contains(PlatformSet::WINDOWS_X64));
        assert!(cap.platforms.contains(PlatformSet::WINDOWS_ARM64));
        assert!(!cap.platforms.contains(PlatformSet::LINUX_X64));
        assert_eq!(cap.elevation, ElevationRequirement::None);
        assert!(cap.vendor.is_none());
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn disk_probe_unavailable_on_non_windows() {
        let c = WindowsPdhDiskCollector::new();
        match c.probe() {
            ProbeResult::Unavailable { .. } => {}
            _ => panic!("expected Unavailable on non-Windows"),
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn disk_probe_smoke_windows() {
        let c = WindowsPdhDiskCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => panic!("must not request elevation"),
        }
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
        let evs = WindowsPdhDiskParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn disk_parser_emits_events() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("disk.csv");
        let body = "timestamp_ns,device,read_bps,write_bps,iops_r,iops_w,await_ms\n\
                    1000,0 C:,1048576,524288,12,8,0.4500\n\
                    1000,1 D:,2097152,1048576,30,20,0.6000\n\
                    2000,0 C:,2097152,1048576,24,16,0.5000\n";
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
        let evs = WindowsPdhDiskParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(evs.len(), 3);
        let names_vec = names.into_vec();
        match &evs[0].payload {
            EventPayload::DiskIoBurst {
                device_name_id,
                read_bps,
                write_bps,
                iops_r,
                iops_w,
                await_ms_p99,
            } => {
                assert_eq!(names_vec[*device_name_id as usize], "0 C:");
                assert_eq!(*read_bps, 1_048_576);
                assert_eq!(*write_bps, 524_288);
                assert_eq!(*iops_r, 12);
                assert_eq!(*iops_w, 8);
                assert!((*await_ms_p99 - 0.45).abs() < 0.001);
            }
            _ => panic!("wrong payload"),
        }
        // Same device gets the same lane_hint across timestamps.
        assert_eq!(evs[0].lane_hint, evs[2].lane_hint);
        assert_ne!(evs[0].lane_hint, evs[1].lane_hint);
    }
}
