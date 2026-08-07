// ===== File: control.rs — the small i32 data a step steers itself with =====
//
// Ids, positions, KV bases, the page table and the context lengths. Tiny next
// to any activation, and none of it is arithmetic — but all of it has to reach
// the device BEFORE the kernels that read it and WITHOUT stopping the ones
// already queued, which is the whole reason it lives in one place.

use std::cell::Cell;

use forge_graph::Step;
use forge_hal::{DevBuffer, Device, Event};
use forge_types::{ForgeError, Result};

use super::CudaExec;

/// One control buffer's pinned host mirror and the fence behind its last copy.
pub(super) struct Staging {
    host: DevBuffer,
    copied: Event,
    /// False until the first copy is recorded — an event that was never
    /// recorded cannot be waited on.
    live: Cell<bool>,
}

impl Staging {
    pub(super) fn new(device: &dyn Device, i32s: usize) -> Result<Self> {
        Ok(Self {
            host: device.alloc(
                i32s * 4,
                forge_types::MemKind::PinnedHost,
                forge_hal::Pool::Activations,
            )?,
            copied: device.create_event()?,
            live: Cell::new(false),
        })
    }
}

/// Which staged buffer a mirror belongs to.
pub(super) const STAGE_IDS: usize = 0;
pub(super) const STAGE_POSITIONS: usize = 1;
pub(super) const STAGE_BASES: usize = 2;
pub(super) const STAGE_PAGES: usize = 3;
pub(super) const STAGE_LENGTHS: usize = 4;

impl CudaExec {
    /// Stages the small i32 control data — ids, positions, page tables — a step
    /// needs.
    ///
    /// A HAL write lands on the legacy stream while this executor's stream is
    /// non-blocking, so it is NOT ordered against work already queued: a write
    /// issued now can overwrite bytes a queued kernel has not read yet. Draining
    /// the stream first made that safe and cost a full pipeline refill four
    /// times per decode step. A copy FROM PINNED MEMORY ON THIS STREAM is
    /// ordered against those kernels by construction, so nothing has to drain —
    /// and it is a graph node rather than a host call, which is what lets a
    /// step be captured at all.
    ///
    /// The mirror stays: the page table alone is read by eighty kernels a step
    /// and moves for none of them.
    pub(super) fn stage_i32(&self, dst: &DevBuffer, mirror: usize, values: Vec<i32>) -> Result<()> {
        let held = &self.staged[mirror];
        if *held.borrow() == values {
            return Ok(());
        }
        let stage = &self.stage_host[mirror];
        // The pinned buffer is the SOURCE of a copy that may still be queued,
        // so it may not be rewritten until that copy has read it. In practice
        // the fence is long past — a step reads its logits back before the next
        // one is built — but the wait is what makes that a fact rather than an
        // assumption about the caller.
        if stage.live.get() {
            stage.copied.synchronize()?;
        }
        let bytes = std::mem::size_of_val(&values[..]);
        let ptr = stage.host.host_ptr().ok_or_else(|| {
            ForgeError::Other("bufor sztaplowania nie jest pamięcią hosta".into())
        })?;
        // SAFETY: `ptr` owns `MAX_CONTROL_I32 * 4` pinned bytes and `bytes` is
        // bounded by that allocation; the regions cannot overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(values.as_ptr() as *const u8, ptr, bytes);
        }
        self.device
            .copy(&stage.host, 0, dst, 0, bytes, &self.stream)?;
        self.device.record_event(&stage.copied, &self.stream)?;
        stage.live.set(true);
        *held.borrow_mut() = values;
        Ok(())
    }

    /// Gives every lane of this step the pages its context needs, and stages
    /// the table the kernels read.
    ///
    /// A lane starting at position zero is a sequence starting over, so it
    /// gives back whatever it no longer needs. Inferred from the position
    /// rather than announced, because the vocabulary has no verb for "forget
    /// this sequence" and inventing one for a case the data already states
    /// would be a wider contract bought for nothing.
    pub(super) fn map_pages(&self, step: &Step) -> Result<()> {
        let mut kv = self.kv.borrow_mut();
        let mut seqs = self.seqs.borrow_mut();
        let per_lane = kv.cfg.max_pages_per_seq;
        for lane in step.lanes() {
            let seq = &mut seqs[lane.slot as usize];
            let target = (lane.pos + step.tokens()) as usize;
            // IDEMPOTENTNIE, bo to woła się raz na warstwę — czterdzieści razy
            // na krok. `KvCache::grow` liczy tokeny przyrostowo, więc pętla po
            // `tokens` rosłaby czterdziestokrotnie i sekwencja odjechałaby od
            // pozycji, którą model jej przypisał.
            if seq.len == target {
                continue;
            }
            // Pozycja zero to sekwencja zaczynająca się od nowa, więc oddaje
            // strony, których nowa długość nie potrzebuje. Wywnioskowane z
            // danych, bo słownictwo nie ma czasownika „zapomnij tę sekwencję",
            // a wymyślanie go dla przypadku, który dane już mówią, byłoby
            // szerszym kontraktem kupionym za nic.
            if lane.pos == 0 {
                kv.rollback(seq, 0);
            } else if seq.len != lane.pos as usize {
                return Err(ForgeError::Other(format!(
                    "slot {} stoi na {} tokenach, a krok wznawia od {}",
                    lane.slot, seq.len, lane.pos
                )));
            }
            while seq.len < target {
                kv.grow(seq)?;
            }
        }
        // Tablica idzie w KOLEJNOŚCI LANE'ÓW kroku, nie slotów: kernele
        // adresują ją tym samym indeksem, którym adresują wiersze aktywacji.
        let mut table = vec![0i32; step.lanes().len() * per_lane];
        for (i, lane) in step.lanes().iter().enumerate() {
            let held = &seqs[lane.slot as usize].pages;
            table[i * per_lane..i * per_lane + held.len()].copy_from_slice(held);
        }
        drop(seqs);
        drop(kv);
        self.stage_i32(&self.scratch.pages, STAGE_PAGES, table)
    }

    /// Pages one lane holds, as a table of its own — that is what the
    /// single-sequence prefill kernel takes.
    pub(super) fn lane_pages(&self, lane: usize) -> Result<DevBuffer> {
        let per_lane = self.kv.borrow().cfg.max_pages_per_seq;
        self.device
            .sub_buffer(&self.scratch.pages, lane * per_lane * 4, per_lane * 4)
    }
}
