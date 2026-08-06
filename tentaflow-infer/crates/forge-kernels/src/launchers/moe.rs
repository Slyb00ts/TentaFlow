// ===== File: moe.rs — launchery MoE: router, bramka, redukcja ekspertow =====
use super::*;

impl Kernels {
    /// Bramka MoE DeepSeeka V4. Bias wchodzi WYŁĄCZNIE do rankingu top-k; wagi
    /// biorą się z wyniku bez niego, są normalizowane do sumy 1 i mnożone przez
    /// `route_scale`.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_gate_sqrtsoftplus_f16(
        &self,
        ids: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        gate_w: &DevBuffer,
        bias: &DevBuffer,
        n_tokens: usize,
        hidden: usize,
        n_expert: usize,
        top_k: usize,
        route_scale: f32,
        stream: &Stream,
    ) -> Result<()> {
        if n_expert > 256 {
            return Err(ForgeError::Kernel(format!(
                "moe_gate: {n_expert} ekspertów przekracza limit kernela 256"
            )));
        }
        let k = self.artifacts.get("moe_gate_sqrtsoftplus_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(ids)
            .buf(weights)
            .buf(x)
            .buf(gate_w)
            .buf(bias)
            .scalar(hidden as i64)
            .scalar(n_expert as i64)
            .scalar(top_k as i64)
            .scalar(route_scale);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// MoE router: for each of `n_tokens` rows of `x` (f16, [n_tokens, hidden])
    /// compute logits `x · gate_inp` over `n_expert` experts (f16 router,
    /// [n_expert, hidden]), softmax over all experts, then select the top-k.
    /// Writes `ids` ([n_tokens, top_k] i32) and `weights` ([n_tokens, top_k]
    /// f32). `norm_topk` renormalizes the selected weights to sum 1.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_router_f16(
        &self,
        ids: &DevBuffer,
        weights: &DevBuffer,
        x: &DevBuffer,
        gate_inp: &DevBuffer,
        counts: &DevBuffer,
        n_tokens: usize,
        hidden: usize,
        n_expert: usize,
        top_k: usize,
        norm_topk: bool,
        stream: &Stream,
    ) -> Result<()> {
        // Shared-memory staging caps (mirror MOE_MAX_* in moe.mojo).
        if hidden > 8192 {
            return Err(ForgeError::Kernel(format!(
                "moe_router: hidden {hidden} exceeds kernel cap 8192"
            )));
        }
        if n_expert > 256 {
            return Err(ForgeError::Kernel(format!(
                "moe_router: n_expert {n_expert} exceeds kernel cap 256"
            )));
        }
        let k = self.artifacts.get("moe_router_f16")?;
        let cfg = LaunchConfig {
            grid: (n_tokens as u32, 1, 1),
            block: (BLOCK, 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(ids)
            .buf(weights)
            .buf(x)
            .buf(gate_inp)
            .buf(counts)
            .scalar(hidden as i64)
            .scalar(n_expert as i64)
            .scalar(top_k as i64)
            .scalar(norm_topk as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Fold one routed expert's f16 output into a token's FFN accumulator:
    /// `acc += scale * src` over `n` elements (or `acc = scale * src` when
    /// `init`). Both buffers are addressed by byte offset so a per-token row of
    /// a batched accumulator can be targeted.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_scale_add_f16(
        &self,
        acc: &DevBuffer,
        acc_off: usize,
        src: &DevBuffer,
        src_off: usize,
        n: usize,
        scale: f32,
        init: bool,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_scale_add_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf_at(acc, acc_off)?
            .buf_at(src, src_off)?
            .scalar(n as i64)
            .scalar(scale)
            .scalar(init as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// Like `moe_scale_add_f16` but the router weight is read ON DEVICE from
    /// `weights[sel]`, so no host readback of the routing weights is needed.
    /// For the shared expert, pass its device-resident sigmoid gate scale as
    /// `weights` with `sel = 0`.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_scale_add_gidx_f16(
        &self,
        acc: &DevBuffer,
        acc_off: usize,
        src: &DevBuffer,
        src_off: usize,
        n: usize,
        weights: &DevBuffer,
        sel: usize,
        init: bool,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_scale_add_gidx_f16")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new()
            .buf_at(acc, acc_off)?
            .buf_at(src, src_off)?
            .scalar(n as i64)
            .buf(weights)
            .scalar(sel as i64)
            .scalar(init as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `out[i] = sigmoid(in[i])` over `n` shared-expert gate logits: turns them
    /// (f16, from the gate projection) into device-resident f32 scales, so
    /// folding the shared expert costs no per-layer host round-trip. One logit
    /// per token, and the whole step at once when its projection ran as one
    /// matrix.
    pub fn moe_sigmoid_f16_to_f32(
        &self,
        out: &DevBuffer,
        input: &DevBuffer,
        n: usize,
        stream: &Stream,
    ) -> Result<()> {
        let k = self.artifacts.get("moe_sigmoid_f16_to_f32")?;
        let cfg = LaunchConfig::linear(n as u32, BLOCK);
        let args = LaunchArgs::new().buf(out).buf(input).scalar(n as i64);
        self.device.launch(k, &cfg, &args, stream)
    }

    /// `out[t] = Σ_j weights[t·top_k+j] · src[slots[t·top_k+j]]`, one block per
    /// token: the inverse of the gather that groups a step's selections by
    /// expert.
    ///
    /// The sum walks `j` in the order the router chose, as the per-token route
    /// did, but keeps it in f32 across all `top_k` and rounds to f16 once —
    /// where folding expert by expert rounded after each one. Toward the f32
    /// reference, not away from it.
    #[allow(clippy::too_many_arguments)]
    pub fn moe_combine_f16(
        &self,
        out: &DevBuffer,
        src: &DevBuffer,
        slots: &DevBuffer,
        weights: &DevBuffer,
        tokens: usize,
        cols: usize,
        top_k: usize,
        init: bool,
        stream: &Stream,
    ) -> Result<()> {
        let selections = checked_buffer_bytes("moe_combine selections", &[tokens, top_k], 4)?;
        let out_bytes = checked_buffer_bytes("moe_combine output", &[tokens, cols], 2)?;
        if tokens == 0
            || cols == 0
            || top_k == 0
            || out.len() < out_bytes
            || slots.len() < selections
            || weights.len() < selections
        {
            return Err(ForgeError::Kernel(
                "moe_combine_f16: nieprawidłowy kształt lub zbyt mały bufor".into(),
            ));
        }
        let k = self.artifacts.get("moe_combine_f16")?;
        let cfg = LaunchConfig {
            grid: (tokens as u32, 1, 1),
            block: (BLOCK.min(cols as u32).max(32), 1, 1),
            shared_mem_bytes: 0,
        };
        let args = LaunchArgs::new()
            .buf(out)
            .buf(src)
            .buf(slots)
            .buf(weights)
            .scalar(cols as i64)
            .scalar(top_k as i64)
            .scalar(init as i64);
        self.device.launch(k, &cfg, &args, stream)
    }
}
