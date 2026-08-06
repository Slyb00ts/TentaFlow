// ===== File: delta.rs — the recurrent mixer on CUDA =====
//
// Split out for the same reason the mixture was: it is the one other part with
// a memory model of its own. Nine intermediate buffers that no other operation
// names, and a state that outlives the step — every token folds itself into a
// matrix that the next token reads.
//
// The token loop here is genuinely sequential and not a shortcut. A recurrence
// cannot be widened the way a projection can: token t+1 reads the matrix token
// t wrote. The projections in front of it ARE stateless, so they run for every
// row of the step in one pass, and only the fold is walked one token at a time.

use super::{CudaExec, Quantized};

use forge_graph::{Act, DeltaWeights, SsmShape, Step};
use forge_hal::{DevBuffer, Pool};
use forge_types::{ForgeError, MemKind, Result};

/// Buffers one recurrent layer needs between its two projections.
///
/// Made on first demand and shared by every DeltaNet layer of the model: they
/// hold one step at a time and nothing in them survives the operation, so a set
/// per layer would be forty copies of the same scratch.
#[derive(Clone)]
pub(super) struct DeltaScratch {
    /// Query, key and value of every row, as the in-projection produced them.
    mixed: DevBuffer,
    /// The output gate of every row.
    z: DevBuffer,
    /// Per-head decay and write-gate projections of every row.
    alpha: DevBuffer,
    beta_raw: DevBuffer,
    /// One row's convolved stream, then its pieces.
    conv_out: DevBuffer,
    q16: DevBuffer,
    k16: DevBuffer,
    /// The same, repeated to cover the value heads.
    q32: DevBuffer,
    k32: DevBuffer,
    v: DevBuffer,
    /// Decay and write gate, in f32 because the recurrence is.
    g: DevBuffer,
    beta: DevBuffer,
    /// The recurrence's answer, before and after its gated normalization.
    o: DevBuffer,
    normed: DevBuffer,
    rows: usize,
}

impl CudaExec {
    fn ensure_delta(&self, ssm: SsmShape, rows: usize) -> Result<DeltaScratch> {
        let mut held = self.delta.borrow_mut();
        if let Some(s) = held.as_ref() {
            if s.rows >= rows {
                return Ok(s.clone());
            }
        }
        let f16b = |elems: u32| {
            self.device
                .alloc(elems as usize * 2, MemKind::Device, Pool::Activations)
        };
        let f32b = |elems: u32| {
            self.device
                .alloc(elems as usize * 4, MemKind::Device, Pool::Activations)
        };
        let n = rows as u32;
        let fresh = DeltaScratch {
            mixed: f16b(n * ssm.mixed_width())?,
            z: f16b(n * ssm.value_width())?,
            alpha: f16b(n * ssm.v_heads)?,
            beta_raw: f16b(n * ssm.v_heads)?,
            conv_out: f16b(ssm.mixed_width())?,
            q16: f16b(ssm.key_width())?,
            k16: f16b(ssm.key_width())?,
            q32: f16b(ssm.value_width())?,
            k32: f16b(ssm.value_width())?,
            v: f16b(ssm.value_width())?,
            g: f32b(ssm.v_heads)?,
            beta: f32b(ssm.v_heads)?,
            o: f16b(ssm.value_width())?,
            normed: f16b(ssm.value_width())?,
            rows,
        };
        *held = Some(fresh.clone());
        Ok(fresh)
    }

    /// One projection of every row of the step, straight into a scratch slot.
    ///
    /// Not `matmul`, because that writes an `Act` and these four outputs are
    /// not activations the vocabulary names — they are this operation's
    /// insides. Same kernels and the same format table, though; a recurrent
    /// layer's projections are quantized like every other.
    fn project(&self, w: &Quantized, y: &DevBuffer, x: &DevBuffer, rows: usize) -> Result<()> {
        if rows == 1 {
            return self.gemv_by_kind(w.quant, y, &w.blocks, x, w.rows, w.cols, w.output_scale);
        }
        self.gemm_by_kind(
            w.quant,
            y,
            &w.blocks,
            x,
            w.rows,
            w.cols,
            rows,
            w.output_scale,
        )
    }

    /// A whole recurrent layer, for every row of the step.
    pub(super) fn op_delta_net(
        &self,
        out: Act,
        x: Act,
        layer: usize,
        w: &DeltaWeights,
        step: &Step,
    ) -> Result<()> {
        let ssm = self.ssm.ok_or_else(|| {
            ForgeError::Unsupported(
                "DeltaNet: wykonawca powstał bez geometrii miksera rekurencyjnego".into(),
            )
        })?;
        let rows = step.rows() as usize;
        let hidden = self.shape.hidden as usize;
        let (key, value, mixed) = (
            ssm.key_width() as usize,
            ssm.value_width() as usize,
            ssm.mixed_width() as usize,
        );
        let (v_heads, d_state) = (ssm.v_heads as usize, ssm.d_state as usize);
        if !ssm.v_heads.is_multiple_of(ssm.k_heads) || ssm.k_heads == 0 {
            return Err(ForgeError::Unsupported(format!(
                "{} głowic wartości nie dzieli się na {} głowic klucza",
                ssm.v_heads, ssm.k_heads
            )));
        }
        let rep = v_heads / ssm.k_heads as usize;

        let qkv = self.quant(w.qkv)?;
        let gate = self.quant(w.gate)?;
        let alpha_w = self.quant(w.alpha)?;
        let beta_w = self.quant(w.beta)?;
        let out_w = self.quant(w.out)?;
        for (name, held, want) in [
            ("qkv", qkv.rows, mixed),
            ("bramka", gate.rows, value),
            ("alfa", alpha_w.rows, v_heads),
            ("beta", beta_w.rows, v_heads),
            ("wyjście", out_w.rows, hidden),
        ] {
            if held != want {
                return Err(ForgeError::Unsupported(format!(
                    "DeltaNet: projekcja {name} ma {held} wierszy wobec {want}"
                )));
            }
        }

        let s = self.ensure_delta(ssm, rows)?;
        let conv_w = self.plain(w.conv)?;
        let dt_bias = self.plain(w.dt_bias)?;
        let a = self.plain(w.a)?;
        let norm_w = self.plain(w.norm)?;

        // The four projections are stateless, so every row of the step goes
        // through them in ONE pass over the weights. Only the fold below has to
        // be walked token by token.
        let src = self.buf(x);
        self.project(qkv, &s.mixed, src, rows)?;
        self.project(gate, &s.z, src, rows)?;
        self.project(alpha_w, &s.alpha, src, rows)?;
        self.project(beta_w, &s.beta_raw, src, rows)?;

        let mut state = self.recurrent.borrow_mut();
        let held = state.ensure(&*self.device, layer)?;
        let (conv_buf, state_buf) = (held.conv.clone(), held.state.clone());
        let cfg = state.config();
        let tokens = step.tokens() as usize;
        for (lane, l) in step.lanes().iter().enumerate() {
            let slot = l.slot as usize;
            // A lane starting at position zero is a sequence that starts here,
            // so whatever the previous occupant of this slot folded into the
            // state is not its history. Derived from the step rather than
            // signalled separately, because a separate signal can be forgotten
            // and this cannot.
            if l.pos == 0 {
                state.clear(&*self.device, layer, slot)?;
            }
            let window = self.device.sub_buffer(
                &conv_buf,
                cfg.conv_offset(slot),
                cfg.conv_bytes(),
            )?;
            let matrix = self.device.sub_buffer(
                &state_buf,
                cfg.state_offset(slot),
                cfg.state_bytes(),
            )?;
            for t in 0..tokens {
                let row = lane * tokens + t;
                self.kernels.deltanet_conv_silu_f16_at(
                    &s.conv_out,
                    0,
                    &window,
                    &s.mixed,
                    row * mixed * 2,
                    &conv_w,
                    mixed,
                    ssm.d_conv as usize,
                    &self.stream,
                )?;
                // The convolved stream is query, key and value end to end. The
                // norms read their slice by byte offset; the value needs a copy
                // because the recurrence takes it as its own buffer.
                self.device
                    .copy(&s.conv_out, 2 * key * 2, &s.v, 0, value * 2, &self.stream)?;
                self.kernels.l2norm_heads_f16_at(
                    &s.q16,
                    &s.conv_out,
                    0,
                    ssm.k_heads as usize,
                    d_state,
                    self.shape.eps,
                    &self.stream,
                )?;
                self.kernels.l2norm_heads_f16_at(
                    &s.k16,
                    &s.conv_out,
                    key * 2,
                    ssm.k_heads as usize,
                    d_state,
                    self.shape.eps,
                    &self.stream,
                )?;
                // Value head h reads key head h % k_heads, so the key block is
                // laid out repeated rather than indexed with a stride.
                self.kernels
                    .deltanet_repeat_qk_f16(&s.q32, &s.k32, &s.q16, &s.k16, key, rep, &self.stream)?;
                self.kernels.deltanet_log_decay_f32_at(
                    &s.g,
                    0,
                    &s.alpha,
                    row * v_heads * 2,
                    &dt_bias,
                    &a,
                    v_heads,
                    &self.stream,
                )?;
                self.kernels.deltanet_beta_sigmoid_f32_at(
                    &s.beta,
                    &s.beta_raw,
                    row * v_heads * 2,
                    v_heads,
                    &self.stream,
                )?;
                self.kernels.deltanet_gated_step_f16(
                    &s.o,
                    &matrix,
                    &s.q32,
                    &s.k32,
                    &s.v,
                    &s.g,
                    &s.beta,
                    v_heads,
                    d_state,
                    &self.stream,
                )?;
                self.kernels.deltanet_gated_rmsnorm_f16_at(
                    &s.normed,
                    &s.o,
                    &s.z,
                    row * value * 2,
                    &norm_w,
                    v_heads,
                    d_state,
                    self.shape.eps,
                    &self.stream,
                )?;
                let dst = self
                    .device
                    .sub_buffer(self.buf(out), row * hidden * 2, hidden * 2)?;
                self.project(out_w, &dst, &s.normed, 1)?;
            }
        }
        Ok(())
    }
}
