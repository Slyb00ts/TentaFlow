// ===== File: fp8.rs — the second form of a weight, built for prompt width =====
//
// A quantized weight has one form on disk and the kernels read it directly.
// That form is the right one for decode, where a single row reads the whole
// matrix and the cost is the bytes moved. It is the WRONG one for a prompt,
// where hundreds of rows share the matrix and the cost is arithmetic: the block
// formats decode their superblocks inside the inner loop, and no amount of
// tiling makes that free.
//
// So a projection that gets multiplied at prompt width acquires a SECOND form —
// e4m3 bytes with one scale per output row — and keeps both. Which one runs is
// decided by the width of the step, not by a mode: decode reads the source
// blocks, a prompt reads the pack.
//
// Built on FIRST WIDE USE rather than at load, and that is the point of putting
// it here. The executor does not know what a weight is FOR — `put_quant` sees
// rows, columns and a format — so packing at load would mean packing everything,
// including expert stacks whose prefill goes through address-indexed kernels
// that have no dense GEMM at all. Waiting until a weight is actually multiplied
// wide answers the question exactly, with no role table to keep in step.

use std::collections::hash_map::Entry;

use super::{CudaExec, Quantized};

use forge_graph::WeightId;
use forge_hal::{DevBuffer, Pool};
use forge_types::{MemKind, QuantKind, Result};

/// The e4m3 form of one projection.
///
/// One byte per weight and one f32 per row, against roughly half a byte per
/// weight for Q4_K — so this rather more than doubles what the projection
/// occupies. Held ALONGSIDE the source blocks and not instead of them: decode
/// is faster on the source, and the logit head at one row would lose accuracy
/// for nothing.
pub(super) struct Fp8Pack {
    codes: DevBuffer,
    scales: DevBuffer,
}

/// Rows below which a step is a decode batch and not a prompt.
///
/// The int8 batch forms know widths 2, 4, 8 and 16 and are built for exactly
/// that shape; above them there is no dedicated kernel and the tile takes over,
/// which is where the pack is worth its memory.
const DP4A_BATCH_ROWS: u32 = 16;

impl CudaExec {
    /// The pack for one weight, made on demand.
    ///
    /// A refusal is REMEMBERED as such: a format with no packer, or a card with
    /// no room left, must not be re-attempted on every layer of every step.
    fn fp8_pack(&self, id: WeightId, w: &Quantized) -> Result<bool> {
        let mut held = self.fp8.borrow_mut();
        let slot = match held.entry(id.0) {
            Entry::Occupied(e) => return Ok(e.get().is_some()),
            Entry::Vacant(e) => e,
        };
        // Only the block formats the packer reads, and only widths its GEMM
        // addresses. Anything else keeps the source path, permanently.
        let packable = matches!(
            w.quant,
            QuantKind::Q4K | QuantKind::Q6K | QuantKind::Q8_0 | QuantKind::NVFP4Gguf
        ) && w.cols.is_multiple_of(64);
        if !packable {
            slot.insert(None);
            return Ok(false);
        }
        // The weights pool is claimed once at construction, so a model large
        // enough to fill it simply does not get the second form. That is a
        // slower prompt, not a failure — and recording the refusal means the
        // allocator is asked once rather than once per step.
        let Ok(codes) = self
            .device
            .alloc(w.rows * w.cols, MemKind::Device, Pool::Weights)
        else {
            slot.insert(None);
            return Ok(false);
        };
        let Ok(scales) = self.device.alloc(w.rows * 4, MemKind::Device, Pool::Weights) else {
            slot.insert(None);
            return Ok(false);
        };
        self.kernels.pack_gguf_fp8(
            &codes,
            &scales,
            &w.blocks,
            0,
            w.rows,
            w.cols,
            w.quant,
            w.output_scale,
            &self.stream,
        )?;
        slot.insert(Some(Fp8Pack { codes, scales }));
        Ok(true)
    }

    /// Multiply through the e4m3 form, if this weight and this width have one.
    ///
    /// Answers whether it ran, so the caller keeps its own dispatch: this is a
    /// faster route for the same product, not a different operation.
    pub(super) fn fp8_matmul(
        &self,
        id: WeightId,
        w: &Quantized,
        y: &DevBuffer,
        x: &DevBuffer,
        rows: u32,
    ) -> Result<bool> {
        if rows <= DP4A_BATCH_ROWS {
            return Ok(false);
        }
        // Ask for the shape's kernels BEFORE packing. A projection whose shape
        // no fp8 GEMM covers would otherwise pay for a pack it can never use.
        let modular = self
            .kernels
            .has_artifact(&format!("gemm_fp8_mod_{}_{}", w.rows, w.cols));
        if !modular && !self.kernels.has_artifact("gemm_fp8_f16") {
            return Ok(false);
        }
        if !self.fp8_pack(id, w)? {
            return Ok(false);
        }
        let held = self.fp8.borrow();
        let pack = held
            .get(&id.0)
            .and_then(Option::as_ref)
            .expect("paczka właśnie potwierdzona");
        let n = rows as usize;
        // The multistage kernel is compiled for one (rows, cols) and is the
        // faster of the two where it exists; the tiled one covers every shape.
        if modular {
            self.kernels.gemm_fp8_modular(
                y,
                &pack.codes,
                &pack.scales,
                x,
                w.rows,
                w.cols,
                n,
                &self.stream,
            )?;
        } else {
            self.kernels.gemm_fp8(
                y,
                &pack.codes,
                &pack.scales,
                x,
                w.rows,
                w.cols,
                n,
                &self.stream,
            )?;
        }
        Ok(true)
    }
}
