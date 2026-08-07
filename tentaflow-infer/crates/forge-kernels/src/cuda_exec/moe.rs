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

use crate::launchers::moe::{GroupedTiles, GROUPED_TILE_ROWS_MIN};
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
            // One entry per tile of the grouped launch. The worst case is one
            // tile per expert plus one per full tile of selections, which is
            // what an even split and a maximally uneven one each cost.
            tile_expert: dev((experts + selections / GROUPED_TILE_ROWS_MIN + 1) * 4)?,
            tile_first: dev((experts + selections / GROUPED_TILE_ROWS_MIN + 1) * 4)?,
            tile_end: dev((experts + selections / GROUPED_TILE_ROWS_MIN + 1) * 4)?,
            identity: dev(selections * 4)?,
            grouped_x: dev(selections * hidden * 2)?,
            grouped_gate: dev(selections * inter * 2)?,
            grouped_up: dev(selections * inter * 2)?,
            grouped_out: dev(selections * hidden * 2)?,
            grouped_xq: dev(selections * hidden.max(inter) / 64 * 36)?,
            grouped_xs: dev(selections * 4)?,
            logits: dev(rows * experts * 4)?,
            selections,
            experts,
        };
        // Both combines read a slot table, and for both it is the identity:
        // the shared expert has one selection per token, and a decode step
        // leaves every selection at its own row in the router's order. Written
        // once, long enough for the wider of the two.
        let identity: Vec<i32> = (0..selections as i32).collect();
        self.device
            .write(bytemuck::cast_slice(&identity), &fresh.identity, 0)?;
        self.forget_graphs();
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
        self.device.write(bytemuck::cast_slice(&addrs), &table, 0)?;
        self.expert_tables.borrow_mut().insert(id.0, table.clone());
        Ok(table)
    }

    /// Puts the grouped activation into four-bit form, when the stack that will
    /// read it is in four-bit form too.
    ///
    /// Nothing happens for any other format — the block-scaled instruction takes
    /// FOUR BITS ON BOTH SIDES, so this pass exists for exactly the stacks that
    /// can use it, and asking the weight is how it knows.
    fn quantize_grouped(
        &self,
        w: &Quantized,
        moe: &MoeScratch,
        x: &DevBuffer,
        cols: usize,
        selections: usize,
    ) -> Result<()> {
        if w.quant != QuantKind::MXFP4
            || !self.kernels.supports_mxf4_grouped()
            || !cols.is_multiple_of(64)
        {
            return Ok(());
        }
        self.kernels.quantize_act_mxf4(
            &moe.grouped_xq,
            &moe.grouped_xs,
            x,
            cols,
            selections,
            &self.stream,
        )
    }

    /// One projection of every expert over its own block of grouped rows.
    ///
    /// Two shapes, and which one is faster is a property of the FORMAT rather
    /// than of the model — measured, because the two answers are opposite. The
    /// int8 tiles want one grid spanning every expert (7,6× on Qwen3-30B); the
    /// f16 MXFP4 tile wanted a launch per expert (1,3× on Qwen3.6-35B), because
    /// it dequantizes to f16 and is limited by memory. What that comparison was
    /// really measuring is that neither shape had a matrix unit under it: the
    /// per-expert launch covers eight blocks, and thirty thousand of those per
    /// prompt left the card idle. MXFP4 IS four-bit block-scaled data, so it
    /// goes to the four-bit matrix unit and takes the grouped shape with it —
    /// reading the SAME bytes, assembled into fragments as the tile is staged.
    #[allow(clippy::too_many_arguments)]
    /// One projection of every expert on the four-bit matrix unit, or `false`
    /// when this weight has no such form.
    ///
    /// Separate from `project_experts` because a STEP reaches it without any of
    /// the surrounding sort: the tile table of a step is constant, so there are
    /// no per-expert row counts to hand it.
    #[allow(clippy::too_many_arguments)]
    fn mxf4_grouped(
        &self,
        id: WeightId,
        w: &Quantized,
        y: &DevBuffer,
        moe: &MoeScratch,
        tiles: &GroupedTiles<'_>,
        experts: usize,
        rows: usize,
        selections: usize,
    ) -> Result<bool> {
        if w.quant != QuantKind::MXFP4 || !self.kernels.supports_mxf4_grouped() {
            return Ok(false);
        }
        let table = self.expert_table(id, w, experts)?;
        self.kernels.gemm_mxf4_grouped(
            y,
            &table,
            &moe.grouped_xq,
            &moe.grouped_xs,
            GroupedTiles {
                expert: tiles.expert,
                first: tiles.first,
                end: tiles.end,
                count: tiles.count,
            },
            rows,
            w.cols,
            selections,
            &self.stream,
        )?;
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn project_experts(
        &self,
        id: WeightId,
        w: &Quantized,
        y: &DevBuffer,
        x: &DevBuffer,
        moe: &MoeScratch,
        starts: &[u32],
        tiles: &GroupedTiles<'_>,
        experts: usize,
        rows: usize,
        selections: usize,
    ) -> Result<()> {
        if self.mxf4_grouped(id, w, y, moe, tiles, experts, rows, selections)? {
            return Ok(());
        }
        let table = self.expert_table(id, w, experts)?;
        if w.quant != QuantKind::MXFP4 {
            return self.kernels.gemm_grouped_experts(
                w.quant,
                y,
                &table,
                x,
                GroupedTiles {
                    expert: tiles.expert,
                    first: tiles.first,
                    end: tiles.end,
                    count: tiles.count,
                },
                rows,
                w.cols,
                selections,
                &self.stream,
            );
        }
        let stride = w.blocks.len() / experts;
        for e in 0..experts {
            let (from, to) = (starts[e] as usize, starts[e + 1] as usize);
            if from == to {
                continue;
            }
            let n = to - from;
            let xe = self
                .device
                .sub_buffer(x, from * w.cols * 2, n * w.cols * 2)?;
            let ye = self.device.sub_buffer(y, from * rows * 2, n * rows * 2)?;
            self.gemm_by_kind(
                w.quant,
                &ye,
                &w.blocks,
                e * stride,
                &xe,
                rows,
                w.cols,
                n,
                w.output_scale,
            )?;
        }
        Ok(())
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
                return self.gemv_decode(w, y, x, r);
            }
            self.gemm_by_kind(w.quant, y, &w.blocks, 0, x, r, w.cols, rows, w.output_scale)
        };
        project(router, &moe.shared_logit, x, 1)?;
        self.kernels.moe_sigmoid_f16_to_f32(
            &moe.shared_scale,
            &moe.shared_logit,
            rows,
            &self.stream,
        )?;
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
        // Two launches and not one, because the two halves have opposite
        // shapes. The projection is `experts x hidden` against the step's rows
        // — an ordinary multiply, and at decode a million bytes of router
        // weight that one block per token would push through a single
        // multiprocessor. The selection genuinely is one block per token.
        if rows == 1 {
            self.kernels.gemv_f16_out_f32(
                &moe.logits,
                &router.blocks,
                self.buf(x),
                experts,
                hidden,
                &self.stream,
            )?;
        } else {
            self.kernels.gemm_f16_out_f32_at(
                &moe.logits,
                &router.blocks,
                0,
                self.buf(x),
                experts,
                hidden,
                rows,
                &self.stream,
            )?;
        }
        self.kernels.moe_topk_f32(
            &moe.ids,
            &moe.weights,
            &moe.logits,
            &moe.counts,
            rows,
            experts,
            top_k,
            norm_topk,
            &self.stream,
        )?;

        if rows >= GROUPED_MIN_ROWS {
            self.moe_grouped(
                out,
                x,
                [gate_id, up_id, down_id],
                experts,
                top_k,
                &moe,
                step,
            )?;
        } else {
            self.moe_per_token(
                out,
                x,
                [gate_id, up_id, down_id],
                experts,
                top_k,
                &moe,
                step,
            )?;
        }
        if let Some(sh) = shared {
            self.shared_expert(sh, self.buf(out), self.buf(x), rows, &moe)?;
        }
        Ok(())
    }

    /// Every selection read by id on device, all of them in one launch each.
    ///
    /// The step is too narrow to group — every expert is chosen once or twice,
    /// so sorting would hand a handful of rows to a tile built for hundreds —
    /// but it is not too narrow to BATCH. The kernels are the single-row ones
    /// either way; what changes is that the selection comes from the grid
    /// instead of from a launch parameter, so a layer costs five kernels rather
    /// than five per expert.
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
        let selections = rows * top_k;
        let [gate, up, down] = stacks.map(|id| self.quant(id));
        let (gate, up, down) = (gate?, up?, down?);

        for (id, w, y, width) in [
            (stacks[0], gate, &moe.grouped_gate, inter),
            (stacks[1], up, &moe.grouped_up, inter),
        ] {
            let table = self.expert_table(id, w, experts)?;
            self.kernels.gemv_gidx_batch(
                w.quant,
                y,
                &table,
                self.buf(x),
                width,
                w.cols,
                &moe.ids,
                selections,
                top_k,
                &self.stream,
            )?;
        }
        self.kernels.glu_mul_f16(
            FfnActivation::SiLU,
            &moe.grouped_gate,
            &moe.grouped_gate,
            &moe.grouped_up,
            selections * inter,
            &self.stream,
        )?;
        let down_table = self.expert_table(stacks[2], down, experts)?;
        self.kernels.gemv_gidx_batch(
            down.quant,
            &moe.grouped_out,
            &down_table,
            &moe.grouped_gate,
            hidden,
            down.cols,
            &moe.ids,
            selections,
            // One, not `top_k`: the half this reads was written per SELECTION
            // above, so every selection already owns its row.
            1,
            &self.stream,
        )?;
        // Every selection sits at its own row here, in the router's order, so
        // the slot table IS the identity over selections — no reorder happened
        // to invert.
        self.kernels.moe_combine_f16(
            self.buf(out),
            &moe.grouped_out,
            &moe.identity,
            &moe.weights,
            rows,
            hidden,
            top_k,
            true,
            &self.stream,
        )
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

        // One entry per tile, saying which expert that tile reads and where its
        // expert's block begins and ends. Built here because this is where the
        // row counts are known, and uploaded once for all three projections.
        //
        // The STRIDE is the narrowest tile among those three, because a tile
        // covers its own width from `tile_first` and does not loop past it. The
        // three projections of one layer need not share a format — Q4_K_M puts
        // six bits on `ffn_down` and four on the other two — so the table has to
        // fit the narrowest of them or the widest one drops rows.
        let stride = [gate, up, down]
            .iter()
            .map(|w| self.kernels.grouped_tile_rows(w.quant))
            .min()
            .expect("three projections");
        let mut tile_expert = Vec::new();
        let mut tile_first = Vec::new();
        let mut tile_end = Vec::new();
        for e in 0..experts {
            let (from, to) = (starts[e] as usize, starts[e + 1] as usize);
            let mut at = from;
            while at < to {
                tile_expert.push(e as i32);
                tile_first.push(at as i32);
                tile_end.push(to as i32);
                at += stride;
            }
        }
        for (host, dev) in [
            (&tile_expert, &moe.tile_expert),
            (&tile_first, &moe.tile_first),
            (&tile_end, &moe.tile_end),
        ] {
            self.device.write(bytemuck::cast_slice(host), dev, 0)?;
        }
        let tiles = GroupedTiles {
            expert: &moe.tile_expert,
            first: &moe.tile_first,
            end: &moe.tile_end,
            count: tile_expert.len(),
        };

        self.kernels.gather_rows_f16(
            &moe.grouped_x,
            self.buf(x),
            &moe.order,
            selections,
            hidden,
            &self.stream,
        )?;
        // Quantized ONCE for the pair that reads it, not once per projection:
        // gate and up multiply the same rows, and this pass reads every one of
        // them. `down` gets its own below, at the other width.
        self.quantize_grouped(gate, &moe, &moe.grouped_x, hidden, selections)?;
        for (id, w, y) in [
            (stacks[0], gate, &moe.grouped_gate),
            (stacks[1], up, &moe.grouped_up),
        ] {
            self.project_experts(
                id,
                w,
                y,
                &moe.grouped_x,
                &moe,
                &starts,
                &tiles,
                experts,
                inter,
                selections,
            )?;
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
        self.quantize_grouped(down, &moe, &moe.grouped_gate, inter, selections)?;
        self.project_experts(
            stacks[2],
            down,
            &moe.grouped_out,
            &moe.grouped_gate,
            &moe,
            &starts,
            &tiles,
            experts,
            hidden,
            selections,
        )?;
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
