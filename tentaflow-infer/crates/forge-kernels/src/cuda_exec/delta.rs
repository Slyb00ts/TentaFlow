// ===== File: delta.rs — the recurrent mixer on CUDA =====
//
// Split out for the same reason the mixture was: it is the one other part with
// a memory model of its own. Nine intermediate buffers that no other operation
// names, and a state that outlives the step — every token folds itself into a
// matrix that the next token reads.
//
// The FOLD here is genuinely sequential and not a shortcut. A recurrence cannot
// be widened the way a projection can: token t+1 reads the matrix token t wrote.
// What can be widened is everything AROUND it, and that turned out to be almost
// all of it — see `op_delta_net`.

use super::{CudaExec, Quantized};
use crate::DeltaStateLayout;

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
    /// Query, key and value of ONE CHUNK, convolved, normalized and repeated to
    /// cover the value heads — everything the fold reads.
    q32: DevBuffer,
    k32: DevBuffer,
    v: DevBuffer,
    /// Decay and write gate of the chunk, in f32 because the recurrence is.
    g: DevBuffer,
    beta: DevBuffer,
    /// The convolution window as it stood after each token of the chunk. The
    /// prepare kernel writes them for rollback; here only the LAST one is read,
    /// and it is the state the next chunk starts from.
    conv_checkpoints: DevBuffer,
    /// The fold's answer for the whole chunk.
    o: DevBuffer,
    /// Its gated normalization, for EVERY row. The output projection that
    /// reads it is stateless, so keeping every row lets it run once over the
    /// weights instead of once per token — which at this width is the
    /// difference between reading nine megabytes and reading them `tokens`
    /// times.
    normed: DevBuffer,
    rows: usize,
    chunk: usize,
}

/// Tokens the recurrent layer prepares and folds at a time.
///
/// Bounded, and not equal to the step, because the prepare kernel writes one
/// convolution window PER TOKEN: at the width of a 27B hybrid that is about
/// 48 KiB a token, so a 4096-token prompt would ask for two hundred megabytes
/// of scratch to read three hundred bytes of it. The fold is in-place, so
/// chunking it changes nothing about the result.
const DELTA_CHUNK: usize = 256;

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
        let chunk = rows.min(DELTA_CHUNK);
        let c = chunk as u32;
        let window = ssm.d_conv - 1;
        let fresh = DeltaScratch {
            mixed: f16b(n * ssm.mixed_width())?,
            z: f16b(n * ssm.value_width())?,
            alpha: f16b(n * ssm.v_heads)?,
            beta_raw: f16b(n * ssm.v_heads)?,
            q32: f16b(c * ssm.value_width())?,
            k32: f16b(c * ssm.value_width())?,
            v: f16b(c * ssm.value_width())?,
            g: f32b(c * ssm.v_heads)?,
            beta: f32b(c * ssm.v_heads)?,
            conv_checkpoints: f16b(c * ssm.mixed_width() * window)?,
            o: f16b(c * ssm.value_width())?,
            normed: f16b(n * ssm.value_width())?,
            rows,
            chunk,
        };
        self.forget_graphs();
        *held = Some(fresh.clone());
        Ok(fresh)
    }

    /// One projection of every row of the step, straight into a scratch slot.
    ///
    /// Not `matmul`, because that writes an `Act` and these four outputs are
    /// not activations the vocabulary names — they are this operation's
    /// insides. Same kernels and the same format table, though; a recurrent
    /// layer's projections are quantized like every other.
    ///
    /// And unlike every other, they do NOT quantize the ACTIVATION — neither to
    /// e4m3 for a prompt nor to int8 for a step, which is why this is not
    /// `gemv_decode`. Both halves of both were measured. Neither buys anything —
    /// e4m3 gave 69 tok/s against 68, int8 48,7 against 48,8, because what costs
    /// here is the sequential fold and not these four multiplications — and both
    /// cost accuracy: e4m3 took the reference comparison from 0,545% of logit
    /// spread to 2,931% with a genuine swap in the leading five, int8 took the
    /// step-by-step one from 0,443% to 0,573%. A recurrence composes its input
    /// over the whole sequence, so mantissa lost at the front comes back
    /// multiplied; attention has no such lever, which is why the same trade is
    /// right there and wrong here.
    fn project(&self, w: &Quantized, y: &DevBuffer, x: &DevBuffer, rows: usize) -> Result<()> {
        if rows == 1 {
            return self.gemv_by_kind(w.quant, y, &w.blocks, x, w.rows, w.cols, w.output_scale);
        }
        self.gemm_by_kind(
            w.quant,
            y,
            &w.blocks,
            0,
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
        let (value, mixed) = (ssm.value_width() as usize, ssm.mixed_width() as usize);
        let (v_heads, d_state) = (ssm.v_heads as usize, ssm.d_state as usize);
        // The prepare kernel repeats key head `h % k_heads` into value head `h`,
        // so it needs the same divisibility the per-token path needed.
        if !ssm.v_heads.is_multiple_of(ssm.k_heads) || ssm.k_heads == 0 {
            return Err(ForgeError::Unsupported(format!(
                "{} głowic wartości nie dzieli się na {} głowic klucza",
                ssm.v_heads, ssm.k_heads
            )));
        }

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

        // Który układ macierzy stanu ta karta liczy szybciej. `ValueKey` trzyma
        // kolumnę wartości w rejestrach linii i rozkłada głowicę na trzydzieści
        // dwa bloki zamiast na dwa, więc krok generacji ma czym zakryć opóźnienie
        // pamięci: zmierzone 65 us wobec 6 us na warstwę. Układ jest własnością
        // WYKONAWCY — poza tym skanem nikt tu w macierz nie zagląda.
        let value_key =
            self.kernels.preferred_delta_state_layout(d_state) == DeltaStateLayout::ValueKey;

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
        let window = ssm.d_conv as usize - 1;
        let checkpoint_bytes = mixed * window * 2;
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
            let conv_state =
                self.device
                    .sub_buffer(&conv_buf, cfg.conv_offset(slot), cfg.conv_bytes())?;
            let matrix =
                self.device
                    .sub_buffer(&state_buf, cfg.state_offset(slot), cfg.state_bytes())?;
            let mut first = 0usize;
            while first < tokens {
                let take = (tokens - first).min(s.chunk);
                let row = lane * tokens + first;

                // Everything between the two projections except the fold is a
                // function of ONE token, so it does not have to be walked token
                // by token — and walking it was what a prompt actually cost
                // here. The convolution, both head norms, the repeat and both
                // gates come out of one launch for the whole chunk.
                self.kernels.deltanet_prepare_f16(
                    &s.q32,
                    &s.k32,
                    &s.v,
                    &s.g,
                    &s.beta,
                    &s.conv_checkpoints,
                    &conv_state,
                    &self
                        .device
                        .sub_buffer(&s.mixed, row * mixed * 2, take * mixed * 2)?,
                    &conv_w,
                    &self
                        .device
                        .sub_buffer(&s.alpha, row * v_heads * 2, take * v_heads * 2)?,
                    &self
                        .device
                        .sub_buffer(&s.beta_raw, row * v_heads * 2, take * v_heads * 2)?,
                    &dt_bias,
                    &a,
                    take,
                    ssm.k_heads as usize,
                    v_heads,
                    d_state,
                    ssm.d_conv as usize,
                    self.shape.eps,
                    &self.stream,
                )?;
                // The checkpoint of the chunk's last token IS the window the
                // next chunk starts from — same layout, so it is a copy and not
                // a recomputation. It has to happen after the prepare and
                // before the next one, which the stream already guarantees.
                self.device.copy(
                    &s.conv_checkpoints,
                    (take - 1) * checkpoint_bytes,
                    &conv_state,
                    0,
                    checkpoint_bytes,
                    &self.stream,
                )?;
                // The fold, and only the fold, is sequential — and it is
                // sequential INSIDE one launch, which is the difference between
                // a kernel per token and a kernel per chunk.
                // Długi kawałek dostaje wariant TRWAŁY, który przypisuje osnowie
                // dwie kolumny stanu i przechodzi cały kawałek bez wracania po
                // stan. Krótki go nie chce: jego przewaga jest w amortyzacji, a
                // pojedynczy krok zapłaciłby tylko za rozruch.
                if value_key && take > 128 {
                    self.kernels.deltanet_value_key_scan_persistent_f16(
                        &s.o, &matrix, &s.q32, &s.k32, &s.v, &s.g, &s.beta, take, v_heads,
                        &self.stream,
                    )?;
                } else if value_key {
                    self.kernels.deltanet_value_key_scan_inplace_f16(
                        &s.o, &matrix, &matrix, &s.q32, &s.k32, &s.v, &s.g, &s.beta, 1, take,
                        v_heads, &self.stream,
                    )?;
                } else {
                    self.kernels.deltanet_gated_scan_inplace_f16(
                        &s.o,
                        &matrix,
                        &s.q32,
                        &s.k32,
                        &s.v,
                        &s.g,
                        &s.beta,
                        take,
                        v_heads,
                        d_state,
                        &self.stream,
                    )?;
                }
                // The gated norm is per (token, head) and the weight is shared
                // across heads, so the chunk's tokens flatten into the head axis
                // rather than needing a pass of their own.
                self.kernels.deltanet_gated_rmsnorm_f16_at(
                    &s.normed,
                    row * value * 2,
                    &s.o,
                    &s.z,
                    row * value * 2,
                    &norm_w,
                    take * v_heads,
                    d_state,
                    self.shape.eps,
                    &self.stream,
                )?;
                first += take;
            }
        }
        // ONE pass over the output weights for the whole step. Everything above
        // is sequential because it carries state; this is not, and leaving it
        // inside the loop made a prompt read the same nine megabytes once per
        // token — per layer.
        self.project(out_w, self.buf(out), &s.normed, rows)?;
        Ok(())
    }
}
