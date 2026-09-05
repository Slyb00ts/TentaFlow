// ===== File: benches/support/mod.rs — shared bench harness plumbing =====
//
// Not itself a `[[bench]]` target: it lives in a subdirectory of `benches/`
// so Cargo's bench auto-discovery (which only scans files directly inside
// `benches/`) does not try to build it as its own binary. Included via
// plain `mod support;` from each bench's crate-root file.
//
// Every bench that writes to disk uses `bench_dir()` here instead of a
// hand-rolled `std::env::temp_dir()` call, so `TENTABUS_BENCH_DIR`
// consistently overrides where benchmark data lands (some Linux setups
// have `/tmp` on tmpfs, which would silently turn a durability benchmark
// into a memory-throughput benchmark) and the chosen path/filesystem is
// always printed once per process.

// This module is shared by all four bench binaries, each of which uses a
// different subset of it — `cfg(any(test, ...))`-style per-binary allows
// would be noisier than one blanket allow for a helpers module that is
// never part of the crate's public API surface.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Duration;

/// Deterministic, non-repeating fill (splitmix64) so lz4 sees realistic,
/// largely-incompressible payloads instead of a trivially-compressible
/// constant byte — a constant fill would make compression throughput and
/// ratio numbers meaningless.
pub fn pseudo_random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed ^ 0x9E3779B97F4A7C15;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(len);
    out
}

pub fn percentile(sorted: &[Duration], q: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let idx = (((sorted.len() - 1) as f64) * q).round() as usize;
    sorted[idx]
}

/// Root directory every bench in this crate writes datasets under.
/// `TENTABUS_BENCH_DIR` overrides the default (`std::env::temp_dir()`) —
/// set it to point at the disk the M0 gate numbers are meant to describe
/// `std::env::temp_dir()` on Linux is frequently tmpfs, which would make
/// every "durability" number free and every "device ceiling" number a
/// memory-bandwidth number instead.
fn bench_root() -> PathBuf {
    match std::env::var_os("TENTABUS_BENCH_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => std::env::temp_dir(),
    }
}

static PRINT_ROOT_ONCE: Once = Once::new();

/// Returns a fresh, empty directory under the bench root for one dataset,
/// removing any stale directory of the same name left by a previous run
/// so distinct labels never share a directory and multiple bench
/// functions in the same file cannot race to delete each other's dataset.
/// Prints the resolved root path and filesystem once per process.
pub fn bench_dir(bench_name: &str, label: &str) -> PathBuf {
    let root = bench_root();
    PRINT_ROOT_ONCE.call_once(|| {
        // `statfs` needs an existing path; the root itself is only ever
        // created lazily by the first dataset directory otherwise, which
        // would make filesystem detection fail on a fresh
        // `TENTABUS_BENCH_DIR`.
        let _ = std::fs::create_dir_all(&root);
        eprintln!(
            "[bench] TENTABUS_BENCH_DIR={} (fs={})",
            root.display(),
            filesystem_name(&root)
        );
    });
    let dir = root.join(format!(
        "tentaflow-bus-bench-{bench_name}-{}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create bench dataset dir");
    dir
}

/// Best-effort filesystem name for `path`'s volume, for the report
/// header. macOS reports the real fs name (`apfs`, `hfs`,
/// `nfs`, ...) via `statfs`; other platforms report a fixed placeholder
/// rather than guessing wrong — accurate-but-limited beats confidently
/// wrong for a number this load-bearing.
#[cfg(target_os = "macos")]
fn filesystem_name(path: &Path) -> String {
    use std::ffi::CString;
    use std::mem::MaybeUninit;

    let Ok(c_path) = CString::new(path.as_os_str().as_encoded_bytes()) else {
        return "unknown".to_string();
    };
    let mut stat = MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `c_path` is a valid NUL-terminated C string for the lifetime
    // of this call; `stat.as_mut_ptr()` points at enough space for one
    // `libc::statfs` (guaranteed by `MaybeUninit`'s layout), which is all
    // `statfs(2)` writes into.
    let ret = unsafe { libc::statfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if ret != 0 {
        return "unknown".to_string();
    }
    // SAFETY: `statfs` returned 0 (success), so `stat` was fully
    // initialized by the call above.
    let stat = unsafe { stat.assume_init() };
    let name_bytes: Vec<u8> = stat
        .f_fstypename
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as u8)
        .collect();
    String::from_utf8_lossy(&name_bytes).into_owned()
}

#[cfg(not(target_os = "macos"))]
fn filesystem_name(_path: &Path) -> String {
    "unknown (filesystem detection only implemented for macOS in this harness)".to_string()
}

/// Runs `warmup` throwaway calls to `f` (discarded entirely, never timed or
/// recorded), then `samples` timed calls whose individual durations are
/// returned sorted — used for every percentile/msg-per-second number this
/// crate's benches report outside of Criterion's own HTML report.
///
/// Criterion's own `iter_custom` warm-up phase calls the supplied routine
/// an indeterminate number of times before the "real" measurement phase
/// begins, with no signal distinguishing the two from inside the routine —
/// so a routine that pushes into a shared `Vec` across every invocation
/// would mix warm-up latencies into the reported p50/p95/p99 and msg/s.
/// This helper sidesteps Criterion's sampler entirely for those custom
/// numbers: warm-up and measurement are two explicit, separate loops driven
/// directly by this function, with nothing but the `samples` loop ever
/// recorded. Criterion's own `bench_with_input`/`iter_custom` calls (used
/// separately, for the HTML report) are unaffected and keep doing their
/// own internal warm-up/measurement.
pub fn measure_latencies<F: FnMut() -> Duration>(
    warmup: usize,
    samples: usize,
    mut f: F,
) -> Vec<Duration> {
    for _ in 0..warmup {
        std::hint::black_box(f());
    }
    let mut latencies = Vec::with_capacity(samples);
    for _ in 0..samples {
        latencies.push(f());
    }
    latencies.sort_unstable();
    latencies
}

pub struct LatencyReport {
    pub n: usize,
    pub mean: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
}

impl LatencyReport {
    pub fn from_sorted(sorted: &[Duration]) -> Self {
        let mean: Duration = if sorted.is_empty() {
            Duration::ZERO
        } else {
            sorted.iter().sum::<Duration>() / sorted.len() as u32
        };
        Self {
            n: sorted.len(),
            mean,
            p50: percentile(sorted, 0.50),
            p95: percentile(sorted, 0.95),
            p99: percentile(sorted, 0.99),
        }
    }

    pub fn ops_per_sec(&self) -> f64 {
        if self.mean.as_secs_f64() > 0.0 {
            1.0 / self.mean.as_secs_f64()
        } else {
            0.0
        }
    }
}
