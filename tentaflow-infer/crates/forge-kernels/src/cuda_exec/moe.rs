// ===== File: moe.rs — the mixture block on CUDA =====
//
// Split out of the executor rather than folded into it, because it is the one
// part with a memory model of its own: a pointer table per expert stack, a
// scratch that only a routed layer allocates, and a launch sequence driven by
// ids the device produced. The rest of `CudaExec` never looks at any of it.

use super::{CudaExec, MoeScratch, Quantized};

use forge_formats::FfnActivation;
use forge_graph::{Act, Shared, Step, WeightId};
use forge_hal::{DevBuffer, Pool};
use forge_types::{ForgeError, MemKind, QuantKind, Result};

impl CudaExec {
    /// Scratch the mixture needs, allocated the first time one is asked for.
    ///
    /// Lazily, because a dense model must not pay for it: these buffers are
    /// sized by the expert count and the routed width, neither of which a dense
    /// checkpoint has. It grows rather than being reallocated per step.
    fn ensure_moe(&self, selections: usize, experts: usize, hidden: usize) -> Result<MoeScratch> {
        let mut held = self.moe.borrow_mut();
        if let Some(s) = held.as_ref() {
            if s.selections >= selections && s.experts >= experts {
                return Ok(s.clone());
            }
        }
        let fresh = MoeScratch {
            ids: self
                .device
                .alloc(selections * 4, MemKind::Device, Pool::Activations)?,
            weights: self
                .device
                .alloc(selections * 4, MemKind::Device, Pool::Activations)?,
            counts: self
                .device
                .alloc(experts * 4, MemKind::Device, Pool::Activations)?,
            tmp: self
                .device
                .alloc(hidden * 2, MemKind::Device, Pool::Activations)?,
            shared_logit: self.device.alloc(2, MemKind::Device, Pool::Activations)?,
            shared_scale: self.device.alloc(4, MemKind::Device, Pool::Activations)?,
            selections,
            experts,
        };
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
    /// never crosses the bus. Two formats have such a kernel, and a stack in a
    /// third stops here rather than reaching a kernel that would read its bytes
    /// as another format's.
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

    /// The always-on expert of one token, folded on top of the routed sum.
    ///
    /// Its gate never crosses the bus either, for the same reason the routed
    /// ids do not: the logit is turned into a scale ON DEVICE and read from
    /// there by the same accumulate kernel the routed experts use. A readback
    /// here would cost one synchronize per layer per token, which on this
    /// checkpoint is forty of them.
    fn shared_expert(
        &self,
        sh: &Shared,
        out: &DevBuffer,
        out_off: usize,
        x_row: &DevBuffer,
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
        self.gemv_by_kind(
            router.quant,
            &moe.shared_logit,
            &router.blocks,
            x_row,
            1,
            hidden,
            router.output_scale,
        )?;
        self.kernels
            .moe_sigmoid_f16_to_f32(&moe.shared_scale, &moe.shared_logit, &self.stream)?;
        self.gemv_by_kind(
            gate.quant,
            &self.scratch.gate,
            &gate.blocks,
            x_row,
            width,
            hidden,
            gate.output_scale,
        )?;
        self.gemv_by_kind(
            up.quant,
            &self.scratch.up,
            &up.blocks,
            x_row,
            width,
            hidden,
            up.output_scale,
        )?;
        self.kernels.glu_mul_f16(
            FfnActivation::SiLU,
            &self.scratch.act,
            &self.scratch.gate,
            &self.scratch.up,
            width,
            &self.stream,
        )?;
        self.gemv_by_kind(
            down.quant,
            &moe.tmp,
            &down.blocks,
            &self.scratch.act,
            hidden,
            width,
            down.output_scale,
        )?;
        self.kernels.moe_scale_add_gidx_f16(
            out,
            out_off,
            &moe.tmp,
            0,
            hidden,
            &moe.shared_scale,
            0,
            false,
            &self.stream,
        )
    }

    /// Routing, selection and the SwiGLU of the chosen experts.
    ///
    /// The router runs once for the whole step; the experts then run token by
    /// token, because the kernels that read their id on device take one row of
    /// activations. That is a launch count, not a different answer, and the
    /// milestone this belongs to is correctness.
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
        let [router, gate_id, up_id, down_id] = weights;
        let (experts, top_k) = (experts as usize, top_k as usize);
        let rows = step.rows() as usize;
        let hidden = self.shape.hidden as usize;
        let inter = self.shape.inter as usize;
        let (router, gate, up, down) = (
            self.quant(router)?,
            self.quant(gate_id)?,
            self.quant(up_id)?,
            self.quant(down_id)?,
        );
        let tables = [
            self.expert_table(gate_id, gate, experts)?,
            self.expert_table(up_id, up, experts)?,
            self.expert_table(down_id, down, experts)?,
        ];
        // The stacks are flat, so their row count is the only statement that
        // they hold the number of experts the routing is about to index. A
        // mismatch addresses another expert's rows and answers fluently.
        for (name, w, per_expert) in [("gate", gate, inter), ("up", up, inter), ("down", down, hidden)] {
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

        let moe = self.ensure_moe(rows * top_k, experts, hidden)?;
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

        for t in 0..rows {
            let x_row = self.device.sub_buffer(self.buf(x), t * hidden * 2, hidden * 2)?;
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
            if let Some(sh) = shared {
                let out_off = t * hidden * 2;
                self.shared_expert(sh, self.buf(out), out_off, &x_row, &moe)?;
            }
        }
        Ok(())
    }
}
