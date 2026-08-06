// ===== File: moe.rs — the mixture block on CUDA =====
//
// Split out of the executor rather than folded into it, because it is the one
// part with a memory model of its own: a pointer table per expert stack, a
// scratch that only a routed layer allocates, and a launch sequence driven by
// ids the device produced. The rest of `CudaExec` never looks at any of it.
//
// The block runs two ways and the width of the step decides which. At one row
// there is nothing to group: the token picks `top_k` experts and each is one
// matrix-vector product, read straight out of the stack by a kernel that takes
// the expert id on device. At prompt width that shape is a disaster — every
// token launching `3·top_k` kernels means a 512-token prompt launching twelve
// thousand of them PER LAYER, each one reading a whole expert's weights for a
// single row. So a prompt REORDERS instead: selections are grouped by expert,
// every expert multiplies its own block of rows once, and the answers are put
// back where their tokens are. The arithmetic is the same and the accumulation
// order is the same; what changes is that the weights are read once each.

use super::{CudaExec, MoeScratch, Quantized};

use forge_formats::FfnActivation;
use forge_graph::{Act, Shared, Step, WeightId};
use forge_hal::{DevBuffer, Pool};
use forge_types::{ForgeError, MemKind, QuantKind, Result};

/// Rows below which grouping cannot pay for itself.
///
/// Grouping needs the selections on the host to know how many rows each expert
/// takes, which is one synchronize per layer. At decode width the per-token
/// route launches few enough kernels that the synchronize is the larger cost,
/// and the matrix-vector kernels are the ones built for a single row anyway.
const GROUPED_MIN_ROWS: usize = 16;

impl CudaExec {
    /// Scratch the mixture needs, allocated the first time one is asked for.
    ///
    /// Lazily, because a dense model must not pay for it: these buffers are
    /// sized by the expert count and the routed width, neither of which a dense
    /// checkpoint has. It grows rather than being reallocated per step.
    fn ensure_moe(
        &self,
        rows: usize,
        selections: usize,
        experts: usize,
        hidden: usize,
        inter: usize,
    ) -> Result<MoeScratch> {
        let mut held = self.moe.borrow_mut();
        if let Some(s) = held.as_ref() {
            if s.selections >= selections && s.experts >= experts {
                return Ok(s.clone());
            }
        }
        let dev = |bytes: usize| self.device.alloc(bytes, MemKind::Device, Pool::Activations);
        // The grouped buffers are sized by SELECTIONS, not rows: one token
        // appears in `top_k` of them, which is the whole point of the reorder.
        let fresh = MoeScratch {
            ids: dev(selections * 4)?,
            weights: dev(selections * 4)?,
            counts: dev(experts * 4)?,
            tmp: dev(rows * hidden * 2)?,
            shared_logit: dev(rows * 2)?,
            shared_scale: dev(rows * 4)?,
            order: dev(selections * 4)?,
            slots: dev(selections * 4)?,
            identity: dev(rows * 4)?,
            grouped_x: dev(selections * hidden * 2)?,
            grouped_gate: dev(selections * inter * 2)?,
            grouped_up: dev(selections * inter * 2)?,
            grouped_out: dev(selections * hidden * 2)?,
            selections,
            experts,
        };
        // The shared expert folds through the same combine kernel as the routed
        // sum, with one selection per token; its slot table is therefore the
        // identity and never changes, so it is written once.
        let identity: Vec<i32> = (0..rows as i32).collect();
        self.device
            .write(bytemuck::cast_slice(&identity), &fresh.identity, 0)?;
        *held = Some(fresh.clone());
        Ok(fresh)
    }

    /// The per-expert base addresses a `_gidx` kernel indexes.
    ///
    /// These kernels do not take the stack — they take a TABLE OF POINTERS and
    /// dereference `table[ids[sel]]`. Handing them the weight instead reads its
    /// bytes as addresses, which is an illegal access rather than a wrong
    /// number, and that is the one mercy in it.
    ///
    /// Here the table is degenerate: every entry points into one contiguous
    /// stack. That is deliberate rather than a shortcut — the indirection is
    /// exactly what lets an expert's bytes live somewhere else later, so
    /// residency becomes a rewrite of this table and not a change to any
    /// kernel call.
    fn expert_table(&self, id: WeightId, w: &Quantized, experts: usize) -> Result<DevBuffer> {
        if let Some(table) = self.expert_tables.borrow().get(&id.0) {
            return Ok(table.clone());
        }
        if !w.blocks.len().is_multiple_of(experts) {
            return Err(ForgeError::Unsupported(format!(
                "stos {} B nie dzieli się na {experts} ekspertów",
                w.blocks.len()
            )));
        }
        let stride = w.blocks.len() / experts;
        let base = w.blocks.device_ptr();
        let addrs: Vec<u64> = (0..experts).map(|e| base + (e * stride) as u64).collect();
        let table = self
            .device
            .alloc(experts * 8, MemKind::Device, Pool::Weights)?;
        self.device
            .write(bytemuck::cast_slice(&addrs), &table, 0)?;
        self.expert_tables.borrow_mut().insert(id.0, table.clone());
        Ok(table)
    }

    /// One expert's projection, over the row window its id selects.
    ///
    /// The id is read ON DEVICE, which is the whole reason these kernels exist
    /// and the reason the vocabulary carries no expert numbers: the selection
    /// never crosses the bus. Three formats have such a kernel, and a stack in
    /// a fourth stops here rather than reaching a kernel that would read its
    /// bytes as another format's.
    #[allow(clippy::too_many_arguments)]
    fn gemv_gidx(
        &self,
        w: &Quantized,
        table: &DevBuffer,
        y: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        ids: &DevBuffer,
        sel: usize,
    ) -> Result<()> {
        match w.quant {
            QuantKind::Q4K => self
                .kernels
                .gemv_q4_k_dp4a_f16_gidx(y, table, x, rows, w.cols, ids, sel, &self.stream),
            QuantKind::Q6K => self
                .kernels
                .gemv_q6_k_f16_gidx(y, table, x, rows, w.cols, ids, sel, &self.stream),
            QuantKind::MXFP4 => self
                .kernels
                .gemv_mxfp4_f16_gidx(y, table, x, rows, w.cols, ids, sel, &self.stream),
            other => Err(ForgeError::Unsupported(format!(
                "{other:?}: stos ekspertów nie ma kernela adresowanego na urządzeniu"
            ))),
        }
    }

    /// One expert's projection over a CONTIGUOUS block of rows.
    ///
    /// The expert is addressed by byte offset into the flat stack rather than
    /// through the pointer table: a whole block of tokens shares one expert
    /// here, so the id is a constant of the launch and does not have to be read
    /// per row. Which also means an expert with no tokens costs nothing, where
    /// the per-token route paid for it once per token that did not pick it.
    #[allow(clippy::too_many_arguments)]
    fn gemm_expert(
        &self,
        w: &Quantized,
        expert: usize,
        experts: usize,
        y: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        n_tokens: usize,
    ) -> Result<()> {
        let stride = w.blocks.len() / experts;
        self.gemm_by_kind(
            w.quant,
            y,
            &w.blocks,
            expert * stride,
            x,
            rows,
            w.cols,
            n_tokens,
            w.output_scale,
        )
    }

    /// The always-on expert of every row of the step.
    ///
    /// Its gate never crosses the bus, for the same reason the routed ids do
    /// not: the logits are turned into scales ON DEVICE and read from there by
    /// the same combine kernel the routed experts use, with one selection per
    /// token. A readback here would cost one synchronize per layer.
    fn shared_expert(
        &self,
        sh: &Shared,
        out: &DevBuffer,
        x: &DevBuffer,
        rows: usize,
        moe: &MoeScratch,
    ) -> Result<()> {
        let hidden = self.shape.hidden as usize;
        let (gate, up, down, router) = (
            self.quant(sh.gate)?,
            self.quant(sh.up)?,
            self.quant(sh.down)?,
            self.quant(sh.router)?,
        );
        // This expert has a width of its own, stated only by its stacks. The
        // shape's `inter` is the widest feed-forward in the model, so it bounds
        // the scratch these projections write into without being their width.
        let width = gate.rows;
        if width > self.shape.inter as usize || up.rows != width || down.cols != width {
            return Err(ForgeError::Unsupported(format!(
                "ekspert współdzielony: gate {width}, up {}, down×{} przy szerokości pośredniej {}",
                up.rows, down.cols, self.shape.inter
            )));
        }
        if router.rows != 1 || down.rows != hidden {
            return Err(ForgeError::Unsupported(format!(
                "ekspert współdzielony: bramka {} wierszy, down {} wierszy wobec {hidden}",
                router.rows, down.rows
            )));
        }
        let project = |w: &Quantized, y: &DevBuffer, x: &DevBuffer, r: usize| -> Result<()> {
            if rows == 1 {
                return self.gemv_by_kind(w.quant, y, &w.blocks, x, r, w.cols, w.output_scale);
            }
            self.gemm_by_kind(w.quant, y, &w.blocks, 0, x, r, w.cols, rows, w.output_scale)
        };
        project(router, &moe.shared_logit, x, 1)?;
        self.kernels
            .moe_sigmoid_f16_to_f32(&moe.shared_scale, &moe.shared_logit, rows, &self.stream)?;
        project(gate, &self.scratch.gate, x, width)?;
        project(up, &self.scratch.up, x, width)?;
        self.kernels.glu_mul_f16(
            FfnActivation::SiLU,
            &self.scratch.act,
            &self.scratch.gate,
            &self.scratch.up,
            rows * width,
            &self.stream,
        )?;
        project(down, &moe.tmp, &self.scratch.act, hidden)?;
        self.kernels.moe_combine_f16(
            out,
            &moe.tmp,
            &moe.identity,
            &moe.shared_scale,
            rows,
            hidden,
            1,
            false,
            &self.stream,
        )
    }

    /// Routing, selection and the SwiGLU of the chosen experts.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn op_moe_ffn(
        &self,
        out: Act,
        x: Act,
        weights: [WeightId; 4],
        experts: u32,
        top_k: u32,
        norm_topk: bool,
        shared: Option<&Shared>,
        step: &Step,
    ) -> Result<()> {
        let [router_id, gate_id, up_id, down_id] = weights;
        let (experts, top_k) = (experts as usize, top_k as usize);
        let rows = step.rows() as usize;
        let hidden = self.shape.hidden as usize;
        let inter = self.shape.inter as usize;
        let (router, gate, up, down) = (
            self.quant(router_id)?,
            self.quant(gate_id)?,
            self.quant(up_id)?,
            self.quant(down_id)?,
        );
        // The stacks are flat, so their row count is the only statement that
        // they hold the number of experts the routing is about to index. A
        // mismatch addresses another expert's rows and answers fluently.
        for (name, w, per_expert) in [
            ("gate", gate, inter),
            ("up", up, inter),
            ("down", down, hidden),
        ] {
            if w.rows != experts * per_expert {
                return Err(ForgeError::Unsupported(format!(
                    "stos {name} ma {} wierszy, a {experts} ekspertów po {per_expert} to {}",
                    w.rows,
                    experts * per_expert
                )));
            }
        }
        if router.rows != experts {
            return Err(ForgeError::Unsupported(format!(
                "router ma {} wierszy wobec {experts} ekspertów",
                router.rows
            )));
        }

        let moe = self.ensure_moe(rows, rows * top_k, experts, hidden, inter)?;
        // The router is f16 here because its kernel is. That narrowing is the
        // executor's, and it is worth naming: the router picks experts, so a
        // rounding that flips a near-tie changes WHICH expert computes, not the
        // value it computes. The reference keeps f32 and the gate compares the
        // selections, so a flip is reported rather than absorbed.
        self.kernels.moe_router_f16(
            &moe.ids,
            &moe.weights,
            self.buf(x),
            &router.blocks,
            &moe.counts,
            rows,
            hidden,
            experts,
            top_k,
            norm_topk,
            &self.stream,
        )?;

        if rows >= GROUPED_MIN_ROWS {
            self.moe_grouped(out, x, [gate_id, up_id, down_id], experts, top_k, &moe, step)?;
        } else {
            self.moe_per_token(out, x, [gate_id, up_id, down_id], experts, top_k, &moe, step)?;
        }
        if let Some(sh) = shared {
            self.shared_expert(sh, self.buf(out), self.buf(x), rows, &moe)?;
        }
        Ok(())
    }

    /// One token at a time, each of its experts read by id on device.
    fn moe_per_token(
        &self,
        out: Act,
        x: Act,
        stacks: [WeightId; 3],
        experts: usize,
        top_k: usize,
        moe: &MoeScratch,
        step: &Step,
    ) -> Result<()> {
        let rows = step.rows() as usize;
        let hidden = self.shape.hidden as usize;
        let inter = self.shape.inter as usize;
        let [gate, up, down] = stacks.map(|id| self.quant(id));
        let (gate, up, down) = (gate?, up?, down?);
        let tables = [
            self.expert_table(stacks[0], gate, experts)?,
            self.expert_table(stacks[1], up, experts)?,
            self.expert_table(stacks[2], down, experts)?,
        ];
        for t in 0..rows {
            let x_row = self
                .device
                .sub_buffer(self.buf(x), t * hidden * 2, hidden * 2)?;
            let ids = self.device.sub_buffer(&moe.ids, t * top_k * 4, top_k * 4)?;
            let picks = self
                .device
                .sub_buffer(&moe.weights, t * top_k * 4, top_k * 4)?;
            for j in 0..top_k {
                self.gemv_gidx(gate, &tables[0], &self.scratch.gate, &x_row, inter, &ids, j)?;
                self.gemv_gidx(up, &tables[1], &self.scratch.up, &x_row, inter, &ids, j)?;
                self.kernels.glu_mul_f16(
                    FfnActivation::SiLU,
                    &self.scratch.act,
                    &self.scratch.gate,
                    &self.scratch.up,
                    inter,
                    &self.stream,
                )?;
                self.gemv_gidx(down, &tables[2], &moe.tmp, &self.scratch.act, hidden, &ids, j)?;
                self.kernels.moe_scale_add_gidx_f16(
                    self.buf(out),
                    t * hidden * 2,
                    &moe.tmp,
                    0,
                    hidden,
                    &picks,
                    j,
                    j == 0,
                    &self.stream,
                )?;
            }
        }
        Ok(())
    }

    /// Every expert once, over the block of rows that chose it.
    ///
    /// The selections come back to the host — the one synchronize in this path
    /// — because the row count of each expert's block is the grid of its
    /// launch, and a grid is not something a kernel can be told on device. What
    /// crosses is `rows·top_k` integers, and what it buys is a launch count
    /// bounded by the number of experts instead of by the number of tokens.
    #[allow(clippy::too_many_arguments)]
    fn moe_grouped(
        &self,
        out: Act,
        x: Act,
        stacks: [WeightId; 3],
        experts: usize,
        top_k: usize,
        moe: &MoeScratch,
        step: &Step,
    ) -> Result<()> {
        let rows = step.rows() as usize;
        let hidden = self.shape.hidden as usize;
        let inter = self.shape.inter as usize;
        let selections = rows * top_k;
        let [gate, up, down] = stacks.map(|id| self.quant(id));
        let (gate, up, down) = (gate?, up?, down?);

        // Explicitly, and not because a copy is a barrier: the HAL's streams
        // are non-blocking, so a host read runs on the legacy stream and would
        // happily return whatever stood in the buffer BEFORE the router wrote
        // it — the previous layer's selections, which route fluently.
        self.stream.synchronize()?;
        let mut ids = vec![0i32; selections];
        self.device
            .read(&moe.ids, 0, bytemuck::cast_slice_mut(&mut ids))?;

        // A counting sort over experts. `order[p]` is the token whose row the
        // gather puts at position p; `slots[t·top_k+j]` says where that token's
        // j-th expert landed, which is what puts the answers back afterwards.
        let mut starts = vec![0u32; experts + 1];
        for id in &ids {
            let e = *id as usize;
            if e >= experts {
                return Err(ForgeError::Unsupported(format!(
                    "router wybrał eksperta {e} przy {experts} w stosie"
                )));
            }
            starts[e + 1] += 1;
        }
        for e in 0..experts {
            starts[e + 1] += starts[e];
        }
        let mut cursor = starts.clone();
        let mut order = vec![0i32; selections];
        let mut slots = vec![0i32; selections];
        for (sel, id) in ids.iter().enumerate() {
            let p = &mut cursor[*id as usize];
            order[*p as usize] = (sel / top_k) as i32;
            slots[sel] = *p as i32;
            *p += 1;
        }
        self.device
            .write(bytemuck::cast_slice(&order), &moe.order, 0)?;
        self.device
            .write(bytemuck::cast_slice(&slots), &moe.slots, 0)?;

        self.kernels.gather_rows_f16(
            &moe.grouped_x,
            self.buf(x),
            &moe.order,
            selections,
            hidden,
            &self.stream,
        )?;
        for e in 0..experts {
            let (from, to) = (starts[e] as usize, starts[e + 1] as usize);
            if from == to {
                continue;
            }
            let n = to - from;
            let xe = self
                .device
                .sub_buffer(&moe.grouped_x, from * hidden * 2, n * hidden * 2)?;
            let ge = self
                .device
                .sub_buffer(&moe.grouped_gate, from * inter * 2, n * inter * 2)?;
            let ue = self
                .device
                .sub_buffer(&moe.grouped_up, from * inter * 2, n * inter * 2)?;
            self.gemm_expert(gate, e, experts, &ge, &xe, inter, n)?;
            self.gemm_expert(up, e, experts, &ue, &xe, inter, n)?;
        }
        // The activation is elementwise, so the whole grouped block goes at
        // once — it does not care which expert produced which row.
        self.kernels.glu_mul_f16(
            FfnActivation::SiLU,
            &moe.grouped_gate,
            &moe.grouped_gate,
            &moe.grouped_up,
            selections * inter,
            &self.stream,
        )?;
        for e in 0..experts {
            let (from, to) = (starts[e] as usize, starts[e + 1] as usize);
            if from == to {
                continue;
            }
            let n = to - from;
            let ae = self
                .device
                .sub_buffer(&moe.grouped_gate, from * inter * 2, n * inter * 2)?;
            let ye = self
                .device
                .sub_buffer(&moe.grouped_out, from * hidden * 2, n * hidden * 2)?;
            self.gemm_expert(down, e, experts, &ye, &ae, hidden, n)?;
        }
        self.kernels.moe_combine_f16(
            self.buf(out),
            &moe.grouped_out,
            &moe.slots,
            &moe.weights,
            rows,
            hidden,
            top_k,
            true,
            &self.stream,
        )
    }
}
