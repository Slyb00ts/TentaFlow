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
    /// Slot table for the combine, i32 [top_k]. Every selection already sits at
    /// its own row in the router's order, so this is the identity — the combine
    /// takes a table because a path that reorders rows has one to invert.
    pub(crate) identity: DevBuffer,
    /// Pinned-host landing for the shared-expert gate logit (f16), read back in
    /// the same sync as the router top-k (fallback readback path only).
    pub(crate) pinned_shared: DevBuffer,
    /// Device-resident shared-expert sigmoid gate scale (f32, one element). The
    /// device dispatch path computes it on-GPU so folding the shared expert
    /// needs no host round-trip.
    pub(crate) shared_scale: DevBuffer,
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
            identity: {
                let slots = dev(top_k * 4)?;
                let rows: Vec<u8> = (0..top_k as i32).flat_map(|j| j.to_le_bytes()).collect();
                device.write(&rows, &slots, 0)?;
                slots
            },
            pinned_shared: pinned(2)?,
            shared_scale: {
                // Seed 1.0 so a shared expert without a per-token gate (no
                // shared_gate) folds in unscaled; the device sigmoid kernel
                // overwrites this each layer when a gate exists.
                let sc = dev(4)?;
                device.write(&1.0f32.to_le_bytes(), &sc, 0)?;
                sc
            },
        })
    }
}
