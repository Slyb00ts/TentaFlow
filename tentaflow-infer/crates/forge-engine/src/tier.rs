// ===== File: tier.rs — KV-cache tiering: VRAM pages spill to pinned RAM and NVMe in chunks =====
// SPEC §5.4: the spill unit is a contiguous multi-MB chunk per (sequence,
// layer-group; v1 = one group spanning all layers), never page-by-page to
// disk. Chunk layout: for each layer l, [K pages][V pages] back to back, so a
// single layer's region is one contiguous slice — the streamed attention path
// restores per layer with two copies per chunk. The NVMe file is append-only;
// pinned-RAM chunks demote to it FIFO when the RAM budget fills. Restore vs
// recompute follows measured bandwidths (EMA-updated from real transfers) and
// every decision is logged.

use std::cell::Cell;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use forge_hal::{DevBuffer, Device, Pool, Stream};
use forge_types::{ForgeError, MemKind, Result};

use crate::kv::{KvCache, SeqKv, SpilledRange};

/// Most-recent pages of a sequence that are never spilled: decode appends
/// into the last page and the streamed copy-back assumes the tail is resident.
pub const HOT_TAIL_PAGES: usize = 4;

/// Aggregation target for one spill chunk (SPEC §5.4 mandates 4–16 MB
/// append-only chunks; small random I/O to disk is the known failure mode).
const TARGET_CHUNK_BYTES: usize = 8 << 20;
const MAX_CHUNK_BYTES: usize = 16 << 20;

/// Default bandwidth priors used until the first real transfer refines them
/// (bytes/s for transfers, tokens/s for prefill recompute).
const DEFAULT_H2D_BPS: f64 = 10e9;
const DEFAULT_D2H_BPS: f64 = 10e9;
const DEFAULT_DISK_BPS: f64 = 1.5e9;
const DEFAULT_PREFILL_TPS: f64 = 1500.0;

/// Where spilled KV chunks live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KvTierMode {
    /// No tiering: today's VRAM-only behavior.
    Off,
    /// VRAM → pinned host RAM (bounded by the RAM budget; exhaustion errors).
    Ram,
    /// VRAM → pinned RAM → append-only NVMe file (RAM acts as the warm cache).
    Nvme,
}

#[derive(Clone, Debug)]
pub struct KvTierConfig {
    pub mode: KvTierMode,
    /// NVMe spill directory (`Nvme` mode). `None` = a per-process directory
    /// under the system temp dir, removed on drop.
    pub dir: Option<PathBuf>,
    /// Pinned host RAM budget for warm chunks.
    pub ram_budget_bytes: usize,
    /// Spill proactively when free VRAM pages / total pages falls below this.
    pub watermark: f64,
}

impl Default for KvTierConfig {
    fn default() -> Self {
        Self {
            mode: KvTierMode::Off,
            dir: None,
            ram_budget_bytes: 8 << 30,
            watermark: 0.10,
        }
    }
}

impl KvTierConfig {
    pub fn enabled(&self) -> bool {
        self.mode != KvTierMode::Off
    }
}

/// Smallest VRAM page count a tiered sequence needs simultaneously resident:
/// one full prefill chunk being appended plus the hot tail. Serve admission
/// uses this as the tier-mode floor instead of the request's full page demand.
pub fn min_resident_pages(page_size: usize) -> usize {
    crate::model::MAX_PREFILL_CHUNK.div_ceil(page_size) + HOT_TAIL_PAGES + 1
}

enum ChunkLoc {
    /// Warm: the chunk bytes sit in one pinned host buffer.
    Ram(DevBuffer),
    /// Cold: the chunk bytes sit at `offset` in the append-only spill file.
    File { offset: u64 },
}

struct Chunk {
    pages: usize,
    bytes: usize,
    loc: ChunkLoc,
}

pub struct TierManager {
    cfg: KvTierConfig,
    device: Arc<dyn Device>,
    n_layers: usize,
    /// Bytes of ONE page of ONE layer's K (or V) region in the cache dtype.
    page_bytes: usize,
    /// Pages aggregated per spill chunk (sized into the 4–16 MB band).
    chunk_pages: usize,
    chunks: HashMap<u64, Chunk>,
    /// FIFO of warm chunk ids for RAM→NVMe demotion.
    ram_order: Vec<u64>,
    next_id: u64,
    ram_used: usize,
    file: Option<File>,
    file_len: u64,
    /// Spill file path + whether the default directory should be removed.
    file_path: Option<PathBuf>,
    default_dir: Option<PathBuf>,
    /// Pinned staging for cold-chunk reads (grown on demand).
    scratch: Option<DevBuffer>,
    // Measured bandwidths (EMA over real transfers) driving the
    // transfer-vs-recompute rule. Cells: updated from &self staging paths.
    d2h_bps: Cell<f64>,
    h2d_bps: Cell<f64>,
    disk_write_bps: Cell<f64>,
    disk_read_bps: Cell<f64>,
    prefill_tps: Cell<f64>,
    // Cumulative counters for reporting.
    spilled_bytes: Cell<u64>,
    restored_bytes: Cell<u64>,
    streamed_bytes: Cell<u64>,
}

fn ema(cell: &Cell<f64>, sample: f64, default: f64) {
    if !sample.is_finite() || sample <= 0.0 {
        return;
    }
    let old = cell.get();
    cell.set(if old == default {
        sample
    } else {
        0.7 * old + 0.3 * sample
    });
}

impl TierManager {
    pub fn new(
        cfg: KvTierConfig,
        device: Arc<dyn Device>,
        n_layers: usize,
        page_bytes: usize,
    ) -> Result<Self> {
        let per_page = n_layers * 2 * page_bytes;
        let mut chunk_pages = (TARGET_CHUNK_BYTES / per_page.max(1)).max(1);
        if chunk_pages * per_page > MAX_CHUNK_BYTES {
            chunk_pages = (MAX_CHUNK_BYTES / per_page.max(1)).max(1);
        }
        let (file, file_len, file_path, default_dir) = if cfg.mode == KvTierMode::Nvme {
            let (dir, default_dir) = match &cfg.dir {
                Some(d) => (d.clone(), None),
                None => {
                    let d = std::env::temp_dir()
                        .join(format!("forge-kv-tier-{}", std::process::id()));
                    (d.clone(), Some(d))
                }
            };
            std::fs::create_dir_all(&dir).map_err(|e| {
                ForgeError::Scheduler(format!("create kv tier dir {}: {e}", dir.display()))
            })?;
            let path = dir.join("kv.tier");
            let file = File::options()
                .create(true)
                .truncate(true)
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|e| {
                    ForgeError::Scheduler(format!("open kv tier file {}: {e}", path.display()))
                })?;
            (Some(file), 0u64, Some(path), default_dir)
        } else {
            (None, 0, None, None)
        };
        tracing::info!(
            "kv tiering on: mode={:?} chunk={} pages ({:.1} MiB) ram_budget={:.1} GiB watermark={:.0}%",
            cfg.mode,
            chunk_pages,
            (chunk_pages * per_page) as f64 / (1 << 20) as f64,
            cfg.ram_budget_bytes as f64 / (1u64 << 30) as f64,
            cfg.watermark * 100.0
        );
        Ok(Self {
            cfg,
            device,
            n_layers,
            page_bytes,
            chunk_pages,
            chunks: HashMap::new(),
            ram_order: Vec::new(),
            next_id: 1,
            ram_used: 0,
            file,
            file_len,
            file_path,
            default_dir,
            scratch: None,
            d2h_bps: Cell::new(DEFAULT_D2H_BPS),
            h2d_bps: Cell::new(DEFAULT_H2D_BPS),
            disk_write_bps: Cell::new(DEFAULT_DISK_BPS),
            disk_read_bps: Cell::new(DEFAULT_DISK_BPS),
            prefill_tps: Cell::new(DEFAULT_PREFILL_TPS),
            spilled_bytes: Cell::new(0),
            restored_bytes: Cell::new(0),
            streamed_bytes: Cell::new(0),
        })
    }

    /// VRAM pages to keep free beyond immediate need (spill watermark).
    pub fn reserve_pages(&self, n_pages: usize) -> usize {
        ((n_pages as f64) * self.cfg.watermark).ceil() as usize
    }

    /// Byte offset of layer `l`'s region inside a chunk of `pages` pages
    /// (K slice first, V slice right after).
    fn layer_off(&self, l: usize, pages: usize) -> usize {
        l * 2 * pages * self.page_bytes
    }

    /// Spill up to `want_pages` of `seq`'s coldest resident pages (the oldest,
    /// right after the already-spilled prefix; the hot tail stays resident).
    /// Returns the number of pages actually spilled.
    pub fn spill(
        &mut self,
        kv: &mut KvCache,
        seq: &mut SeqKv,
        want_pages: usize,
        stream: &Stream,
    ) -> Result<usize> {
        let frontier = seq.resident_frontier();
        let cold_end = seq.pages.len().saturating_sub(HOT_TAIL_PAGES);
        if cold_end <= frontier || want_pages == 0 {
            return Ok(0);
        }
        // Round the request up to whole chunks, capped by what is spillable.
        let avail = cold_end - frontier;
        let take = want_pages
            .div_ceil(self.chunk_pages)
            .saturating_mul(self.chunk_pages)
            .min(avail);
        let mut done = 0usize;
        let mut first = frontier;
        while done < take {
            let n = self.chunk_pages.min(cold_end - first);
            self.spill_chunk(kv, seq, first, n, stream)?;
            first += n;
            done += n;
        }
        Ok(done)
    }

    fn spill_chunk(
        &mut self,
        kv: &mut KvCache,
        seq: &mut SeqKv,
        first: usize,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let pb = self.page_bytes;
        let bytes = self.n_layers * 2 * n * pb;
        let pinned = self
            .device
            .alloc(bytes, MemKind::PinnedHost, Pool::Activations)?;
        let t0 = Instant::now();
        for l in 0..self.n_layers {
            let base = self.layer_off(l, n);
            for i in 0..n {
                let phys = seq.pages[first + i];
                if phys < 0 {
                    return Err(ForgeError::Scheduler(
                        "kv tier spill hit an already-spilled page".into(),
                    ));
                }
                let phys = phys as usize;
                self.device
                    .copy(&kv.k[l], phys * pb, &pinned, base + i * pb, pb, stream)?;
                self.device
                    .copy(&kv.v[l], phys * pb, &pinned, base + (n + i) * pb, pb, stream)?;
            }
        }
        stream.synchronize()?;
        ema(&self.d2h_bps, bytes as f64 / t0.elapsed().as_secs_f64(), DEFAULT_D2H_BPS);
        self.spilled_bytes.set(self.spilled_bytes.get() + bytes as u64);
        for i in 0..n {
            kv.push_free(seq.pages[first + i]);
            seq.pages[first + i] = -1;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.chunks.insert(
            id,
            Chunk {
                pages: n,
                bytes,
                loc: ChunkLoc::Ram(pinned),
            },
        );
        self.ram_order.push(id);
        self.ram_used += bytes;
        seq.spilled.push(SpilledRange {
            first_page: first,
            n_pages: n,
            chunk: id,
        });
        tracing::debug!(
            "kv tier spill: seq {} pages {}..{} ({:.1} MiB) d2h {:.1} GB/s",
            seq.id,
            first,
            first + n,
            bytes as f64 / (1 << 20) as f64,
            self.d2h_bps.get() / 1e9
        );
        self.enforce_ram_budget()
    }

    /// Demote warm chunks to the NVMe file (FIFO) until under the RAM budget.
    /// In `Ram` mode exhaustion is a hard error — there is nowhere to demote.
    fn enforce_ram_budget(&mut self) -> Result<()> {
        while self.ram_used > self.cfg.ram_budget_bytes {
            let Some(&id) = self.ram_order.first() else {
                break;
            };
            if self.cfg.mode == KvTierMode::Ram {
                return Err(ForgeError::Scheduler(format!(
                    "kv tier RAM budget exhausted ({:.1} GiB used); raise --kv-tier-ram-gb \
                     or switch to --kv-tier nvme",
                    self.ram_used as f64 / (1u64 << 30) as f64
                )));
            }
            self.ram_order.remove(0);
            let Some(chunk) = self.chunks.get_mut(&id) else {
                continue;
            };
            let ChunkLoc::Ram(buf) = &chunk.loc else {
                continue;
            };
            let host = buf.host_ptr().expect("pinned buffer has host mapping");
            let slice = unsafe { std::slice::from_raw_parts(host, chunk.bytes) };
            let file = self.file.as_mut().expect("nvme mode has a spill file");
            let t0 = Instant::now();
            file.write_all(slice).map_err(|e| {
                ForgeError::Scheduler(format!("kv tier file write failed: {e}"))
            })?;
            ema(
                &self.disk_write_bps,
                chunk.bytes as f64 / t0.elapsed().as_secs_f64(),
                DEFAULT_DISK_BPS,
            );
            let offset = self.file_len;
            self.file_len += chunk.bytes as u64;
            self.ram_used -= chunk.bytes;
            chunk.loc = ChunkLoc::File { offset };
            tracing::debug!(
                "kv tier demote: chunk {id} → file @{offset} ({:.1} MiB, write {:.1} GB/s)",
                chunk.bytes as f64 / (1 << 20) as f64,
                self.disk_write_bps.get() / 1e9
            );
        }
        Ok(())
    }

    /// Estimated transfer cost of restoring every spilled chunk of `seq`:
    /// (total bytes, seconds).
    pub fn restore_cost(&self, seq: &SeqKv) -> (u64, f64) {
        let mut ram_bytes = 0u64;
        let mut file_bytes = 0u64;
        for r in &seq.spilled {
            if let Some(c) = self.chunks.get(&r.chunk) {
                match c.loc {
                    ChunkLoc::Ram(_) => ram_bytes += c.bytes as u64,
                    ChunkLoc::File { .. } => file_bytes += c.bytes as u64,
                }
            }
        }
        let secs = ram_bytes as f64 / self.h2d_bps.get()
            + file_bytes as f64 * (1.0 / self.disk_read_bps.get() + 1.0 / self.h2d_bps.get());
        (ram_bytes + file_bytes, secs)
    }

    /// Estimated cost of recomputing `tokens` of history by re-prefilling.
    pub fn recompute_cost(&self, tokens: usize) -> f64 {
        tokens as f64 / self.prefill_tps.get()
    }

    /// Feed a measured prefill rate into the recompute estimate.
    pub fn note_prefill(&self, tokens: usize, secs: f64) {
        ema(&self.prefill_tps, tokens as f64 / secs.max(1e-9), DEFAULT_PREFILL_TPS);
    }

    /// Restore every spilled chunk of `seq` into freshly allocated VRAM pages.
    /// The caller has verified the free-page count covers the spilled pages.
    pub fn restore_all(
        &mut self,
        kv: &mut KvCache,
        seq: &mut SeqKv,
        stream: &Stream,
    ) -> Result<()> {
        let pb = self.page_bytes;
        let t0 = Instant::now();
        let mut total = 0usize;
        // Buffers referenced by in-flight async copies must outlive the sync.
        let mut retained: Vec<DevBuffer> = Vec::new();
        let ranges: Vec<SpilledRange> = seq.spilled.drain(..).collect();
        for r in ranges {
            let chunk = self.chunks.remove(&r.chunk).ok_or_else(|| {
                ForgeError::Scheduler("kv tier chunk missing during restore".into())
            })?;
            let n = chunk.pages;
            total += chunk.bytes;
            let (src, cold) = match chunk.loc {
                ChunkLoc::Ram(buf) => {
                    self.ram_used -= chunk.bytes;
                    self.ram_order.retain(|&id| id != r.chunk);
                    (buf, false)
                }
                ChunkLoc::File { offset } => {
                    let scratch = self.ensure_scratch(chunk.bytes)?;
                    let host = scratch.host_ptr().expect("pinned mapping");
                    let dst = unsafe { std::slice::from_raw_parts_mut(host, chunk.bytes) };
                    let tr = Instant::now();
                    read_exact_at(self.file.as_ref().expect("nvme file"), dst, offset)?;
                    ema(
                        &self.disk_read_bps,
                        chunk.bytes as f64 / tr.elapsed().as_secs_f64(),
                        DEFAULT_DISK_BPS,
                    );
                    (scratch, true)
                }
            };
            for i in 0..n {
                let phys = kv.pop_free().ok_or_else(|| {
                    ForgeError::Scheduler("kv tier restore ran out of free pages".into())
                })?;
                seq.pages[r.first_page + i] = phys;
            }
            for l in 0..self.n_layers {
                let base = self.layer_off(l, n);
                for i in 0..n {
                    let phys = seq.pages[r.first_page + i] as usize;
                    self.device
                        .copy(&src, base + i * pb, &kv.k[l], phys * pb, pb, stream)?;
                    self.device
                        .copy(&src, base + (n + i) * pb, &kv.v[l], phys * pb, pb, stream)?;
                }
            }
            if cold {
                // The scratch buffer is reused by the next cold chunk; drain
                // its in-flight H2D copies first.
                stream.synchronize()?;
            } else {
                retained.push(src);
            }
        }
        stream.synchronize()?;
        drop(retained);
        ema(&self.h2d_bps, total as f64 / t0.elapsed().as_secs_f64(), DEFAULT_H2D_BPS);
        self.restored_bytes.set(self.restored_bytes.get() + total as u64);
        tracing::info!(
            "kv tier restore: seq {} {:.1} MiB in {:.1} ms ({:.1} GB/s)",
            seq.id,
            total as f64 / (1 << 20) as f64,
            t0.elapsed().as_secs_f64() * 1e3,
            total as f64 / t0.elapsed().as_secs_f64() / 1e9
        );
        Ok(())
    }

    /// Size the pinned scratch for the streamed path: one layer's cold bytes
    /// must fit at once. Call before a streamed step / prefill chunk.
    pub fn prepare_streaming(&mut self, seq: &SeqKv) -> Result<()> {
        let mut cold_layer_bytes = 0usize;
        for r in &seq.spilled {
            if let Some(c) = self.chunks.get(&r.chunk) {
                if matches!(c.loc, ChunkLoc::File { .. }) {
                    cold_layer_bytes += 2 * c.pages * self.page_bytes;
                }
            }
        }
        if cold_layer_bytes > 0 {
            self.ensure_scratch(cold_layer_bytes)?;
        }
        Ok(())
    }

    fn ensure_scratch(&mut self, bytes: usize) -> Result<DevBuffer> {
        if self.scratch.as_ref().is_none_or(|s| s.len() < bytes) {
            self.scratch = Some(self.device.alloc(
                bytes,
                MemKind::PinnedHost,
                Pool::Activations,
            )?);
        }
        Ok(self.scratch.as_ref().expect("allocated above").clone())
    }

    /// Enqueue the copies that materialize layer `l`'s full-context K/V for
    /// `seq` in the staging slabs (staging page index == logical page index):
    /// spilled chunks stream in from RAM/file, resident pages copy D2D from
    /// the paged slabs. Returns true when cold-file bytes were staged through
    /// the pinned scratch — the caller must synchronize the device before the
    /// NEXT layer's staging overwrites the scratch.
    #[allow(clippy::too_many_arguments)]
    pub fn stage_layer(
        &self,
        kv: &KvCache,
        seq: &SeqKv,
        l: usize,
        stage_k: &DevBuffer,
        stage_v: &DevBuffer,
        stream: &Stream,
    ) -> Result<bool> {
        let pb = self.page_bytes;
        let mut any_cold = false;
        let mut cursor = 0usize;
        let mut bytes = 0usize;
        for r in &seq.spilled {
            let chunk = self.chunks.get(&r.chunk).ok_or_else(|| {
                ForgeError::Scheduler("kv tier chunk missing during streaming".into())
            })?;
            let n = chunk.pages;
            let region = n * pb;
            let base = self.layer_off(l, n);
            bytes += 2 * region;
            match &chunk.loc {
                ChunkLoc::Ram(buf) => {
                    self.device
                        .copy(buf, base, stage_k, r.first_page * pb, region, stream)?;
                    self.device
                        .copy(buf, base + region, stage_v, r.first_page * pb, region, stream)?;
                }
                ChunkLoc::File { offset } => {
                    any_cold = true;
                    let scratch = self.scratch.as_ref().ok_or_else(|| {
                        ForgeError::Scheduler("kv tier scratch missing (prepare_streaming)".into())
                    })?;
                    let host = scratch.host_ptr().expect("pinned mapping");
                    let dst = unsafe {
                        std::slice::from_raw_parts_mut(host.add(cursor), 2 * region)
                    };
                    let tr = Instant::now();
                    read_exact_at(
                        self.file.as_ref().expect("nvme file"),
                        dst,
                        offset + base as u64,
                    )?;
                    ema(
                        &self.disk_read_bps,
                        (2 * region) as f64 / tr.elapsed().as_secs_f64(),
                        DEFAULT_DISK_BPS,
                    );
                    self.device
                        .copy(scratch, cursor, stage_k, r.first_page * pb, region, stream)?;
                    self.device.copy(
                        scratch,
                        cursor + region,
                        stage_v,
                        r.first_page * pb,
                        region,
                        stream,
                    )?;
                    cursor += 2 * region;
                }
            }
        }
        // Resident pages (hot tail + anything not yet spilled) copy D2D so the
        // staging slab holds the whole context for this layer.
        for logical in seq.resident_frontier()..seq.pages.len() {
            let phys = seq.pages[logical];
            if phys < 0 {
                return Err(ForgeError::Scheduler(
                    "kv tier: non-prefix spilled page during streaming".into(),
                ));
            }
            let phys = phys as usize;
            self.device
                .copy(&kv.k[l], phys * pb, stage_k, logical * pb, pb, stream)?;
            self.device
                .copy(&kv.v[l], phys * pb, stage_v, logical * pb, pb, stream)?;
        }
        self.streamed_bytes.set(self.streamed_bytes.get() + bytes as u64);
        Ok(any_cold)
    }

    /// Drop every chunk belonging to `seq` (release / recompute). File space
    /// is not reclaimed (append-only log; the file is deleted on drop).
    pub fn drop_seq(&mut self, seq: &mut SeqKv) {
        for r in seq.spilled.drain(..) {
            if let Some(c) = self.chunks.remove(&r.chunk) {
                if matches!(c.loc, ChunkLoc::Ram(_)) {
                    self.ram_used -= c.bytes;
                    self.ram_order.retain(|&id| id != r.chunk);
                }
            }
        }
    }
}

impl Drop for TierManager {
    fn drop(&mut self) {
        if let Some(path) = &self.file_path {
            let _ = std::fs::remove_file(path);
        }
        if let Some(dir) = &self.default_dir {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Positioned read that fills `buf` from `offset` (unix pread; the spill file
/// is only used on Linux/macOS servers).
fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
        .map_err(|e| ForgeError::Scheduler(format!("kv tier file read failed: {e}")))
}
