// ===== File: benches/read_perf.rs — fan-out reads, seeks, pread vs mmap (PLAN §5.4 item 3) =====
//
// Everything here reads data the same process just wrote and fsynced, so it
// stays resident in the OS page cache — this is deliberately *not* a
// cold-disk benchmark (PLAN §5.2 P3: "consume z page cache"). Three things
// are measured: (1) aggregate throughput fanning out to 4 independent
// readers (P3's actual scenario), (2) single-seek latency by offset and by
// time using the sparse indexes, (3) — only with `--features mmap-read` —
// a head-to-head of the production `pread` path against a raw `mmap` scan
// over the same bytes, which is the data `mmap-read`'s on/off decision is
// made from (PLAN §2.1).
//
// `scan_via_pread` reads each batch with the same readahead-buffer,
// single-syscall-in-the-common-case strategy the production
// `PartitionReader::fetch_from_offset` uses (partition.rs's `ReadBuf`),
// instead of two full `pread`s per batch (one that only reads the header,
// then a second that re-reads the header as part of reading the whole
// batch again) — that double-read would be the unfair half of the mmap
// comparison below: `scan_via_mmap` pays neither a syscall nor an
// allocation, so `scan_via_pread` must not pay for either twice. They are
// still not identical (mmap's zero-copy slice vs. pread's mandatory one
// copy into an owned buffer is an inherent difference, not an artifact of
// the harness) — matching the read strategy removes the *artifact*, not
// the fundamental difference the `mmap-read` decision is supposed to be
// measured on.
//
// Each bench function gets its own dataset directory (via
// `support::bench_dir`, keyed by function name) instead of multiple
// functions sharing — and therefore racing to delete — one fixed path.

use std::fs::File;
use std::thread;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use tentaflow_bus::batch::{BatchHeader, BatchView, BATCH_HEADER_LEN};
use tentaflow_bus::{BatchBuilder, Durability, Partition, RecordInput, RollPolicy};

mod support;
use support::bench_dir;

const RECORD_SIZE: usize = 1024;
const RECORDS_PER_BATCH: usize = 64;
const BATCH_COUNT: usize = 800; // ~50 MiB total, comfortably page-cache-resident
/// Matches `partition.rs`'s `READAHEAD_BYTES` — the point of this bench is
/// to measure the same strategy the production reader uses.
const READAHEAD_BYTES: usize = 1024 * 1024;

fn build_dataset(dataset_label: &str) -> (std::path::PathBuf, Partition, u64) {
    let dir = bench_dir("read-perf", dataset_label);
    let partition = Partition::open(&dir, RollPolicy::default(), Durability::FsyncBatch, 64)
        .expect("open partition");

    let payload = vec![0x5Au8; RECORD_SIZE];
    for batch_idx in 0..BATCH_COUNT {
        let mut b = BatchBuilder::with_capacity(0, 1, RECORDS_PER_BATCH * (RECORD_SIZE + 32));
        for r in 0..RECORDS_PER_BATCH {
            let ts = (batch_idx * RECORDS_PER_BATCH + r) as i64;
            b.push(RecordInput::new(Bytes::from(payload.clone()), ts))
                .unwrap();
        }
        partition.append_batch(b.build().unwrap()).unwrap();
    }
    let total_records = partition.log_end_offset();
    (dir, partition, total_records)
}

fn bench_fanout_4_readers(c: &mut Criterion) {
    let (_dir, partition, total_records) = build_dataset("fanout");

    let mut group = c.benchmark_group("read_fanout");
    group.sample_size(15);
    group.throughput(Throughput::Elements(total_records * 4));
    group.bench_function(
        BenchmarkId::from_parameter(format!("4_readers_x_{total_records}_records")),
        |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let start = Instant::now();
                    let handles: Vec<_> = (0..4)
                        .map(|_| {
                            let reader = partition.open_reader();
                            thread::spawn(move || {
                                let mut offset = 0u64;
                                let mut records = 0u64;
                                loop {
                                    let batches =
                                        reader.fetch_from_offset(offset, 4 * 1024 * 1024).unwrap();
                                    if batches.is_empty() {
                                        break;
                                    }
                                    for view in &batches {
                                        records += view.header().record_count as u64;
                                        offset = view.header().next_offset();
                                    }
                                }
                                records
                            })
                        })
                        .collect();
                    let mut records_read = 0u64;
                    for h in handles {
                        records_read += h.join().unwrap();
                    }
                    total += start.elapsed();
                    assert_eq!(records_read, total_records * 4);
                }
                total
            });
        },
    );
    group.finish();
}

fn bench_seeks(c: &mut Criterion) {
    let (_dir, partition, total_records) = build_dataset("seeks");
    let reader = partition.open_reader();
    let mid_offset = total_records / 2;

    let mut group = c.benchmark_group("read_seek");
    group.sample_size(50);

    group.bench_function("seek_by_offset_midpoint", |b| {
        b.iter(|| {
            let batches = reader.fetch_from_offset(mid_offset, 64 * 1024).unwrap();
            assert!(!batches.is_empty());
            std::hint::black_box(&batches);
        });
    });

    let mid_ts = (mid_offset) as i64; // ts assigned == record index in build_dataset
    group.bench_function("seek_by_timestamp_midpoint", |b| {
        b.iter(|| {
            let batches = reader.fetch_from_timestamp(mid_ts, 64 * 1024).unwrap();
            assert!(!batches.is_empty());
            std::hint::black_box(&batches);
        });
    });

    group.finish();
}

/// Sequential full-segment scan via the same readahead-buffer, single
/// -syscall-in-the-common-case strategy `PartitionReader::fetch_from_offset`
/// uses internally — a `readbuf` reused across every batch in the scan, one
/// `pread` covering header+body for any batch that fits inside
/// `READAHEAD_BYTES`.
fn scan_via_pread(file: &File, seg_len: u64) -> (u64, u64) {
    let mut pos = 0u64;
    let mut records = 0u64;
    let mut bytes = 0u64;
    let mut readbuf = vec![0u8; READAHEAD_BYTES];
    while pos + BATCH_HEADER_LEN as u64 <= seg_len {
        let want = READAHEAD_BYTES.min((seg_len - pos) as usize);
        if readbuf.len() < want {
            readbuf.resize(want, 0);
        }
        tentaflow_bus::segment::pread_exact(file, pos, &mut readbuf[..want]).unwrap();
        let header = BatchHeader::decode(&readbuf[..BATCH_HEADER_LEN]).unwrap();
        let total = BATCH_HEADER_LEN as u64 + header.body_len as u64;
        let raw = if total <= want as u64 {
            Bytes::copy_from_slice(&readbuf[..total as usize])
        } else {
            let mut full = vec![0u8; total as usize];
            full[..want].copy_from_slice(&readbuf[..want]);
            tentaflow_bus::segment::pread_exact(file, pos + want as u64, &mut full[want..])
                .unwrap();
            Bytes::from(full)
        };
        let view = BatchView::parse(raw).unwrap();
        records += view.header().record_count as u64;
        bytes += total;
        pos += total;
    }
    (records, bytes)
}

/// Same scan, but batch bytes come from a zero-copy `Bytes` slice of one
/// whole-file `mmap` (mapped once outside the timed loop) instead of a
/// pread + allocation per batch.
#[cfg(feature = "mmap-read")]
fn scan_via_mmap(mapped: &Bytes, seg_len: u64) -> (u64, u64) {
    let mut pos = 0usize;
    let mut records = 0u64;
    let mut bytes = 0u64;
    while pos as u64 + BATCH_HEADER_LEN as u64 <= seg_len {
        let header = BatchHeader::decode(&mapped[pos..pos + BATCH_HEADER_LEN]).unwrap();
        let total = BATCH_HEADER_LEN + header.body_len as usize;
        let view = BatchView::parse(mapped.slice(pos..pos + total)).unwrap();
        records += view.header().record_count as u64;
        bytes += total as u64;
        pos += total;
    }
    (records, bytes)
}

fn bench_pread_vs_mmap(c: &mut Criterion) {
    let (dir, partition, total_records) = build_dataset("pread-vs-mmap");
    drop(partition); // release the writer thread; the file itself stays on disk

    let log_path = tentaflow_bus::segment::log_path(&dir, 0);
    let seg_len = std::fs::metadata(&log_path).unwrap().len();

    let mut group = c.benchmark_group("read_pread_vs_mmap");
    group.sample_size(20);
    group.throughput(Throughput::Elements(total_records));

    group.bench_function("pread_full_scan", |b| {
        let file = File::open(&log_path).unwrap();
        b.iter(|| {
            let (records, _bytes) = scan_via_pread(&file, seg_len);
            assert_eq!(records, total_records);
        });
    });

    #[cfg(feature = "mmap-read")]
    {
        let mmap = tentaflow_bus::segment::mmap_open(&log_path).unwrap();
        let mapped = Bytes::from_owner(mmap);
        group.bench_function("mmap_full_scan", |b| {
            b.iter(|| {
                let (records, _bytes) = scan_via_mmap(&mapped, seg_len);
                assert_eq!(records, total_records);
            });
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_fanout_4_readers,
    bench_seeks,
    bench_pread_vs_mmap
);
criterion_main!(benches);
