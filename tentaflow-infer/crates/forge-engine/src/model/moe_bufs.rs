// ===== File: moe_bufs.rs — scratch mieszanki ekspertów =====
use forge_formats::MoeParams;

use super::*;

/// MoE scratch (allocated only for Mixture-of-Experts models). The router
/// output is sized for a full prefill chunk; decode uses the first row.
pub(crate) struct MoeBufs {
    /// Selected expert ids, i32 [MAX_PREFILL_CHUNK * top_k].
    pub(crate) ids: DevBuffer,
    /// Router logits, f32 [MAX_PREFILL_CHUNK * n_experts]. The projection that
    /// fills it is an ordinary multiply and runs as one, so the selection that
    /// follows is the only part that is genuinely one block per token.
    pub(crate) logits: DevBuffer,
    /// Routing weights, f32 [MAX_PREFILL_CHUNK * top_k].
    pub(crate) weights: DevBuffer,
    pub(crate) pinned_ids: DevBuffer,
    pub(crate) pinned_weights: DevBuffer,
    /// One token's FFN-normed hidden, f16 [hidden] — prefill copies a row here
    /// so the per-expert GEMV reads a contiguous single-token activation.
    pub(crate) xrow: DevBuffer,
    /// One expert's down-projection output, f16 [hidden].
    pub(crate) tmp: DevBuffer,
    /// One decode token's selections, each at its own row: the gate half (which
    /// the gate function overwrites in place), the up half, and the down
    /// output, f16 [top_k * moe_inter] / [top_k * hidden].
    pub(crate) sel_gate: DevBuffer,
    pub(crate) sel_up: DevBuffer,
    pub(crate) sel_out: DevBuffer,
    /// Slot table for the combine, i32 [MAX_PREFILL_CHUNK]. Every selection of
    /// a decode step already sits at its own row in the router's order, so its
    /// first `top_k` entries are what that combine needs; the shared expert of
    /// a prefill chunk reads one entry per token from the same table. The
    /// combine takes a table because a path that reorders rows has one to
    /// invert.
    pub(crate) identity: DevBuffer,
    /// A prefill chunk's selections gathered into expert order: the activation
    /// each selection reads, the two feed-forward halves and the down output.
    /// f16 [chunk * top_k * hidden] / [chunk * top_k * moe_inter].
    pub(crate) grouped_x: DevBuffer,
    pub(crate) grouped_gate: DevBuffer,
    pub(crate) grouped_up: DevBuffer,
    pub(crate) grouped_out: DevBuffer,
    /// `order[p]` is the token whose row the gather puts at position `p`;
    /// `slots[sel]` says where that selection landed, which is what puts the
    /// answers back. i32 [chunk * top_k].
    pub(crate) order: DevBuffer,
    pub(crate) slots: DevBuffer,
    /// One entry per tile of the grouped launch: which expert it reads, and
    /// where that expert's block of rows begins and ends. i32, bounded by one
    /// tile per expert plus one per selection.
    pub(crate) tile_expert: DevBuffer,
    pub(crate) tile_first: DevBuffer,
    pub(crate) tile_end: DevBuffer,
    /// Shared-expert gate logits, f16 [MAX_PREFILL_CHUNK] — `ffn_gate_inp_shexp · x`
    /// for every token of the chunk (decode fills the first element only).
    pub(crate) shared_logits: DevBuffer,
    /// Device-resident shared-expert sigmoid gate scales, f32
    /// [MAX_PREFILL_CHUNK]. The device dispatch path reads element 0 straight
    /// from here, so folding the shared expert needs no host round-trip.
    pub(crate) shared_scale: DevBuffer,
    /// Pinned-host landing for `shared_scale`, read back in the same sync as
    /// the router top-k (the host-dispatch paths only).
    pub(crate) pinned_shared: DevBuffer,
}

impl MoeBufs {
    pub(crate) fn new(device: &dyn Device, moe: &MoeParams, hidden: usize) -> Result<Self> {
        let top_k = moe.n_experts_used;
        let idw = MAX_PREFILL_CHUNK * top_k;
        let dev = |bytes: usize| device.alloc(bytes, MemKind::Device, Pool::Activations);
        let pinned = |bytes: usize| device.alloc(bytes, MemKind::PinnedHost, Pool::Activations);
        let sel_ffn = top_k * moe.moe_intermediate_size * 2;
        Ok(Self {
            ids: dev(idw * 4)?,
            logits: dev(MAX_PREFILL_CHUNK * moe.n_experts * 4)?,
            weights: dev(idw * 4)?,
            pinned_ids: pinned(idw * 4)?,
            pinned_weights: pinned(idw * 4)?,
            xrow: dev(hidden * 2)?,
            tmp: dev(hidden * 2)?,
            sel_gate: dev(sel_ffn)?,
            sel_up: dev(sel_ffn)?,
            sel_out: dev(top_k * hidden * 2)?,
            grouped_x: dev(idw * hidden * 2)?,
            grouped_gate: dev(idw * moe.moe_intermediate_size * 2)?,
            grouped_up: dev(idw * moe.moe_intermediate_size * 2)?,
            grouped_out: dev(idw * hidden * 2)?,
            order: dev(idw * 4)?,
            slots: dev(idw * 4)?,
            tile_expert: dev((moe.n_experts + idw) * 4)?,
            tile_first: dev((moe.n_experts + idw) * 4)?,
            tile_end: dev((moe.n_experts + idw) * 4)?,
            identity: {
                let slots = dev(MAX_PREFILL_CHUNK * 4)?;
                let rows: Vec<u8> = (0..MAX_PREFILL_CHUNK as i32)
                    .flat_map(|j| j.to_le_bytes())
                    .collect();
                device.write(&rows, &slots, 0)?;
                slots
            },
            shared_logits: dev(MAX_PREFILL_CHUNK * 2)?,
            shared_scale: {
                // Seed 1.0 so a shared expert without a per-token gate (no
                // shared_gate) folds in unscaled; the sigmoid kernel overwrites
                // these each layer when a gate exists.
                let sc = dev(MAX_PREFILL_CHUNK * 4)?;
                let ones: Vec<u8> = std::iter::repeat_n(1.0f32.to_le_bytes(), MAX_PREFILL_CHUNK)
                    .flatten()
                    .collect();
                device.write(&ones, &sc, 0)?;
                sc
            },
            pinned_shared: pinned(MAX_PREFILL_CHUNK * 4)?,
        })
    }
}
