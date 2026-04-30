#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux/uncore_imc.rs — RAM bandwidth via uncore counters.
// Wytwarza TimelineEvent::RamBandwidth z agregowanych liczników memory
// controller'a. Odblokowuje mockup #09 sekcję "RAM bandwidth (uncore counters)".
//
// Auto-detect:
//   Intel uncore_imc_*  -> events: cas_count_read, cas_count_write
//                          (cache-line 64B; mnozymy aby uzyskac bytes/s)
//   AMD amd_df          -> events: amd_df/event=0x07,umask=0x38/  (DRAM channel
//                          activity). Mniej precyzyjne; uzywamy proxy:
//                          umc/dram_read i dram_write z amd_df gdy dostepne.
//
// Probe wymaga `perf` w PATH + perf_event_paranoid<=0 (uncore wymaga root
// w wiekszosci przypadkow). Bez root probe zwraca NeedsElevation; orchestrator
// pomija collector chyba ze user dostarczyl sudo password.
// =============================================================================

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, ElevationKind, FrameInterner,
    NameInterner, PlatformSet, ProbeResult, ProfileCollector, RawCapture, RunningCollector,
    SessionCtx,
};

const COLLECTOR_ID: &str = "linux.uncore.imc";
const CSV_FILENAME: &str = "uncore_imc.csv";
const SAMPLE_INTERVAL_MS: u32 = 1000;
const CACHE_LINE_BYTES: u64 = 64;

pub struct LinuxUncoreImcCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxUncoreImcCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::RamBandwidth],
                // sudo/root - perf_event_paranoid najczesciej domyslnie 2-4
                // (Fedora/Ubuntu), uncore counters wymaga 0. Rozwiazanie:
                // uruchomic z sudo (uniwersalnie) ALBO setcap CAP_PERFMON.
                elevation: ElevationRequirement::Sudo,
                platforms: PlatformSet::from_flags(PlatformSet::LINUX_X64),
                vendor: None,
                description:
                    "RAM bandwidth via uncore IMC counters (Intel uncore_imc_* / AMD amd_df). Requires sudo.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxUncoreImcCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for LinuxUncoreImcCollector {
    fn id(&self) -> &str {
        &self.id
    }
    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }
    fn probe(&self) -> ProbeResult {
        #[cfg(target_os = "linux")]
        {
            if which::which("perf").is_err() {
                return ProbeResult::Unavailable {
                    reason: "perf not in PATH".into(),
                };
            }
            let events = detect_uncore_events();
            if events.is_empty() {
                return ProbeResult::Unavailable {
                    reason: "no uncore_imc_* (Intel) ani amd_df (AMD) events found via perf list"
                        .into(),
                };
            }
            // Sprawdz paranoid level - uncore zazwyczaj wymaga 0.
            let paranoid = read_paranoid_level();
            if paranoid > 0 {
                return ProbeResult::NeedsElevation {
                    kind: ElevationKind::Sudo,
                    reason: format!(
                        "perf_event_paranoid={paranoid} blocks uncore IMC; need sudo or paranoid<=0"
                    ),
                };
            }
            ProbeResult::Available { version: None }
        }
        #[cfg(not(target_os = "linux"))]
        {
            ProbeResult::Unavailable {
                reason: "linux.uncore.imc is Linux-only".into(),
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        fs::create_dir_all(&ctx.output_dir)?;
        let csv_path = ctx.output_dir.join(CSV_FILENAME);
        let started_at = Instant::now();
        let start_clock_ns = read_monotonic_ns();

        let events = detect_uncore_events();
        if events.is_empty() {
            return Err(CollectorError::Custom(
                "no uncore IMC events detected on this CPU".into(),
            ));
        }
        let events_arg = events.join(",");

        let mut cmd = Command::new("perf");
        cmd.arg("stat")
            .arg("-I")
            .arg(SAMPLE_INTERVAL_MS.to_string())
            .arg("-x")
            .arg(",")
            .arg("-e")
            .arg(events_arg)
            .arg("-a")
            .arg("-o")
            .arg(&csv_path)
            .arg("--")
            .arg("sleep")
            .arg("99999");
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd
            .spawn()
            .map_err(|e| CollectorError::Spawn(format!("perf stat (uncore) spawn: {e}")))?;

        Ok(Box::new(LinuxUncoreImcRunning {
            id: COLLECTOR_ID.to_string(),
            output_dir: ctx.output_dir.clone(),
            csv_path,
            child: Mutex::new(Some(child)),
            samples_observed: Arc::new(AtomicU64::new(0)),
            started_at,
            start_clock_ns,
        }))
    }

    #[cfg(not(target_os = "linux"))]
    fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        Err(CollectorError::Custom(
            "linux.uncore.imc is Linux-only".into(),
        ))
    }
}

pub struct LinuxUncoreImcRunning {
    id: String,
    output_dir: PathBuf,
    csv_path: PathBuf,
    child: Mutex<Option<std::process::Child>>,
    samples_observed: Arc<AtomicU64>,
    started_at: Instant,
    start_clock_ns: u64,
}

impl RunningCollector for LinuxUncoreImcRunning {
    fn collector_id(&self) -> &str {
        &self.id
    }
    fn stop(self: Box<Self>) -> Result<RawCapture, CollectorError> {
        let mut child = self.child.lock().ok().and_then(|mut g| g.take());
        if let Some(ref mut ch) = child {
            #[cfg(target_os = "linux")]
            unsafe {
                libc::kill(ch.id() as libc::pid_t, libc::SIGINT);
            }
            let deadline = Instant::now() + std::time::Duration::from_secs(3);
            loop {
                match ch.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if Instant::now() > deadline {
                            let _ = ch.kill();
                            let _ = ch.wait();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(_) => break,
                }
            }
        }
        let end_clock_ns = read_monotonic_ns();
        let end_session_ns = self.started_at.elapsed().as_nanos() as u64;

        let mut metadata: HashMap<String, String> = HashMap::new();
        metadata.insert("source".into(), "perf stat -e uncore_imc_*/amd_df".into());
        metadata.insert("cache_line_bytes".into(), CACHE_LINE_BYTES.to_string());

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
    fn abort(self: Box<Self>) {
        if let Ok(mut g) = self.child.lock() {
            if let Some(mut ch) = g.take() {
                let _ = ch.kill();
                let _ = ch.wait();
            }
        }
        let _ = fs::remove_dir_all(&self.output_dir);
    }
}

#[cfg(target_os = "linux")]
fn detect_uncore_events() -> Vec<String> {
    // perf list -e uncore* zwraca lite eventow per channel; intel ma dziesiatki
    // (uncore_imc_0/cas_count_read/, uncore_imc_1/..., ...). AMD ma amd_df
    // z parametrami. Probujemy oba i bierzemy co znajdziemy.
    let out = match Command::new("perf")
        .arg("list")
        .arg("-x")
        .arg(",")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut events = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split(',').collect();
        if cols.is_empty() {
            continue;
        }
        let name = cols[0].trim();
        // Intel uncore_imc_<N>/cas_count_{read,write}/
        if name.starts_with("uncore_imc_")
            && (name.ends_with("cas_count_read") || name.ends_with("cas_count_write"))
        {
            events.push(name.to_string());
        }
        // AMD amd_df/event=0x07,umask=0x38/ - DRAM activity (Zen2+ proxy).
        // Najprostsze: jezeli widzimy hasla 'amd_df' to dolaczamy znany default.
    }
    if events.is_empty() {
        // AMD fallback - use raw event encoding which works on Zen 2+
        if text.contains("amd_df") {
            // amd_df/event=0x1f,umask=0x38/  - mem bw read (zoptymalizowane Zen3+)
            events.push("amd_df/event=0x1f,umask=0x38/".to_string());
            events.push("amd_df/event=0x07,umask=0x38/".to_string());
        }
    }
    events
}

#[cfg(target_os = "linux")]
fn read_paranoid_level() -> i32 {
    fs::read_to_string("/proc/sys/kernel/perf_event_paranoid")
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .unwrap_or(2)
}

#[cfg(target_os = "linux")]
fn read_monotonic_ns() -> u64 {
    use std::mem::MaybeUninit;
    let mut ts: MaybeUninit<libc::timespec> = MaybeUninit::uninit();
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, ts.as_mut_ptr());
    }
    let ts = unsafe { ts.assume_init() };
    (ts.tv_sec as u64) * 1_000_000_000 + (ts.tv_nsec as u64)
}

#[cfg(not(target_os = "linux"))]
fn read_monotonic_ns() -> u64 {
    0
}

pub struct LinuxUncoreImcParser;

impl CollectorParser for LinuxUncoreImcParser {
    fn parse(
        &self,
        raw: RawCapture,
        _ctx: &SessionCtx,
        _names: &mut NameInterner,
        _frames: &mut FrameInterner,
    ) -> Result<Vec<TimelineEvent>, CollectorError> {
        let Some(csv) = raw.artifacts.first() else {
            return Ok(Vec::new());
        };
        let content = match fs::read_to_string(csv) {
            Ok(s) => s,
            Err(_) => return Ok(Vec::new()),
        };
        Ok(parse_uncore_csv(&content))
    }
}

fn parse_uncore_csv(content: &str) -> Vec<TimelineEvent> {
    let mut events = Vec::new();
    // interval_ns -> (sum_read_ops, sum_write_ops)
    let mut buckets: HashMap<u64, (u64, u64)> = HashMap::new();
    for line in content.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 4 {
            continue;
        }
        let interval_s: f64 = match cols[0].trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let value: u64 = match cols[1].trim().parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let event_name = cols[3].trim();
        let interval_ns = (interval_s * 1e9) as u64;
        let entry = buckets.entry(interval_ns).or_insert((0, 0));
        if event_name.contains("read") || event_name.contains("0x1f") {
            entry.0 = entry.0.saturating_add(value);
        } else if event_name.contains("write") || event_name.contains("0x07") {
            entry.1 = entry.1.saturating_add(value);
        }
    }
    let mut intervals: Vec<u64> = buckets.keys().copied().collect();
    intervals.sort();
    let interval_s = SAMPLE_INTERVAL_MS as f64 / 1000.0;
    for ts_ns in intervals {
        let (r_ops, w_ops) = buckets.get(&ts_ns).copied().unwrap_or((0, 0));
        // Cast operations -> bytes (cache line = 64B) -> bytes per second.
        let read_bytes = r_ops.saturating_mul(CACHE_LINE_BYTES);
        let write_bytes = w_ops.saturating_mul(CACHE_LINE_BYTES);
        let read_bps = ((read_bytes as f64) / interval_s) as u64;
        let write_bps = ((write_bytes as f64) / interval_s) as u64;
        events.push(TimelineEvent {
            source_idx: 0,
            t_start_ns: ts_ns,
            t_end_ns: ts_ns,
            category: EventCategory::RamBandwidth,
            lane_hint: 0,
            payload: EventPayload::RamBandwidth {
                read_bps,
                write_bps,
            },
        });
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_default_id_and_capability() {
        let c = LinuxUncoreImcCollector::new();
        assert_eq!(c.id(), "linux.uncore.imc");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::RamBandwidth));
        assert_eq!(cap.elevation, ElevationRequirement::Sudo);
    }

    #[test]
    fn parse_uncore_csv_intel_two_intervals() {
        let csv = "\
1.000,1000000,,uncore_imc_0/cas_count_read/,1000000000,100.00,,
1.000,500000,,uncore_imc_0/cas_count_write/,1000000000,100.00,,
1.000,1000000,,uncore_imc_1/cas_count_read/,1000000000,100.00,,
1.000,500000,,uncore_imc_1/cas_count_write/,1000000000,100.00,,
2.000,2000000,,uncore_imc_0/cas_count_read/,1000000000,100.00,,
2.000,1000000,,uncore_imc_0/cas_count_write/,1000000000,100.00,,
";
        let events = parse_uncore_csv(csv);
        assert_eq!(events.len(), 2);
        // 1st interval: read = (1M + 1M) * 64B = 128 MB; per second = 128 MB/s.
        if let EventPayload::RamBandwidth {
            read_bps,
            write_bps,
        } = &events[0].payload
        {
            assert_eq!(*read_bps, 2_000_000 * 64);
            assert_eq!(*write_bps, 1_000_000 * 64);
        } else {
            panic!("expected RamBandwidth");
        }
        // 2nd interval: read = 2M * 64B = 128 MB.
        if let EventPayload::RamBandwidth {
            read_bps,
            write_bps,
        } = &events[1].payload
        {
            assert_eq!(*read_bps, 2_000_000 * 64);
            assert_eq!(*write_bps, 1_000_000 * 64);
        } else {
            panic!("expected RamBandwidth");
        }
    }
}
