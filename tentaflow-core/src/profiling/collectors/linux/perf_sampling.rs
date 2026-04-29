#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
// =============================================================================
// File: collectors/linux/perf_sampling.rs — `perf record -F 99 -g` based CPU
// stack sampling collector. Generuje TimelineEvent::CpuSample z stack_id
// wskazujacym na ProfileReportV2.frames + ProfileReportV2.stacks.
// Odblokowuje mockup #06 (CPU Flamegraph) + sekcje "Top symbols" w mockup #07.
//
// Workflow:
//   start  -> spawn `perf record -F 99 -g <-a|--pid PID>` -> tworzy perf.data
//   stop   -> SIGINT do perf -> wait -> file gotowy
//   parse  -> spawn `perf script -i perf.data -F comm,pid,tid,cpu,time,callchain`
//             -> parsowanie linii sample-headerow + frame-lines.
// =============================================================================

use std::collections::HashMap;
use std::fs;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;

use tentaflow_protocol::profiling::{
    ClockSamples, ElevationRequirement, EventCategory, EventPayload, TimelineEvent,
};

use crate::profiling::collectors::{
    CollectorCapability, CollectorError, CollectorParser, FrameInterner, FrameKey, NameInterner,
    PlatformSet, ProbeResult, ProfileCollector, RawCapture, RunningCollector, SessionCtx,
};

const COLLECTOR_ID: &str = "linux.perf.cpu_sampling";
const PERF_DATA_FILENAME: &str = "perf.data";

/// CPU stack sampling collector — wraps perf record.
pub struct LinuxPerfSamplingCollector {
    capability: CollectorCapability,
    id: String,
}

impl LinuxPerfSamplingCollector {
    pub fn new() -> Self {
        Self {
            capability: CollectorCapability {
                categories: vec![EventCategory::CpuSample],
                // System-wide profiling moze wymagac perf_event_paranoid <= 1
                // lub CAP_PERFMON. Probe sprawdza paranoid; gdy >= 2 to dalej
                // dziala, bo padniemy do --pid trybu zamiast -a.
                elevation: ElevationRequirement::None,
                platforms: PlatformSet::from_flags(
                    PlatformSet::LINUX_X64 | PlatformSet::LINUX_ARM64,
                ),
                vendor: None,
                description:
                    "Stack-trace sampling 99 Hz (perf record -F 99 -g) — flamegraph + hot symbols.",
            },
            id: COLLECTOR_ID.to_string(),
        }
    }
}

impl Default for LinuxPerfSamplingCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl ProfileCollector for LinuxPerfSamplingCollector {
    fn id(&self) -> &str {
        &self.id
    }

    fn capability(&self) -> &CollectorCapability {
        &self.capability
    }

    fn probe(&self) -> ProbeResult {
        #[cfg(target_os = "linux")]
        {
            // 1) `perf` w PATH.
            let perf = match which::which("perf") {
                Ok(p) => p,
                Err(_) => {
                    return ProbeResult::Unavailable {
                        reason: "perf not found in PATH (install linux-tools-common / perf)".into(),
                    };
                }
            };
            // 2) `perf --version` -> version string.
            let version = Command::new(&perf)
                .arg("--version")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .ok()
                .and_then(|o| {
                    if !o.status.success() {
                        return None;
                    }
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .map(|s| s.trim().to_string())
                });
            // 3) /proc/sys/kernel/perf_event_paranoid -> wskaznik czy uda sie
            // -a (system-wide). Jesli >= 3, perf record -a sie nie uda; padamy
            // do trybu --pid wlasnym procesem ktorego paranoid >=3 nadal blokuje.
            // Probe nadal zwraca Available — start() ma fallback.
            ProbeResult::Available { version }
        }
        #[cfg(not(target_os = "linux"))]
        {
            ProbeResult::Unavailable {
                reason: "linux.perf.cpu_sampling is Linux-only".into(),
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn start(&self, ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        fs::create_dir_all(&ctx.output_dir)?;
        let perf_data_path = ctx.output_dir.join(PERF_DATA_FILENAME);
        let started_at = Instant::now();
        let start_clock_ns = read_monotonic_ns();
        let samples_observed = Arc::new(AtomicU64::new(0));

        let hz = ctx.scope.cpu_sampling_hz.max(1).min(999);

        // Decyzja o trybie -a vs --pid:
        //   target == OwnProcess -> --pid <pid> (zawsze dziala, niezaleznie od paranoid)
        //   target == Pid(p)     -> --pid p
        //   target == SystemWide -> -a (wymaga paranoid <= 1 albo CAP_PERFMON)
        // Gdy SystemWide a paranoid blokuje -> fallback do --pid biezacego procesu.
        use tentaflow_protocol::profiling::ProfileTarget;
        let mut cmd = Command::new("perf");
        cmd.arg("record")
            .arg("-F")
            .arg(hz.to_string())
            .arg("-g")
            .arg("--call-graph")
            .arg("dwarf,8192")
            .arg("-o")
            .arg(&perf_data_path);
        match &ctx.scope.target {
            ProfileTarget::OwnProcess => {
                cmd.arg("--pid").arg(std::process::id().to_string());
            }
            ProfileTarget::Pid(p) => {
                cmd.arg("--pid").arg(p.to_string());
            }
            ProfileTarget::SystemWide => {
                let paranoid = read_paranoid_level();
                if paranoid > 1 {
                    // Bezpieczny fallback - tracujemy wlasny proces zamiast pelnego systemu.
                    cmd.arg("--pid").arg(std::process::id().to_string());
                } else {
                    cmd.arg("-a");
                }
            }
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let child = cmd
            .spawn()
            .map_err(|e| CollectorError::Spawn(format!("perf record spawn: {e}")))?;

        Ok(Box::new(LinuxPerfSamplingRunning {
            id: COLLECTOR_ID.to_string(),
            output_dir: ctx.output_dir.clone(),
            perf_data_path,
            child: Mutex::new(Some(child)),
            samples_observed,
            started_at,
            start_clock_ns,
        }))
    }

    #[cfg(not(target_os = "linux"))]
    fn start(&self, _ctx: SessionCtx) -> Result<Box<dyn RunningCollector>, CollectorError> {
        Err(CollectorError::Custom(
            "linux.perf.cpu_sampling is Linux-only".into(),
        ))
    }
}

/// Live perf record sub-process.
pub struct LinuxPerfSamplingRunning {
    id: String,
    output_dir: PathBuf,
    perf_data_path: PathBuf,
    child: Mutex<Option<std::process::Child>>,
    samples_observed: Arc<AtomicU64>,
    started_at: Instant,
    start_clock_ns: u64,
}

impl RunningCollector for LinuxPerfSamplingRunning {
    fn collector_id(&self) -> &str {
        &self.id
    }

    fn stop(self: Box<Self>) -> Result<RawCapture, CollectorError> {
        // SIGINT -> perf record graceful shutdown -> file is written.
        let mut child = match self.child.lock() {
            Ok(mut g) => g.take(),
            Err(_) => None,
        };
        if let Some(ref mut ch) = child {
            #[cfg(target_os = "linux")]
            unsafe {
                libc::kill(ch.id() as libc::pid_t, libc::SIGINT);
            }
            // Czekaj do 5s.
            let deadline = Instant::now() + std::time::Duration::from_secs(5);
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
        metadata.insert("source".into(), "perf record -F 99 -g".into());
        metadata.insert("format".into(), "perf.data".into());

        let artifacts = if self.perf_data_path.exists() {
            vec![self.perf_data_path.clone()]
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

/// Parser — wola `perf script -i perf.data` i parsuje linie callchain.
pub struct LinuxPerfSamplingParser;

impl CollectorParser for LinuxPerfSamplingParser {
    fn parse(
        &self,
        raw: RawCapture,
        _ctx: &SessionCtx,
        names: &mut NameInterner,
        frames: &mut FrameInterner,
    ) -> Result<Vec<TimelineEvent>, CollectorError> {
        let Some(perf_data) = raw.artifacts.first() else {
            return Ok(Vec::new());
        };
        if !perf_data.exists() {
            return Ok(Vec::new());
        }

        // perf script -F comm,pid,tid,cpu,time,sym,dso --no-demangle
        // Output: blok per sample - header line + frame lines z wcieciem.
        // Dla typowej sesji perf.data ~10-100 MB, output text duzy wiec
        // czytamy calosc do String potem parsujemy linia po linii.
        let mut cmd = Command::new("perf");
        cmd.arg("script")
            .arg("-i")
            .arg(perf_data)
            .arg("-F")
            .arg("comm,pid,tid,cpu,time,sym,dso")
            .arg("--no-demangle")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let output = match cmd.output() {
            Ok(o) => o,
            Err(_) => return Ok(Vec::new()),
        };
        if !output.status.success() {
            return Ok(Vec::new());
        }
        let text = String::from_utf8_lossy(&output.stdout);

        Ok(parse_perf_script(&text, names, frames))
    }
}

/// Czysta funkcja - parsuje text output `perf script -F comm,pid,tid,cpu,time,sym,dso`.
/// Format:
///   header: "tentaflow 14872/14872 [001] 12345.678901: cycles:"
///   frames: "        ffffffff cafebabe symbol_name+0x10 (/path/to/binary)"
///   blank line  -> end of sample
fn parse_perf_script(
    text: &str,
    names: &mut NameInterner,
    frames: &mut FrameInterner,
) -> Vec<TimelineEvent> {
    let mut events = Vec::new();
    let mut cur_frames: Vec<u32> = Vec::new();
    let mut cur_tid: u32 = 0;
    let mut cur_cpu: u16 = 0;
    let mut cur_t_ns: u64 = 0;
    let mut have_header = false;

    let flush =
        |events: &mut Vec<TimelineEvent>,
         cur_frames: &mut Vec<u32>,
         frames_intern: &mut FrameInterner,
         tid: u32,
         cpu: u16,
         t_ns: u64| {
            if cur_frames.is_empty() {
                return;
            }
            // Stack: leaf-first order (perf script).
            let stack_id = frames_intern.intern_stack(std::mem::take(cur_frames));
            events.push(TimelineEvent {
                source_idx: 0,
                t_start_ns: t_ns,
                t_end_ns: t_ns,
                category: EventCategory::CpuSample,
                lane_hint: cpu,
                payload: EventPayload::CpuSample { tid, cpu, stack_id },
            });
        };

    for line in text.lines() {
        if line.trim().is_empty() {
            if have_header {
                flush(&mut events, &mut cur_frames, frames, cur_tid, cur_cpu, cur_t_ns);
            }
            have_header = false;
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            // Header line (e.g. "tentaflow 14872/14872 [001] 12345.678901: cycles:").
            if have_header {
                flush(&mut events, &mut cur_frames, frames, cur_tid, cur_cpu, cur_t_ns);
            }
            if let Some((tid, cpu, t_ns)) = parse_header(line) {
                cur_tid = tid;
                cur_cpu = cpu;
                cur_t_ns = t_ns;
                have_header = true;
            } else {
                have_header = false;
            }
        } else if have_header {
            // Frame line: leading spaces, then "<addr> <symbol> (<dso>)".
            if let Some(frame_id) = parse_frame_line(line, names, frames) {
                cur_frames.push(frame_id);
            }
        }
    }
    if have_header {
        flush(&mut events, &mut cur_frames, frames, cur_tid, cur_cpu, cur_t_ns);
    }
    events
}

/// "tentaflow 14872/14872 [001] 12345.678901: cycles:" -> (tid, cpu, t_ns).
fn parse_header(line: &str) -> Option<(u32, u16, u64)> {
    let mut it = line.split_ascii_whitespace();
    // skip comm (may have multiple words for kernel threads — heuristic: token
    // containing '/' is pid/tid; before that = comm).
    let mut tid: Option<u32> = None;
    let mut cpu: Option<u16> = None;
    let mut t_ns: Option<u64> = None;
    while let Some(tok) = it.next() {
        if tid.is_none() && tok.contains('/') {
            // pid/tid
            let parts: Vec<&str> = tok.split('/').collect();
            if parts.len() == 2 {
                tid = parts[1].parse().ok();
            }
        } else if cpu.is_none() && tok.starts_with('[') && tok.ends_with(']') {
            cpu = tok.trim_start_matches('[').trim_end_matches(']').parse().ok();
        } else if t_ns.is_none() && tok.ends_with(':') {
            // 12345.678901: -> seconds.nanoseconds. We want monotonic ns.
            let core = tok.trim_end_matches(':');
            if let Some((sec, frac)) = core.split_once('.') {
                let s: u64 = sec.parse().ok()?;
                // frac up to 9 digits
                let mut frac_padded = String::from(frac);
                while frac_padded.len() < 9 {
                    frac_padded.push('0');
                }
                frac_padded.truncate(9);
                let ns: u64 = frac_padded.parse().ok()?;
                t_ns = Some(s.saturating_mul(1_000_000_000).saturating_add(ns));
            }
        }
    }
    Some((tid.unwrap_or(0), cpu.unwrap_or(0), t_ns.unwrap_or(0)))
}

/// "        ffffffff cafebabe symbol_name+0x10 (/path/to/binary)" -> frame id.
fn parse_frame_line(
    line: &str,
    _names: &mut NameInterner,
    frames: &mut FrameInterner,
) -> Option<u32> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Format: "<address> <symbol+offset> (<dso>)"
    let mut it = trimmed.splitn(2, ' ');
    let _addr = it.next()?;
    let rest = it.next()?;

    // dso w nawiasach na koncu.
    let (sym_part, dso_part) = if let Some(open) = rest.rfind(" (") {
        let dso = rest[open + 2..].trim_end_matches(')').trim().to_string();
        (rest[..open].to_string(), Some(dso))
    } else {
        (rest.to_string(), None)
    };
    // Strip "+0x..." offset.
    let symbol = if let Some(plus) = sym_part.rfind('+') {
        sym_part[..plus].trim().to_string()
    } else {
        sym_part.trim().to_string()
    };
    if symbol.is_empty() {
        return None;
    }

    let key = FrameKey {
        symbol,
        module: dso_part.unwrap_or_default(),
        file: None,
        line: None,
    };
    Some(frames.intern_frame(key))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_default_id_and_capability() {
        let c = LinuxPerfSamplingCollector::new();
        assert_eq!(c.id(), "linux.perf.cpu_sampling");
        let cap = c.capability();
        assert!(cap.categories.contains(&EventCategory::CpuSample));
        assert!(cap.platforms.contains(PlatformSet::LINUX_X64));
    }

    #[test]
    fn parse_header_full_line() {
        let h = "tentaflow 14872/14881 [001] 12345.678901234: cycles:";
        let (tid, cpu, t_ns) = parse_header(h).unwrap();
        assert_eq!(tid, 14881);
        assert_eq!(cpu, 1);
        assert_eq!(t_ns, 12345 * 1_000_000_000 + 678_901_234);
    }

    #[test]
    fn parse_perf_script_full_block() {
        let text = "\
tentaflow 14872/14881 [001] 12.500000000: cycles:
        ffffffff abc tokenize::lex+0x10 (/usr/local/bin/tentaflow)
        ffffffff def main+0x20 (/usr/local/bin/tentaflow)

tentaflow 14872/14881 [002] 12.510000000: cycles:
        ffffffff cab json::parse+0x40 (/usr/local/bin/tentaflow)
        ffffffff dab main+0x20 (/usr/local/bin/tentaflow)
";
        let mut names = NameInterner::default();
        let mut frames = FrameInterner::default();
        let events = parse_perf_script(text, &mut names, &mut frames);
        assert_eq!(events.len(), 2);
        for e in &events {
            assert_eq!(e.category, EventCategory::CpuSample);
            if let EventPayload::CpuSample { tid, cpu, stack_id: _ } = e.payload {
                assert_eq!(tid, 14881);
                assert!(cpu == 1 || cpu == 2);
            } else {
                panic!("expected CpuSample payload");
            }
        }
    }
}
