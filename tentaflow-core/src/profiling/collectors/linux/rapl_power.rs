#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux/rapl_power.rs — CPU package / core / DRAM power
// collector reading /sys/class/powercap/intel-rapl:* energy_uj counters at
// 2 Hz. Probe returns Unavailable (with EPERM hint) on hosts where the kernel
// restricts the counter to root: this is honest reporting; sudo is rarely a
// reliable workaround for the post-2020 mitigation. Internal helpers are
// Linux-only by design.
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
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, PowerDomain, TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, NameInterner,
    PlatformSet, ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "linux.rapl.power";
const CSV_FILENAME: &str = "power.csv";
#[cfg(target_os = "linux")]
const SAMPLE_PERIOD: Duration = Duration::from_millis(500);
#[cfg(target_os = "linux")]
const POWERCAP_ROOT: &str = "/sys/class/powercap";

/// CPU power sampler driven by RAPL (Intel and AMD on recent kernels).
pub struct LinuxRaplPowerCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxRaplPowerCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::PowerSample],
                elevation: ElevationRequirement::None,
                // RAPL exists only on Linux x86_64; ARM64 servers expose
                // power separately (e.g. hwmon) and are out of scope here.
                platforms: PlatformSet::from_flag(PlatformSet::LINUX_X64),
                vendor: None,
                description:
                    "CPU package, core, DRAM power consumption from RAPL energy counters at 2 Hz.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxRaplPowerCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for LinuxRaplPowerCollector {
    fn id(&self) -> &str {
        &self.id
    }
    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        #[cfg(target_os = "linux")]
        {
            // 1. Powercap subsystem must exist.
            let pkg0 = format!("{POWERCAP_ROOT}/intel-rapl:0/energy_uj");
            if fs::metadata(&pkg0).is_err() {
                return ProbeResult::Unavailable {
                    reason: format!("RAPL powercap not present ({pkg0})"),
                };
            }
            // 2. The counter must be readable for the current user. Recent
            //    kernels (>= 5.10) hide energy_uj behind root because it
            //    enables the PLATYPUS side-channel attack. We do not attempt
            //    to escalate; a follow-up `chmod a+r` or running as root is
            //    the operator's choice.
            match fs::read_to_string(&pkg0) {
                Ok(_) => ProbeResult::Available { version: None },
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    ProbeResult::Unavailable {
                        reason: "RAPL energy_uj not readable (kernel >= 5.10 may require root \
                                 to mitigate PLATYPUS side-channel; run `chmod a+r \
                                 /sys/class/powercap/intel-rapl:*/energy_uj` if you accept \
                                 the risk)"
                            .into(),
                    }
                }
                Err(e) => ProbeResult::Unavailable {
                    reason: format!("RAPL energy_uj read failed: {e}"),
                },
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            ProbeResult::Unavailable {
                reason: "linux.rapl.power is Linux-only".into(),
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        let domains = enumerate_domains();
        if domains.is_empty() {
            return Err(CollectorError::Custom(
                "no readable RAPL domains found".into(),
            ));
        }
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
            .name("tf-rapl-collector".into())
            .spawn(move || {
                if let Err(e) = polling_loop(stop_t, samples_t, csv_t, started_t, domains) {
                    eprintln!("linux.rapl.power polling loop ended: {e}");
                }
            })
            .map_err(|e| CollectorError::Spawn(format!("rapl thread spawn: {e}")))?;

        Ok(Box::new(LinuxRaplPowerRunning {
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
            "linux.rapl.power is Linux-only".into(),
        ))
    }
}

pub struct LinuxRaplPowerRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    handle: Option<JoinHandle<()>>,
    started_at: Instant,
    start_clock_ns: u64,
}

impl RunningCollector for LinuxRaplPowerRunning {
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
        metadata.insert("source".into(), "/sys/class/powercap/intel-rapl:*".into());
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

#[derive(Clone, Debug)]
struct RaplDomain {
    /// Stable index across the session for lane_hint emission.
    idx: u16,
    /// Raw powercap name (e.g. "package-0", "core", "dram").
    name: String,
    /// Mapped enum we report on parse.
    domain: PowerDomain,
    /// Path to the energy_uj counter.
    energy_path: PathBuf,
}

fn map_domain_name(raw: &str) -> PowerDomain {
    if raw.starts_with("package") {
        PowerDomain::CpuPkg
    } else if raw == "core" {
        PowerDomain::CpuCore
    } else if raw.starts_with("dram") {
        PowerDomain::Dram
    } else {
        PowerDomain::Other
    }
}

#[cfg(target_os = "linux")]
fn enumerate_domains() -> Vec<RaplDomain> {
    let mut out: Vec<RaplDomain> = Vec::new();
    let Ok(entries) = fs::read_dir(POWERCAP_ROOT) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|d| d.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|n| n.starts_with("intel-rapl"))
                .unwrap_or(false)
        })
        .collect();
    paths.sort();
    let mut next_idx: u16 = 0;
    for path in paths {
        let energy_path = path.join("energy_uj");
        let name_path = path.join("name");
        let Ok(name) = fs::read_to_string(&name_path) else {
            continue;
        };
        // Verify we can actually read the counter; otherwise skip the domain.
        if fs::read_to_string(&energy_path).is_err() {
            continue;
        }
        let name_trim = name.trim().to_string();
        let domain = map_domain_name(&name_trim);
        out.push(RaplDomain {
            idx: next_idx,
            name: name_trim,
            domain,
            energy_path,
        });
        next_idx = next_idx.saturating_add(1);
    }
    out
}

#[cfg(target_os = "linux")]
fn polling_loop(
    stop_flag: Arc<AtomicBool>,
    samples_observed: Arc<AtomicU64>,
    csv_path: PathBuf,
    started_at: Instant,
    domains: Vec<RaplDomain>,
) -> Result<(), CollectorError> {
    let mut file = fs::File::create(&csv_path)?;
    writeln!(file, "timestamp_ns,domain,domain_idx,watts")?;

    // Initial energy read; if any domain fails, skip it for the rest of the run.
    let mut prev_uj: HashMap<u16, u64> = HashMap::new();
    for d in &domains {
        if let Ok(v) = read_uj(&d.energy_path) {
            prev_uj.insert(d.idx, v);
        }
    }
    let mut prev_at = Instant::now();

    while !stop_flag.load(Ordering::Relaxed) {
        thread::sleep(SAMPLE_PERIOD);
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        let now_at = Instant::now();
        let dt_us = now_at
            .saturating_duration_since(prev_at)
            .as_micros()
            .max(1) as u64;
        prev_at = now_at;
        let ts_ns = started_at.elapsed().as_nanos() as u64;

        for d in &domains {
            let cur = match read_uj(&d.energy_path) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let prev = prev_uj.get(&d.idx).copied().unwrap_or(cur);
            // Energy counters wrap; treat decrease as a wrap and emit zero.
            let delta_uj = cur.saturating_sub(prev);
            let watts = (delta_uj as f64) / (dt_us as f64);
            // delta_uj/us = uW per us = uW; we want Watts so divide by 1e6.
            let watts = (watts / 1_000_000.0) as f32;
            writeln!(
                file,
                "{ts_ns},{},{},{:.6}",
                d.name, d.idx, watts.max(0.0)
            )?;
            samples_observed.fetch_add(1, Ordering::Relaxed);
            prev_uj.insert(d.idx, cur);
        }
    }
    file.flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn read_uj(path: &PathBuf) -> Result<u64, CollectorError> {
    let s = fs::read_to_string(path)?;
    s.trim()
        .parse::<u64>()
        .map_err(|e| CollectorError::Parse(format!("rapl energy parse: {e}")))
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

/// Parser implementation paired with `LinuxRaplPowerCollector`.
pub struct LinuxRaplPowerParser;

impl CollectorParser for LinuxRaplPowerParser {
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
            let raw_domain = cols[1];
            let domain_idx: u16 = cols[2].parse().unwrap_or(0);
            let watts: f32 = cols[3].parse().unwrap_or(0.0);
            let domain = map_domain_name(raw_domain);
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: ts,
                t_end_ns: ts,
                category: EventCategory::PowerSample,
                lane_hint: domain_idx,
                payload: EventPayload::PowerSample { domain, watts },
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
                sources: ProfileSourceFlags(ProfileSourceFlags::POWER),
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
    fn rapl_collector_default_id_and_capability() {
        let c = LinuxRaplPowerCollector::new();
        assert_eq!(c.id(), "linux.rapl.power");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::PowerSample));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
        // ARM64 deliberately excluded.
        assert!(!cap.platforms.contains(PlatformSet::LINUX_ARM64));
        assert!(cap.vendor.is_none());
    }

    #[test]
    fn rapl_probe_smoke() {
        let c = LinuxRaplPowerCollector::new();
        match c.probe() {
            ProbeResult::Available { .. } | ProbeResult::Unavailable { .. } => {}
            ProbeResult::NeedsElevation { .. } => panic!("must not request elevation"),
        }
    }

    #[test]
    fn rapl_map_domain_name_known_kinds() {
        assert_eq!(map_domain_name("package-0"), PowerDomain::CpuPkg);
        assert_eq!(map_domain_name("package-1"), PowerDomain::CpuPkg);
        assert_eq!(map_domain_name("core"), PowerDomain::CpuCore);
        assert_eq!(map_domain_name("dram"), PowerDomain::Dram);
        assert_eq!(map_domain_name("dram-0"), PowerDomain::Dram);
        assert_eq!(map_domain_name("uncore"), PowerDomain::Other);
        assert_eq!(map_domain_name("psys"), PowerDomain::Other);
    }

    #[test]
    fn rapl_parser_handles_empty_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("power.csv");
        fs::write(&csv, "timestamp_ns,domain,domain_idx,watts\n").unwrap();
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
        let evs = LinuxRaplPowerParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert!(evs.is_empty());
    }

    #[test]
    fn rapl_parser_emits_events_from_sample_csv() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("power.csv");
        let body = "timestamp_ns,domain,domain_idx,watts\n\
                    1000,package-0,0,15.500000\n\
                    1000,core,1,8.200000\n\
                    1000,dram-0,2,3.100000\n";
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
        let evs = LinuxRaplPowerParser
            .parse(raw, &ctx, &mut names, &mut frames)
            .unwrap();
        assert_eq!(evs.len(), 3);
        match &evs[0].payload {
            EventPayload::PowerSample { domain, watts } => {
                assert_eq!(*domain, PowerDomain::CpuPkg);
                assert!((*watts - 15.5).abs() < 0.01);
            }
            _ => panic!("wrong payload"),
        }
        match &evs[1].payload {
            EventPayload::PowerSample { domain, .. } => {
                assert_eq!(*domain, PowerDomain::CpuCore);
            }
            _ => panic!("wrong payload"),
        }
        match &evs[2].payload {
            EventPayload::PowerSample { domain, .. } => {
                assert_eq!(*domain, PowerDomain::Dram);
            }
            _ => panic!("wrong payload"),
        }
    }
}
