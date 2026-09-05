// ===== File: benches/device_ceiling.rs — raw sequential write ceiling (PLAN §5.4 item 2) =====
//
// Bypasses the batch/segment/index machinery entirely: plain positional
// writes + the same fsync policy as `log_perf.rs`, straight to a file. This
// is the denominator every append-throughput number in the M0 report is
// expressed against as "% of device ceiling" (PLAN §5.2).
//
// A file that grows on every write (`write_all` extending it chunk by
// chunk) pays the same journal/metadata-update cost `Segment::create_new`'s
// preallocation exists to avoid — measuring the ceiling against a growing
// file would make "engine / ceiling" land near 100% because numerator and
// denominator hide the *same* cost, not because the engine is efficient.
// This bench therefore measures a `preallocated` variant
// (`F_PREALLOCATE`/`fallocate`, writes into the *middle* of the file at a
// fixed offset so the file never grows during the timed loop at all)
// alongside a `growing` variant, so the gap between the two — not either
// number alone — is what "physics of the disk" actually means here. It
// also measures an `F_FULLFSYNC` variant on macOS (PLAN §5.3.6) alongside
// the `fsync_data`/`sync_data` one, since `sync_data()` alone is not a
// durability barrier on Apple platforms.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

mod support;
use support::{bench_dir, pseudo_random_bytes};

const PREALLOC_TOTAL: u64 = 256 * 1024 * 1024; // 256 MiB reserved up front
const MIDDLE_OFFSET: u64 = 64 * 1024 * 1024; // fixed write position inside it

#[derive(Clone, Copy)]
enum Durability {
    Os,
    FsyncData,
    FsyncFull,
    FsyncInterval(Duration),
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn fsync_full(file: &File) {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `file`'s fd is valid and open for the duration of this call;
    // `F_FULLFSYNC` takes no argument pointer.
    let ret = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) };
    assert_eq!(
        ret,
        0,
        "F_FULLFSYNC failed: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn fsync_full(file: &File) {
    file.sync_data().expect("fsync");
}

fn fsync(file: &File, durability: Durability, last_fsync: &mut Instant) {
    match durability {
        Durability::Os => {}
        Durability::FsyncData => file.sync_data().expect("fsync"),
        Durability::FsyncFull => fsync_full(file),
        Durability::FsyncInterval(interval) => {
            if last_fsync.elapsed() >= interval {
                file.sync_data().expect("fsync");
                *last_fsync = Instant::now();
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn preallocate(file: &File, len: u64) {
    use std::os::unix::io::AsRawFd;
    let mut store = libc::fstore_t {
        fst_flags: libc::F_ALLOCATECONTIG,
        fst_posmode: libc::F_PEOFPOSMODE,
        fst_offset: 0,
        fst_length: len as libc::off_t,
        fst_bytesalloc: 0,
    };
    // SAFETY: `file`'s fd is valid and open for the duration of this call;
    // `store` is a fully-initialized `fstore_t` alive for the call.
    let mut ret = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut store) };
    if ret == -1 {
        store.fst_flags = libc::F_ALLOCATEALL;
        // SAFETY: same as above.
        ret = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PREALLOCATE, &mut store) };
    }
    assert_eq!(
        ret,
        0,
        "F_PREALLOCATE failed: {}",
        std::io::Error::last_os_error()
    );
    file.set_len(len).expect("extend to preallocated size");
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn preallocate(file: &File, len: u64) {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `file`'s fd is valid and open for the duration of this call.
    let ret = unsafe { libc::fallocate(file.as_raw_fd(), 0, 0, len as libc::off_t) };
    assert_eq!(
        ret,
        0,
        "fallocate failed: {}",
        std::io::Error::last_os_error()
    );
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "android")))]
fn preallocate(file: &File, len: u64) {
    file.set_len(len).expect("set_len fallback preallocation");
}

fn bench_device_ceiling(c: &mut Criterion) {
    let chunk_sizes: [usize; 2] = [64 * 1024, 1024 * 1024];
    let mut durabilities: Vec<(&str, Durability)> = vec![
        ("os", Durability::Os),
        ("sync_data", Durability::FsyncData),
        (
            "fsync_interval_100ms",
            Durability::FsyncInterval(Duration::from_millis(100)),
        ),
    ];
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        durabilities.push(("f_fullfsync", Durability::FsyncFull));
    }

    let mut group = c.benchmark_group("device_ceiling");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(3));
    group.warm_up_time(Duration::from_secs(1));

    for chunk_size in chunk_sizes {
        let chunk = pseudo_random_bytes(chunk_size, chunk_size as u64);

        for &(durability_name, durability) in &durabilities {
            // --- growing variant: file extends on every write, exactly
            // what `Segment::append` used to do before preallocation was
            // added. ---
            {
                let label =
                    format!("chunk={chunk_size}B/durability={durability_name}/layout=growing");
                let dir = bench_dir("device-ceiling", &label.replace(['=', '/'], "_"));
                let path = dir.join("device_ceiling.raw");
                let mut file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&path)
                    .expect("open raw file");
                let mut last_fsync = Instant::now();

                group.throughput(Throughput::Bytes(chunk_size as u64));
                group.bench_with_input(BenchmarkId::from_parameter(&label), &label, |b, _| {
                    b.iter_custom(|iters| {
                        let start = Instant::now();
                        for _ in 0..iters {
                            file.write_all(&chunk).expect("write_all");
                            fsync(&file, durability, &mut last_fsync);
                        }
                        start.elapsed()
                    });
                });
                let _ = std::fs::remove_dir_all(&dir);
            }

            // --- preallocated variant: F_PREALLOCATE/fallocate up front,
            // then every write in the timed loop lands at the *same* fixed
            // offset (never extends the file) — isolates pure write+fsync
            // cost from the metadata/journal cost of growing a file. ---
            if chunk_size as u64 <= PREALLOC_TOTAL - MIDDLE_OFFSET {
                let label =
                    format!("chunk={chunk_size}B/durability={durability_name}/layout=preallocated");
                let dir = bench_dir("device-ceiling", &label.replace(['=', '/'], "_"));
                let path = dir.join("device_ceiling.raw");
                let file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&path)
                    .expect("open raw file");
                preallocate(&file, PREALLOC_TOTAL);
                let mut last_fsync = Instant::now();

                group.throughput(Throughput::Bytes(chunk_size as u64));
                group.bench_with_input(BenchmarkId::from_parameter(&label), &label, |b, _| {
                    use std::os::unix::fs::FileExt;
                    b.iter_custom(|iters| {
                        let start = Instant::now();
                        for _ in 0..iters {
                            file.write_all_at(&chunk, MIDDLE_OFFSET).expect("pwrite");
                            fsync(&file, durability, &mut last_fsync);
                        }
                        start.elapsed()
                    });
                });
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }

    group.finish();
}

criterion_group!(benches, bench_device_ceiling);
criterion_main!(benches);
