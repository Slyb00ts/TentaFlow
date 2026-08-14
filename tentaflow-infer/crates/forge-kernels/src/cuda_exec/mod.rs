// ===== File: cuda_exec.rs — the dense vocabulary, executed on CUDA =====
//
// The third implementer of the same contract, after Metal and the host
// reference. It shares no line with either of them except `Op`, `Act` and
// `WeightId` — which is the whole point: if the model description were secretly
// written for one card, a second card could not run it unchanged.
//
// Two things are deliberately different from the Metal executor, and both are
// properties of THESE kernels rather than preferences:
//
//   * Weights stay in the source's GGUF blocks. The Metal kernels index three
//     separate arrays and get them by rewriting; the kernels here read
//     superblocks directly, so rewriting would be work undone. The choice is
//     the executor's exactly so that both can be right.
//   * The KV cache is PAGED, and the model does not know it. `KvAppend` and
//     `Attention` name a layer and a lane; where that lane's context physically
//     sits is answered by `forge-state` — the SAME paged cache the engine uses,
//     not a second one written for this side. That is the question step 2 of
//     docs/ZADANIE_CUDA_EXECUTOR.md asks, and the answer is yes: paging needs
//     nothing from the vocabulary, and it needs only one implementation.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use half::f16;

use forge_formats::FfnActivation;
use forge_graph::{
    Act, ExecSpec, Executor, Layout, Op, QuantWeight, SsmShape, Step, Tile, WeightId, WeightStore,
};
use forge_hal::{DevBuffer, Device, Event, ExecGraph, Pool, Stream};
use forge_state::kv::{KvCache, KvConfig, KvLayerMap, KvQuant, SeqKv};
use forge_state::recurrent::{RecurrentConfig, RecurrentState};
use forge_types::{DType, DenseShape, ForgeError, MemKind, QuantKind, Result};

use crate::launchers::{Kernels, SAMPLE_SCRATCH_PAIRS};

mod control;
mod delta;
mod formats;
mod fp8;
mod graph;
mod moe;

/// Activation rows carried through the layers in one pass — lanes times tokens.
///
/// Bounded by the scratch it forces: at 4096 hidden and 11264 intermediate,
/// every extra row costs about 100 KB across the eleven slots.
const MAX_ROWS: u32 = 512;

/// Sequences held at once.
///
/// Small on purpose. Every lane is a live context and they share the page
/// budget below, so raising this without raising the budget only means the
/// lanes run out of pages sooner.
const MAX_LANES: u32 = 4;

/// Context one sequence may reach.
const SEQ_CAP: u32 = 4096;

/// Tokens in one KV page.
///
/// The page is the unit of allocation, so it trades two things off: a large
/// page wastes the tail of every sequence, a small one makes the page table
/// long and the indirection frequent. This is the size the engine's cache uses.
const PAGE: u32 = 256;

/// Splits of the context in the decode attention's partial pass.
const DECODE_SPLITS: u32 = 8;

/// A quantized weight, in the blocks the source packed and the kernels read.
struct Quantized {
    blocks: DevBuffer,
    /// Skalar całego tensora, gdy kernele formatu go biorą. Jedynka dla
    /// pozostałych — trzymany osobno, bo to on dzieli tabelę na dwie sekcje.
    output_scale: f32,
    /// Q4_K or Q6_K. Q4_K_M carries BOTH in one model — six bits on `attn_v`,
    /// `ffn_down` and the head, four on everything else — so this is a property
    /// of the weight and never of the model.
    quant: QuantKind,
    rows: usize,
    cols: usize,
}

enum Weight {
    Quant(Quantized),
    /// Normalization weights, widened or narrowed to the f16 the kernels read.
    Plain(DevBuffer),
}

/// Named activation slots. Everything is f16 except the logits, which are what
/// the sampler reads.
struct Scratch {
    h: DevBuffer,
    /// Unrounded residual values used by fused kernels for precise norm recomputation.
    h32: DevBuffer,
    norm: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn: DevBuffer,
    /// The attention output gate. As wide as `attn`, and paid for by every
    /// model — including the ones that never gate. Sized here rather than
    /// lazily because it is the TARGET of an ordinary projection, so it has to
    /// exist before the operation that fills it names it.
    attn_gate: DevBuffer,
    proj: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    logits: DevBuffer,
    /// Token ids of the current step, read by the embedding gather.
    ids: DevBuffer,
    /// Absolute position of every row, read by RoPE.
    positions: DevBuffer,
    /// First position of every lane, read by the KV append.
    bases: DevBuffer,
    /// Pages of every lane of this step, lane-major.
    pages: DevBuffer,
    /// Context length of every lane, read by the decode attention.
    lengths: DevBuffer,
    /// Per-split partial sums of the decode attention.
    parts: DevBuffer,
    /// Chosen id per lane.
    choice: DevBuffer,
    sample_vals: DevBuffer,
    sample_idx: DevBuffer,
}

/// Buffers only a mixture-of-experts layer needs.
///
/// Handles are cheap to clone and the allocations are not, so this is cloned
/// out of its cell rather than borrowed across the launches that use it.
#[derive(Clone)]
struct MoeScratch {
    /// Chosen expert per token, `[tokens][top_k]`, written by the router.
    ids: DevBuffer,
    /// Routing weight of each choice, same shape.
    weights: DevBuffer,
    /// How often each expert was picked — the router writes it; nothing here
    /// reads it yet, and residency is what will.
    counts: DevBuffer,
    /// The shared expert's output for every row of the step, before its gate
    /// scales it into the accumulator.
    tmp: DevBuffer,
    /// The shared expert's gate logit per row, and the same after the sigmoid —
    /// f16 and f32. The second is read by the combine kernel in the very place
    /// it reads a routing weight, so the gate never crosses the bus.
    shared_logit: DevBuffer,
    shared_scale: DevBuffer,
    /// The grouped order: `order[p]` is the token whose activation row sits at
    /// position p once the step's selections are sorted by expert.
    order: DevBuffer,
    /// Its inverse: where each token's j-th selection landed in that order.
    slots: DevBuffer,
    /// One entry per tile of the grouped launch: which expert that tile reads,
    /// and where its expert's block of rows begins and ends. Three arrays
    /// because a block reads all three and they are meaningless apart.
    tile_expert: DevBuffer,
    tile_first: DevBuffer,
    tile_end: DevBuffer,
    /// `[0, 1, 2, …]`. The slot table of both combines that do not reorder:
    /// the shared expert's one selection per token, and a decode step, whose
    /// selections already sit in the router's order.
    identity: DevBuffer,
    /// Activations, both feed-forward halves and the answers, all in grouped
    /// order. Sized by SELECTIONS rather than rows: one token appears in
    /// `top_k` of them, which is what the reorder is for.
    grouped_x: DevBuffer,
    grouped_gate: DevBuffer,
    grouped_up: DevBuffer,
    grouped_out: DevBuffer,
    /// The grouped activation in four-bit form, and its per-row multiplier.
    /// ONE buffer for both widths: gate and up read it at `hidden`, then `down`
    /// overwrites it at `inter`, and nothing reads across that boundary.
    grouped_xq: DevBuffer,
    grouped_xs: DevBuffer,
    /// Router logits of every row, `[rows][experts]` in f32.
    logits: DevBuffer,
    selections: usize,
    experts: usize,
}

/// The dense vocabulary on a CUDA device.
pub struct CudaExec {
    device: Arc<dyn Device>,
    kernels: Kernels,
    stream: Stream,
    weights: Vec<Weight>,
    /// Key and value per layer, `[strona][głowica KV][pozycja][wymiar]` f16 —
    /// the layout every paged kernel in the catalogue reads.
    /// Scratch a mixture-of-experts layer needs, made on first demand so a
    /// dense model never allocates it.
    moe: RefCell<Option<MoeScratch>>,
    /// Per-expert base addresses, one table per stack, built on first use.
    expert_tables: RefCell<HashMap<u32, DevBuffer>>,
    /// The e4m3 form of every weight that has been multiplied at prompt width.
    /// `None` records a weight that cannot have one, so the question is asked
    /// once per weight and not once per step.
    fp8: RefCell<HashMap<u32, Option<fp8::Fp8Pack>>>,
    /// Scratch a recurrent layer needs, made on first demand and shared by all
    /// of them — nothing in it survives the operation.
    delta: RefCell<Option<delta::DeltaScratch>>,
    /// The convolution windows and state matrices of every recurrent layer,
    /// held by the same crate that holds the pages.
    recurrent: RefCell<RecurrentState>,
    /// Geometry of the recurrent mixer, when this model has one.
    ssm: Option<SsmShape>,
    kv: RefCell<KvCache>,
    /// One slot's page table and length, owned by the shared cache's types.
    seqs: RefCell<Vec<SeqKv>>,
    scratch: Scratch,
    /// Kopie tego, co stoi w buforach sterujących na urządzeniu. Patrz
    /// `stage_i32` — bez nich każdy zapis byłby wyścigiem z tym, co już stoi w
    /// kolejce, a tablica stron jest ta sama dla wszystkich czterdziestu warstw
    /// kroku.
    staged: [RefCell<Vec<i32>>; 5],
    /// Pinned mirror of each control buffer. The copy out of it is
    /// STREAM-ORDERED against the kernels that read the buffer, which is what
    /// lets the write happen without draining the pipeline first.
    stage_host: [control::Staging; 5],
    /// Fence behind the last step's control copies. See `stage_values`.
    control_fence: Event,
    fence_live: Cell<bool>,
    /// Decode steps already recorded, by lane count.
    graphs: RefCell<HashMap<u32, ExecGraph>>,
    /// Lane counts that have run once. The first step of a shape allocates
    /// what is still lazy, so the second one is the one worth recording.
    warmed: RefCell<HashSet<u32>>,
    shape: DenseShape,
    /// Dtype the source keeps its normalization weights in. GGUF says f32 and
    /// the kernels read f16, so the conversion happens on upload — and a source
    /// that says something else is refused rather than reinterpreted.
    norm_weights: DType,
}

impl CudaExec {
    /// Loads the kernel catalogue and allocates everything a step writes into.
    /// No weights yet — those arrive through `WeightStore`.
    pub fn new(device: Arc<dyn Device>, spec: ExecSpec) -> Result<Self> {
        let shape = spec.shape;
        if shape.vocab as usize > crate::launchers::SAMPLE_MAX_VOCAB {
            return Err(ForgeError::Unsupported(format!(
                "słownik {} przekracza pojemność wyboru {}",
                shape.vocab,
                crate::launchers::SAMPLE_MAX_VOCAB
            )));
        }
        let kernels = Kernels::load(device.clone())?;
        let stream = device.create_stream()?;

        let n = MAX_ROWS;
        let f16b =
            |elems: u32| device.alloc(elems as usize * 2, MemKind::Device, Pool::Activations);
        let f32b =
            |elems: u32| device.alloc(elems as usize * 4, MemKind::Device, Pool::Activations);
        let i32b =
            |elems: u32| device.alloc(elems as usize * 4, MemKind::Device, Pool::Activations);
        let per_lane = SEQ_CAP.div_ceil(PAGE) as usize;
        let scratch = Scratch {
            h: f16b(n * shape.hidden)?,
            h32: f32b(n * shape.hidden)?,
            norm: f16b(n * shape.hidden)?,
            q: f16b(n * shape.attn_width())?,
            k: f16b(n * shape.kv_width())?,
            v: f16b(n * shape.kv_width())?,
            attn: f16b(n * shape.attn_width())?,
            attn_gate: f16b(n * shape.attn_width())?,
            proj: f16b(n * shape.hidden)?,
            gate: f16b(n * shape.inter)?,
            up: f16b(n * shape.inter)?,
            act: f16b(n * shape.inter)?,
            // Logity liczymy tylko dla ostatniego tokenu każdego lane'a, więc
            // wiersz na lane — słownik razy kafel to megabajty, z których
            // czytamy 1/n.
            logits: i32b(MAX_LANES * shape.vocab)?,
            ids: i32b(n)?,
            positions: i32b(n)?,
            bases: i32b(MAX_LANES)?,
            pages: i32b(MAX_LANES * per_lane as u32)?,
            lengths: i32b(MAX_LANES)?,
            parts: i32b(MAX_LANES * shape.heads * DECODE_SPLITS * (shape.head_dim + 4))?,
            choice: i32b(MAX_LANES.max(2))?,
            sample_vals: i32b(SAMPLE_SCRATCH_PAIRS as u32)?,
            sample_idx: i32b(SAMPLE_SCRATCH_PAIRS as u32)?,
        };

        // One pinned mirror per control buffer, each as wide as the widest of
        // them: five allocations of a few kilobytes, made once, so that no step
        // ever copies out of pageable memory.
        let control_i32 = (n).max(MAX_LANES * per_lane as u32) as usize;
        let control_fence = device.create_event()?;
        let stage_host: [control::Staging; 5] = {
            let mut made = Vec::with_capacity(5);
            for _ in 0..5 {
                made.push(control::Staging::new(&*device, control_i32)?);
            }
            made.try_into()
                .map_err(|_| ForgeError::Other("pięć buforów sztaplowania".into()))?
        };

        // Only the layers that ATTEND get a slab. A page costs its bytes in
        // every allocated layer at once, so a hybrid stack sized by
        // `shape.layers` would need four times the pool to reach the same
        // context — measured on Qwen3.6: 1.28 GiB against 320 MiB.
        if spec.attends.len() != shape.layers as usize {
            return Err(ForgeError::Unsupported(format!(
                "maska uwagi ma {} warstw, a kształt mówi {}",
                spec.attends.len(),
                shape.layers
            )));
        }
        let kv_layers = spec.attends.iter().filter(|a| **a).count();
        if kv_layers == 0 {
            return Err(ForgeError::Unsupported(
                "żadna warstwa nie ma uwagi — ten wykonawca nie ma czego stronicować".into(),
            ));
        }
        let page_bytes = (shape.kv_heads * PAGE * shape.head_dim) as usize * 2;
        let cfg = KvConfig {
            n_layers: kv_layers,
            n_kv_heads: shape.kv_heads as usize,
            head_dim: shape.head_dim as usize,
            page_size: PAGE as usize,
            n_pages: page_budget(&*device, page_bytes, kv_layers, per_lane)?,
            max_pages_per_seq: per_lane,
            // Pełna wierność. Kwantyzacja KV jest w tym cache'u od dawna i to
            // jest właśnie powód, dla którego ten wykonawca go bierze zamiast
            // hodować własny — ale włączenie jej to osobny pomiar jakości.
            quant: KvQuant::F16,
        };
        let kv = KvCache::new_mapped(
            &*device,
            cfg,
            KvLayerMap::from_attention_mask(spec.attends.iter().copied()),
        )?;
        let seqs = (0..MAX_LANES).map(|_| kv.new_seq()).collect();
        // Sized even for a model with no recurrent layer, because the config is
        // the only thing allocated here — the slabs arrive layer by layer, as
        // the operations that need them do.
        let recurrent = RecurrentState::new(RecurrentConfig {
            slots: MAX_LANES as usize,
            conv_channels: spec.ssm.map(|s| s.mixed_width() as usize).unwrap_or(1),
            conv_taps: spec.ssm.map(|s| s.d_conv as usize).unwrap_or(2),
            v_heads: spec.ssm.map(|s| s.v_heads as usize).unwrap_or(1),
            d_state: spec.ssm.map(|s| s.d_state as usize).unwrap_or(1),
        })?;

        Ok(Self {
            device,
            kernels,
            stream,
            weights: Vec::new(),
            moe: RefCell::new(None),
            expert_tables: RefCell::new(HashMap::new()),
            fp8: RefCell::new(HashMap::new()),
            delta: RefCell::new(None),
            recurrent: RefCell::new(recurrent),
            ssm: spec.ssm,
            kv: RefCell::new(kv),
            seqs: RefCell::new(seqs),
            scratch,
            staged: std::array::from_fn(|_| RefCell::new(Vec::new())),
            stage_host,
            control_fence,
            fence_live: Cell::new(false),
            graphs: RefCell::new(HashMap::new()),
            warmed: RefCell::new(HashSet::new()),
            shape,
            norm_weights: spec.norm_weights,
        })
    }
}

/// How many pages the KV pool can carry, capped by what the lanes could ever
/// use.
///
/// Asked of the pool rather than assumed, because the answer decides whether a
/// long sequence runs or is refused — and reserving `lanes * seq_cap` up front
/// would be the very thing paging exists to avoid.
fn page_budget(
    device: &dyn Device,
    page_bytes: usize,
    layers: usize,
    per_lane: usize,
) -> Result<usize> {
    let want = per_lane * MAX_LANES as usize;
    // Jedna strona kosztuje tyle we WSZYSTKICH warstwach naraz i po obu
    // stronach cache'u — inaczej budżet mówiłby o czymś, czego się nie
    // alokuje.
    let each = page_bytes
        .checked_mul(layers * 2)
        .ok_or_else(|| ForgeError::Device("przepełnienie budżetu stron KV".into()))?;
    let pages = match device.pool_available(Pool::KvCache) {
        Some(free) => want.min(free / each),
        None => want,
    };
    if pages < per_lane {
        return Err(ForgeError::OutOfMemory {
            requested: per_lane * each,
            available: pages * each,
        });
    }
    Ok(pages)
}

impl WeightStore for CudaExec {
    /// The blocks go up as they came off disk.
    ///
    /// A source that only offers the affine triple is refused rather than
    /// repacked: packing six-bit weights back into nibbles is lossy, and a
    /// quantized model that quietly loses two bits per weight still produces
    /// fluent text.
    fn put_quant(&mut self, w: QuantWeight) -> Result<WeightId> {
        let QuantWeight::Packed(mut w) = w else {
            return Err(ForgeError::Unsupported(
                "kernele CUDA czytają bloki źródła, a to źródło oddaje wyłącznie \
                 postać afiniczną"
                    .into(),
            ));
        };
        if !Self::knows(w.quant) {
            return Err(ForgeError::Unsupported(format!(
                "{:?}: ten wykonawca nie ma dla niego kerneli",
                w.quant
            )));
        }
        // Cała dzisiejsza tabela czyta bloki. Waga w innym układzie zatrzyma
        // się tutaj, a nie trafi na kernel, który przeczyta jej bajty jak bloki.
        if w.layout != Layout::Blocks {
            return Err(ForgeError::Unsupported(format!(
                "{:?} w układzie {:?}, a ta tabela czyta bloki",
                w.quant, w.layout
            )));
        }
        // Waga niekwantyzowana JEST formatem — jej dtype jest jej formatem.
        // Kernele czytają f16, więc bf16 zwęża się tutaj, raz.
        let codes = if w.quant == QuantKind::None {
            to_f16_bytes(&w.planes.codes, w.dtype)?
        } else {
            std::mem::take(&mut w.planes.codes)
        };
        // Sekcja skalowana ŻĄDA skalara, pozostałe go nie przyjmują. Kernel,
        // który dostałby jedynkę zamiast prawdziwej skali, policzyłby wagi
        // mniejsze o stały czynnik — czyli model, który mówi, tylko nie to.
        let output_scale = if Self::scaled(w.quant) {
            w.global()?
        } else if w.planes.global.is_some() {
            return Err(ForgeError::Unsupported(format!(
                "{:?} niesie skalar tensora, a jego kernele go nie biorą",
                w.quant
            )));
        } else {
            1.0
        };
        // Wiersz krótszy niż blok adresowałby cudzy blok, więc sprawdzane TU,
        // przy wgraniu, a nie zakładane przy każdym mnożeniu.
        if !w.cols.is_multiple_of(w.quant.block_elems()) {
            return Err(ForgeError::Unsupported(format!(
                "{} kolumn nie dzieli się na bloki {:?} po {}",
                w.cols,
                w.quant,
                w.quant.block_elems()
            )));
        }
        // Każdy wiersz DZISIEJSZEJ tabeli czyta skale z wnętrza bloku. Waga,
        // która niesie je osobno, zatrzymuje się tutaj — zamiast trafić na
        // kernel, który tej płaszczyzny nie dostanie i policzy bez niej.
        if w.planes.scales.is_some() {
            return Err(ForgeError::Unsupported(format!(
                "{:?} niesie skale poza kodami, a ta tabela ich nie wiąże",
                w.quant
            )));
        }
        self.weights.push(Weight::Quant(Quantized {
            blocks: upload(&*self.device, &codes)?,
            output_scale,
            quant: w.quant,
            rows: w.rows,
            cols: w.cols,
        }));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }

    /// Normalization weights, narrowed to the f16 `rmsnorm_f16` reads.
    ///
    /// GGUF keeps these in f32 and the quantization scales in f16 — two
    /// different dtypes in one file, which is why `ExecSpec` carries them
    /// separately and why this reads the one meant for norms.
    fn put_plain(&mut self, bytes: Vec<u8>) -> Result<WeightId> {
        let halves: Vec<u8> = match self.norm_weights {
            DType::F32 => bytes
                .chunks_exact(4)
                .flat_map(|c| {
                    f16::from_f32(f32::from_le_bytes([c[0], c[1], c[2], c[3]])).to_le_bytes()
                })
                .collect(),
            DType::F16 => bytes,
            DType::BF16 => bytes
                .chunks_exact(2)
                .flat_map(|c| {
                    f16::from_f32(half::bf16::from_le_bytes([c[0], c[1]]).to_f32()).to_le_bytes()
                })
                .collect(),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "waga normy ma typ {other:?}, a kernel czyta f16"
                )))
            }
        };
        let buf = upload(&*self.device, &halves)?;
        self.weights.push(Weight::Plain(buf));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }
}

impl Executor for CudaExec {
    fn run_step(&self, ops: &[Op]) -> Result<()> {
        self.run_whole_step(ops)
    }

    fn run(&self, op: &Op) -> Result<()> {
        let step = match op {
            Op::Embed { step, .. }
            | Op::RmsNorm { step, .. }
            | Op::MatMul { step, .. }
            | Op::HeadNorm { step, .. }
            | Op::Rope { step, .. }
            | Op::KvAppend { step, .. }
            | Op::Attention { step, .. }
            | Op::SiluMul { step }
            | Op::SigmoidMul { step, .. }
            | Op::DeltaNet { step, .. }
            | Op::MoeFfn { step, .. }
            | Op::FusedNormMatMul { step, .. }
            | Op::FusedMatMulResidual { step, .. }
            | Op::Residual { step, .. }
            | Op::LogitsOfLast { step, .. } => step,
        };
        self.admit(step)?;
        match op {
            Op::Embed { table, tokens, .. } => {
                self.op_embed(*table, tokens)?;
                self.sync_h32(step.rows())
            }
            Op::RmsNorm { out, x, w, .. } => self.op_rmsnorm(*out, *x, *w, step),
            Op::MatMul { out, w, x, .. } => {
                self.matmul(self.buf(*out), *w, self.buf(*x), step.rows())
            }
            Op::FusedNormMatMul {
                out,
                w,
                norm_w,
                x,
                step,
            } => {
                let quant = self.quant(*w)?;
                if Self::fusable(step) && Self::fused_quant(quant.quant) {
                    self.gemv_norm_by_kind(
                        quant,
                        self.buf(*out),
                        self.buf(*x),
                        self.plain(*norm_w)?,
                    )
                } else {
                    self.run(&Op::RmsNorm {
                        out: Act::Norm,
                        x: *x,
                        w: *norm_w,
                        step: step.clone(),
                    })?;
                    self.run(&Op::MatMul {
                        out: *out,
                        w: *w,
                        x: Act::Norm,
                        step: step.clone(),
                    })
                }
            }
            Op::FusedMatMulResidual { w, x, step } => {
                let quant = self.quant(*w)?;
                if Self::fusable(step) && Self::fused_quant(quant.quant) {
                    self.gemv_residual_by_kind(quant, self.buf(*x))
                } else {
                    self.run(&Op::MatMul {
                        out: Act::Proj,
                        w: *w,
                        x: *x,
                        step: step.clone(),
                    })?;
                    self.run(&Op::Residual {
                        src: Act::Proj,
                        step: step.clone(),
                    })
                }
            }
            Op::HeadNorm { act, w, heads, .. } => self.op_head_norm(*act, *w, *heads, step),
            Op::Rope { act, heads, .. } => self.op_rope(*act, *heads, step),
            Op::SigmoidMul { act, gate, .. } => self.kernels.sigmoid_mul_f16(
                self.buf(*act),
                self.buf(*act),
                self.buf(*gate),
                step.rows() as usize * self.shape.attn_width() as usize,
                &self.stream,
            ),
            Op::KvAppend { layer, .. } => self.op_kv_append(*layer, step),
            Op::Attention { layer, .. } => self.op_attention(*layer, step),
            Op::SiluMul { .. } => self.kernels.glu_mul_f16(
                FfnActivation::SiLU,
                &self.scratch.act,
                &self.scratch.gate,
                &self.scratch.up,
                step.rows() as usize * self.shape.inter as usize,
                &self.stream,
            ),
            Op::MoeFfn {
                out,
                x,
                router,
                gate,
                up,
                down,
                experts,
                top_k,
                norm_topk,
                shared,
                ..
            } => self.op_moe_ffn(
                *out,
                *x,
                [*router, *gate, *up, *down],
                *experts,
                *top_k,
                *norm_topk,
                shared.as_ref(),
                step,
            ),
            Op::DeltaNet {
                out, x, layer, w, ..
            } => self.op_delta_net(*out, *x, *layer, w, step),
            Op::Residual { src, .. } => {
                self.kernels.residual_add_f16(
                    &self.scratch.h,
                    self.buf(*src),
                    step.rows() as usize * self.shape.hidden as usize,
                    &self.stream,
                )?;
                self.sync_h32(step.rows())
            }
            Op::LogitsOfLast { w, x, .. } => self.op_logits_of_last(*w, *x, step),
        }
    }

    fn sync(&self) -> Result<()> {
        self.stream.synchronize()
    }

    fn read(&self, act: Act, len: usize) -> Result<Vec<f32>> {
        self.stream.synchronize()?;
        if act == Act::Logits {
            let mut raw = vec![0u8; len * 4];
            self.device.read(self.buf(act), 0, &mut raw)?;
            return Ok(raw
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect());
        }
        let mut raw = vec![0u8; len * 2];
        self.device.read(self.buf(act), 0, &mut raw)?;
        Ok(raw
            .chunks_exact(2)
            .map(|c| f16::from_bits(u16::from_le_bytes([c[0], c[1]])).to_f32())
            .collect())
    }

    /// Greedy choice on the device, so the vocabulary never crosses the bus
    /// just to be scanned for its maximum.
    fn argmax(&self, act: Act, lanes: usize) -> Result<Vec<u32>> {
        let vocab = self.shape.vocab as usize;
        if lanes == 1 {
            self.kernels.sample_argmax_f32(
                &self.scratch.choice,
                &self.scratch.sample_vals,
                &self.scratch.sample_idx,
                self.buf(act),
                vocab,
                &self.stream,
            )?;
        } else {
            self.kernels.sample_batched_argmax_f32(
                &self.scratch.choice,
                self.buf(act),
                lanes,
                vocab,
                &self.stream,
            )?;
        }
        self.stream.synchronize()?;
        let mut raw = vec![0u8; lanes * 4];
        self.device.read(&self.scratch.choice, 0, &mut raw)?;
        raw.chunks_exact(4)
            .map(|c| {
                let id = i32::from_le_bytes(c.try_into().unwrap());
                u32::try_from(id)
                    .map_err(|_| ForgeError::Other(format!("wybór zwrócił token {id}")))
            })
            .collect()
    }

    fn seq_cap(&self) -> u32 {
        SEQ_CAP
    }

    fn tile(&self) -> Tile {
        Tile {
            max_tokens: MAX_ROWS,
            max_lanes: MAX_LANES,
            // Kernele biorą liczbę wierszy jako argument i same domykają ogon,
            // więc nie ma kształtu, do którego warto by prompt dociągać.
            align: 1,
        }
    }
}

impl CudaExec {
    fn buf(&self, a: Act) -> &DevBuffer {
        match a {
            Act::Hidden => &self.scratch.h,
            Act::Norm => &self.scratch.norm,
            Act::Query => &self.scratch.q,
            Act::Key => &self.scratch.k,
            Act::Value => &self.scratch.v,
            Act::Attn => &self.scratch.attn,
            Act::AttnGate => &self.scratch.attn_gate,
            Act::Proj => &self.scratch.proj,
            Act::Gate => &self.scratch.gate,
            Act::Up => &self.scratch.up,
            Act::Activated => &self.scratch.act,
            Act::Logits => &self.scratch.logits,
        }
    }

    /// Checks a step against what this executor actually holds.
    ///
    /// In one place and before anything runs, because the alternative is each
    /// kernel discovering its own half of the problem: the scratch would clip
    /// the rows, the page table would address a lane that has none, and the
    /// answer would come out finished and wrong.
    fn admit(&self, step: &Step) -> Result<()> {
        if step.rows() > MAX_ROWS {
            return Err(ForgeError::Unsupported(format!(
                "{} lane'ów po {} tokenów to {} wierszy, a scratch ma {MAX_ROWS}",
                step.lanes().len(),
                step.tokens(),
                step.rows()
            )));
        }
        for lane in step.lanes() {
            if lane.slot >= MAX_LANES {
                return Err(ForgeError::Unsupported(format!(
                    "slot {}, a wykonawca trzyma {MAX_LANES}",
                    lane.slot
                )));
            }
            if lane.pos + step.tokens() > SEQ_CAP {
                return Err(ForgeError::Unsupported(format!(
                    "slot {} sięga pozycji {}, a cache trzyma {SEQ_CAP}",
                    lane.slot,
                    lane.pos + step.tokens()
                )));
            }
        }
        Ok(())
    }

    fn quant(&self, id: WeightId) -> Result<&Quantized> {
        match self.weights.get(id.0 as usize) {
            Some(Weight::Quant(q)) => Ok(q),
            _ => Err(ForgeError::Other(format!(
                "waga {} nie jest kwantyzowana",
                id.0
            ))),
        }
    }

    fn plain(&self, id: WeightId) -> Result<&DevBuffer> {
        match self.weights.get(id.0 as usize) {
            Some(Weight::Plain(b)) => Ok(b),
            _ => Err(ForgeError::Other(format!("waga {} nie jest zwykła", id.0))),
        }
    }

    fn sync_h32(&self, rows: u32) -> Result<()> {
        self.kernels.cast_f16_f32(
            &self.scratch.h32,
            &self.scratch.h,
            rows as usize * self.shape.hidden as usize,
            &self.stream,
        )
    }

    fn op_embed(&self, table: WeightId, tokens: &[u32]) -> Result<()> {
        let w = self.quant(table)?;
        // Zbieranie wierszy ma kernel NA FORMAT, a nie jeden na wszystkie —
        // tablica osadzeń w formacie bez takiego kernela zatrzyma się tutaj, bo
        // jej wiersza nie ma czym przeczytać. NVFP4 z compressed-tensors nie
        // kwantyzuje osadzeń, więc przychodzą jako f16 i idą drugą gałęzią.
        match w.quant {
            QuantKind::Q4K => self.kernels.gather_q4_k_rows_f16(
                &self.scratch.h,
                &w.blocks,
                &self.scratch.ids,
                tokens.len(),
                w.rows,
                w.cols,
                &self.stream,
            ),
            QuantKind::Q8_0 => self.kernels.gather_q8_0_rows_f16(
                &self.scratch.h,
                &w.blocks,
                &self.scratch.ids,
                tokens.len(),
                w.rows,
                w.cols,
                &self.stream,
            ),
            QuantKind::None => self.kernels.gather_rows_f16(
                &self.scratch.h,
                &w.blocks,
                &self.scratch.ids,
                tokens.len(),
                w.cols,
                &self.stream,
            ),
            other => Err(ForgeError::Unsupported(format!(
                "tablica osadzeń jest {other:?}, a jej wierszy nie ma czym zebrać"
            ))),
        }
    }

    fn op_rmsnorm(&self, out: Act, x: Act, w: WeightId, step: &Step) -> Result<()> {
        self.kernels.rmsnorm_f16(
            self.buf(out),
            self.buf(x),
            self.plain(w)?,
            step.rows() as usize,
            self.shape.hidden as usize,
            self.shape.eps,
            &self.stream,
        )
    }

    /// RMS normalization with HEADS AS ROWS, in place.
    ///
    /// The same kernel as the ordinary norm — the only difference is how many
    /// rows it splits the buffer into. Input and output are one buffer, since
    /// every block reads and writes only its own row.
    ///
    /// Not to be confused with the catalogue's `rmsnorm_head_f16`: that one
    /// carries NO weight and belongs to DeepSeek V4's second Q norm.
    fn op_head_norm(&self, act: Act, w: WeightId, heads: u32, step: &Step) -> Result<()> {
        let buf = self.buf(act);
        self.kernels.rmsnorm_f16(
            buf,
            buf,
            self.plain(w)?,
            step.rows() as usize * heads as usize,
            self.shape.head_dim as usize,
            self.shape.eps,
            &self.stream,
        )
    }

    fn op_rope(&self, act: Act, heads: u32, step: &Step) -> Result<()> {
        // A partial rotary is a DIFFERENT kernel, not the same one told to stop
        // early: it pairs dimension j with j + rot/2 and derives the frequency
        // from `rot`, so running the full kernel over a narrower slice would
        // pair the wrong dimensions at the wrong frequencies.
        if self.shape.rope_partial() {
            return self.kernels.rope_neox_partial_f16(
                self.buf(act),
                &self.scratch.positions,
                step.rows() as usize,
                heads as usize,
                self.shape.head_dim as usize,
                self.shape.rope_rot as usize,
                self.shape.rope_theta,
                &self.stream,
            );
        }
        self.kernels.rope_neox_f16(
            self.buf(act),
            &self.scratch.positions,
            step.rows() as usize,
            heads as usize,
            self.shape.head_dim as usize,
            self.shape.rope_theta,
            None,
            &self.stream,
        )
    }

    /// Klucz i wartość kroku na swoje strony w cache'u warstwy.
    fn op_kv_append(&self, layer: usize, step: &Step) -> Result<()> {
        let s = self.shape;
        // Uchwyt cache'u żyje do końca uruchomienia, bo kernel czyta jego
        // bufory; `map_pages` skończyło swoje zapożyczenie wyżej.
        let kv = self.kv.borrow();
        let per_lane = kv.cfg.max_pages_per_seq;
        let (kc, vc) = layer_slabs(&kv, layer)?;
        self.kernels.kv_append_batch_segmented_f16(
            kc,
            vc,
            &self.scratch.k,
            &self.scratch.v,
            &self.scratch.pages,
            &self.scratch.bases,
            step.lanes().len(),
            step.tokens() as usize,
            per_lane,
            s.kv_heads as usize,
            PAGE as usize,
            s.head_dim as usize,
            &self.stream,
        )
    }

    /// Uwaga nad cache'em warstwy.
    ///
    /// Dwa kształty, dwa kernele, i podział przebiega tam, gdzie przebiega
    /// naprawdę: krok dekodowania to JEDNO zapytanie na lane nad długim
    /// kontekstem i liczy wszystkie lane'y jednym uruchomieniem, a kafel
    /// prefillu to wiele zapytań jednej sekwencji i liczy je z maską
    /// przyczynową.
    fn op_attention(&self, layer: usize, step: &Step) -> Result<()> {
        let s = self.shape;
        let kv = self.kv.borrow();
        let per_lane = kv.cfg.max_pages_per_seq;
        let (kc, vc) = layer_slabs(&kv, layer)?;
        if step.tokens() == 1 {
            return self.kernels.attn_decode_f16(
                &self.scratch.attn,
                &self.scratch.parts,
                &self.scratch.q,
                kc,
                vc,
                &self.scratch.pages,
                &self.scratch.lengths,
                step.lanes().len(),
                s.heads as usize,
                s.kv_heads as usize,
                s.head_dim as usize,
                PAGE as usize,
                per_lane,
                s.attn_scale(),
                0,
                &self.stream,
            );
        }
        for (lane, l) in step.lanes().iter().enumerate() {
            let rows = step.tokens() as usize;
            let (q, out) = (
                self.lane_rows(&self.scratch.q, lane, rows)?,
                self.lane_rows(&self.scratch.attn, lane, rows)?,
            );
            self.kernels.attn_prefill(
                &out,
                &q,
                kc,
                vc,
                &self.lane_pages(lane)?,
                l.pos as usize,
                rows,
                s.heads as usize,
                s.kv_heads as usize,
                s.head_dim as usize,
                PAGE as usize,
                DType::F16,
                s.attn_scale(),
                0,
                &self.stream,
            )?;
        }
        Ok(())
    }

    /// The output head, for the LAST token of every lane.
    ///
    /// One GEMV per lane, which is what the engine's own batched head does for
    /// these formats: the Q4_K/Q6_K kernels have no row-count-agnostic batched
    /// form, and inventing one here would be a kernel choice made without a
    /// measurement.
    fn op_logits_of_last(&self, w: WeightId, x: Act, step: &Step) -> Result<()> {
        let weight = self.quant(w)?;
        let tokens = step.tokens() as usize;
        // Wsad dekodowania ma ostatnie tokeny wszystkich lane'ów OBOK SIEBIE,
        // więc głowa może przemiatać swoje 0,9 GiB raz dla całego wsadu zamiast
        // raz na lane. Ten wariant istnieje tylko dla Q6_K — a głowa Q4_K_M
        // jest właśnie Q6_K.
        if tokens == 1
            && weight.quant == QuantKind::Q6K
            && self.kernels.gemv_q6_k_dp4a_batch_out_f32_at(
                &self.scratch.logits,
                0,
                &weight.blocks,
                0,
                self.buf(x),
                0,
                weight.rows,
                weight.cols,
                step.lanes().len(),
                &self.stream,
            )?
        {
            return Ok(());
        }
        for lane in 0..step.lanes().len() {
            let x_off = (lane * tokens + tokens - 1) * self.shape.hidden as usize * 2;
            let y_off = lane * self.shape.vocab as usize * 4;
            // Q4_K i Q6_K mają wariant z przesunięciami, więc nie płacą za
            // pod-bufory na ścieżce, którą naprawdę chodzimy; reszta formatów
            // dostaje ten sam wiersz przez uchwyt na wycinek.
            match weight.quant {
                QuantKind::Q4K => self.kernels.gemv_q4_k_out_f32(
                    &self.scratch.logits,
                    y_off,
                    &weight.blocks,
                    self.buf(x),
                    x_off,
                    weight.rows,
                    weight.cols,
                    &self.stream,
                )?,
                QuantKind::Q6K => self.kernels.gemv_q6_k_out_f32(
                    &self.scratch.logits,
                    y_off,
                    &weight.blocks,
                    self.buf(x),
                    x_off,
                    weight.rows,
                    weight.cols,
                    &self.stream,
                )?,
                QuantKind::Q8_0 => {
                    let y = self
                        .device
                        .sub_buffer(&self.scratch.logits, y_off, weight.rows * 4)?;
                    let row = self
                        .device
                        .sub_buffer(self.buf(x), x_off, weight.cols * 2)?;
                    if weight.cols <= Kernels::DP4A_MAX_COLS {
                        self.kernels.gemv_q8_0_dp4a_out_f32(
                            &y,
                            &weight.blocks,
                            &row,
                            weight.rows,
                            weight.cols,
                            &self.stream,
                        )?;
                    } else {
                        self.kernels.gemv_q8_0_out_f32(
                            &y,
                            &weight.blocks,
                            &row,
                            weight.rows,
                            weight.cols,
                            &self.stream,
                        )?;
                    }
                }
                quant => {
                    let y = self
                        .device
                        .sub_buffer(&self.scratch.logits, y_off, weight.rows * 4)?;
                    let row = self
                        .device
                        .sub_buffer(self.buf(x), x_off, weight.cols * 2)?;
                    self.gemv_out_f32_by_kind(
                        quant,
                        &y,
                        &weight.blocks,
                        &row,
                        weight.rows,
                        weight.cols,
                        weight.output_scale,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// One lane's rows of an activation buffer, as a handle of their own.
    ///
    /// The width is the ATTENTION width, since this is only ever called on `q`
    /// and `attn`. For llama it equals `hidden`, which is why the confusion did
    /// not hurt.
    fn lane_rows(&self, buf: &DevBuffer, lane: usize, rows: usize) -> Result<DevBuffer> {
        let width = self.shape.attn_width() as usize * 2;
        self.device
            .sub_buffer(buf, lane * rows * width, rows * width)
    }

    /// Formats for which a fused kernel exists at all.
    fn fused_quant(quant: QuantKind) -> bool {
        matches!(
            quant,
            QuantKind::None | QuantKind::Q4K | QuantKind::Q6K | QuantKind::Q8_0
        )
    }

    /// Whether the step is one the fused kernels can take.
    ///
    /// The condition is ONE ACTIVATION ROW, and it is deliberately not "one
    /// token": every fused form here is a GEMV, so it computes a single row no
    /// matter how many the step carries. Four lanes of one token each is one
    /// token per lane and four rows — asking a vector kernel for it computes
    /// lane zero and leaves the other three holding the previous step, which
    /// reads as one sequence answering another's prompt rather than as a
    /// failure. That is the exact confusion `Step` exists to prevent, so the
    /// row count is asked for by name and in one place.
    fn fusable(step: &Step) -> bool {
        step.rows() == 1
    }

    fn gemv_norm_by_kind(
        &self,
        w: &Quantized,
        y: &DevBuffer,
        h: &DevBuffer,
        norm_w: &DevBuffer,
    ) -> Result<()> {
        let rows = w.rows;
        let cols = w.cols;
        let ss_from_h16 = false;
        let dp4a = cols <= Kernels::DP4A_MAX_COLS;
        match w.quant {
            QuantKind::None => self.kernels.gemv_norm_f16(
                y,
                &w.blocks,
                h,
                &self.scratch.h32,
                norm_w,
                rows,
                cols,
                ss_from_h16,
                self.shape.eps,
                &self.stream,
            ),
            QuantKind::Q4K if dp4a => self.kernels.gemv_norm_q4_k_dp4a_f16(
                y,
                &w.blocks,
                h,
                &self.scratch.h32,
                norm_w,
                rows,
                cols,
                ss_from_h16,
                self.shape.eps,
                &self.stream,
            ),
            QuantKind::Q4K => self.kernels.gemv_norm_q4_k_f16(
                y,
                &w.blocks,
                h,
                &self.scratch.h32,
                norm_w,
                rows,
                cols,
                ss_from_h16,
                self.shape.eps,
                &self.stream,
            ),
            QuantKind::Q6K if dp4a => self.kernels.gemv_norm_q6_k_dp4a_f16(
                y,
                &w.blocks,
                h,
                &self.scratch.h32,
                norm_w,
                rows,
                cols,
                ss_from_h16,
                self.shape.eps,
                &self.stream,
            ),
            QuantKind::Q6K => self.kernels.gemv_norm_q6_k_f16(
                y,
                &w.blocks,
                h,
                &self.scratch.h32,
                norm_w,
                rows,
                cols,
                ss_from_h16,
                self.shape.eps,
                &self.stream,
            ),
            QuantKind::Q8_0 if dp4a => self.kernels.gemv_norm_q8_0_dp4a_f16(
                y,
                &w.blocks,
                h,
                &self.scratch.h32,
                norm_w,
                rows,
                cols,
                ss_from_h16,
                self.shape.eps,
                &self.stream,
            ),
            QuantKind::Q8_0 => self.kernels.gemv_norm_q8_0_f16(
                y,
                &w.blocks,
                h,
                &self.scratch.h32,
                norm_w,
                rows,
                cols,
                ss_from_h16,
                self.shape.eps,
                &self.stream,
            ),
            other => Err(ForgeError::Unsupported(format!(
                "{other:?}: brak scalonego GEMV norm"
            ))),
        }
    }

    fn gemv_residual_by_kind(&self, w: &Quantized, x: &DevBuffer) -> Result<()> {
        let dp4a = w.cols <= Kernels::DP4A_MAX_COLS;
        match w.quant {
            QuantKind::None => self.kernels.gemv_residual_f16(
                &self.scratch.h,
                &self.scratch.h32,
                &w.blocks,
                x,
                w.rows,
                w.cols,
                &self.stream,
            ),
            QuantKind::Q4K if dp4a => self.kernels.gemv_residual_q4_k_dp4a_f16(
                &self.scratch.h,
                &self.scratch.h32,
                &w.blocks,
                x,
                w.rows,
                w.cols,
                &self.stream,
            ),
            QuantKind::Q4K => self.kernels.gemv_residual_q4_k_f16(
                &self.scratch.h,
                &self.scratch.h32,
                &w.blocks,
                x,
                w.rows,
                w.cols,
                &self.stream,
            ),
            QuantKind::Q6K if dp4a => self.kernels.gemv_residual_q6_k_dp4a_f16(
                &self.scratch.h,
                &self.scratch.h32,
                &w.blocks,
                x,
                w.rows,
                w.cols,
                &self.stream,
            ),
            QuantKind::Q6K => self.kernels.gemv_residual_q6_k_f16(
                &self.scratch.h,
                &self.scratch.h32,
                &w.blocks,
                x,
                w.rows,
                w.cols,
                &self.stream,
            ),
            QuantKind::Q8_0 if dp4a => self.kernels.gemv_residual_q8_0_dp4a_f16(
                &self.scratch.h,
                &self.scratch.h32,
                &w.blocks,
                x,
                w.rows,
                w.cols,
                &self.stream,
            ),
            QuantKind::Q8_0 => self.kernels.gemv_residual_q8_0_f16(
                &self.scratch.h,
                &self.scratch.h32,
                &w.blocks,
                x,
                w.rows,
                w.cols,
                &self.stream,
            ),
            other => Err(ForgeError::Unsupported(format!(
                "{other:?}: brak scalonego GEMV residual"
            ))),
        }
    }

    /// `out = x * wagaᵀ`, in the form this row count wants.
    ///
    /// Three forms, and the split is between DECODING and a PROMPT rather than
    /// between one row and many:
    ///
    ///   * one row — the vector form, weight-stationary, activations quantized
    ///     to int8;
    ///   * a handful of rows — a decode BATCH: one sweep of the weights serves
    ///     every lane, same int8 activations;
    ///   * many rows — a prompt tile, activations in f16.
    ///
    /// The first two share an arithmetic on purpose. The tile quantizes
    /// differently, so a lane decoded alone and the same lane decoded in a
    /// batch would otherwise disagree by 2,5% of the logit spread and pick the
    /// other token on a tie; sharing it brings that to 0,38%.
    ///
    /// The prompt tile is a bad kernel for a decode batch and that had to be
    /// measured rather than assumed: at three lanes on a DGX Spark the batch
    /// through `gemm_q4_k_f16` was SLOWER (17,2 tok/s together) than three
    /// separate sequences, because that tile is built for hundreds of rows and
    /// four cannot fill its waves. The batch form knows widths 2/4/8/16 and
    /// refuses outside them, so three lanes fall back to the tile.
    fn matmul(&self, out: &DevBuffer, id: WeightId, x: &DevBuffer, rows: u32) -> Result<()> {
        let w = self.quant(id)?;
        // A prompt-width step multiplies through the e4m3 form when this weight
        // has one; see `fp8.rs` for why the second form exists at all.
        if self.fp8_matmul(id, w, out, x, rows)? {
            return Ok(());
        }
        let (r, c) = (w.rows, w.cols);
        // Aktywacja idzie do int8 w blokach po 32, a kernel adresuje te bloki
        // typem, który kończy się na tej szerokości. Szersza projekcja zostaje
        // przy f16 — wolniej, ale to jedyna z tych trzech form, która nie ma
        // granicy kolumn.
        let int8_fits = c <= Kernels::DP4A_MAX_COLS;
        // Q4_K, Q6_K and Q8_0 have dedicated int8 activation kernels for
        // decode; other formats use their table entry.
        let dp4a =
            matches!(w.quant, QuantKind::Q4K | QuantKind::Q6K | QuantKind::Q8_0) && int8_fits;
        if rows == 1 {
            return self.gemv_decode(w, out, x, r);
        }
        if matches!(w.quant, QuantKind::Q4K | QuantKind::Q6K)
            && dp4a
            && self.kernels.gemm_qk_dp4a_batch_at(
                out,
                &w.blocks,
                0,
                x,
                r,
                c,
                rows as usize,
                w.quant != QuantKind::Q4K,
                &self.stream,
            )?
        {
            return Ok(());
        }
        self.gemm_by_kind(
            w.quant,
            out,
            &w.blocks,
            0,
            x,
            r,
            c,
            rows as usize,
            w.output_scale,
        )
    }
}

/// Slaby K i V jednej warstwy modelu.
///
/// Przez mapę warstw cache'u, a nie przez indeks wprost: architektura, w której
/// tylko część warstw ma uwagę, trzyma zwarty slab i pomyłka o ten jeden krok
/// czytałaby cudzą warstwę.
fn layer_slabs<'a>(kv: &'a KvCache, layer: usize) -> Result<(&'a DevBuffer, &'a DevBuffer)> {
    let index = kv
        .layer_index(layer)
        .ok_or_else(|| ForgeError::Other(format!("warstwa {layer} nie ma cache'u KV")))?;
    Ok((&kv.k[index], &kv.v[index]))
}

/// Bajty wagi niekwantyzowanej w f16, w którym czytają je kernele.
fn to_f16_bytes(bytes: &[u8], dtype: DType) -> Result<Vec<u8>> {
    match dtype {
        DType::F16 => Ok(bytes.to_vec()),
        DType::BF16 => Ok(bytes
            .chunks_exact(2)
            .flat_map(|c| {
                f16::from_f32(half::bf16::from_le_bytes([c[0], c[1]]).to_f32()).to_le_bytes()
            })
            .collect()),
        DType::F32 => Ok(bytes
            .chunks_exact(4)
            .flat_map(|c| f16::from_f32(f32::from_le_bytes([c[0], c[1], c[2], c[3]])).to_le_bytes())
            .collect()),
        other => Err(ForgeError::Unsupported(format!(
            "waga niekwantyzowana ma typ {other:?}, a kernele czytają f16"
        ))),
    }
}

fn upload(device: &dyn Device, bytes: &[u8]) -> Result<DevBuffer> {
    let buf = device.alloc(bytes.len().max(1), MemKind::Device, Pool::Weights)?;
    device.write(bytes, &buf, 0)?;
    Ok(buf)
}
