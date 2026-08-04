// ===== File: dense_exec.rs — the dense vocabulary, executed on Metal =====
//
// Everything in this file touches the device. That is why it lives HERE and not
// next to the model: a model holding buffers is a model for one card, and this
// repository already paid for that twice (docs/PRZEGLAD_UKLADU.md).
//
// The model above sends `Op` and holds `WeightId`. This is where an id becomes
// a buffer, an op becomes a dispatch, and a choice between kernel forms becomes
// a lookup in the variant registry.
//
// Two properties are deliberate:
//
//   * A whole step goes into ONE command buffer. Around 500 dispatches at
//     0.61 us each is 0.3 ms of overhead per token; the same work as separate
//     command buffers would be 10 ms, and as host round trips, 47 ms
//     (docs/pomiary/eks-a1-a3-apple-m4.md).
//   * Weights are uploaded quantized and dequantized inside the kernels. A
//     dequantized copy of a 7B checkpoint would be 16 GB against 4.2, and
//     reading the weights once IS the cost of a decode step.

use std::cell::RefCell;
use std::sync::Arc;

use half::f16;

use forge_formats::affine::{to_affine_triple, AffineTriple};
use forge_graph::{Act, ExecSpec, Executor, Op, QuantWeight, Tile, WeightId, WeightStore};
use forge_hal::{DevBuffer, Device, Event, KernelHandle, LaunchArgs, LaunchConfig, Pool, Stream};
use forge_types::{DType, DenseShape, ForgeError, MemKind, Result};

use crate::cpu_matmul::{CpuMatmul, Operands};
use crate::msl::{self, OutDtype, ScaleDtype};
use crate::variant::{
    self, AttentionForm, MatmulForm, Problem, RowSplit, ATTENTION_FORMS, MATMUL_FORMS,
};

/// Prompt tokens carried through the layers in one pass.
///
/// A multiple of the matrix-unit block, because that kernel writes whole
/// blocks: a chunk of 100 tokens stores 128 rows and the last 28 are padding
/// the scratch has to hold.
///
/// Not a round number picked for looks: past roughly this many tokens the
/// batched matmul stops winning, because its activation tile no longer fits in
/// cache and starts being re-read once per output row. Measured on M4 at 2.09x
/// for 128 and 0.72x for 512 (docs/pomiary/eks-a4-batched-matmul-m4.md).
const PREFILL_CHUNK: u32 = 1024;

/// A quantized weight: packed nibbles plus per-group scale and zero point.
struct Quantized {
    packed: DevBuffer,
    /// Bity czwarty i piąty, gdy `bits` wynosi sześć. Pusty bufor przy czterech
    /// — kernel czterobitowy go nie deklaruje, więc nie ma czego związać.
    high: Option<DevBuffer>,
    scales: DevBuffer,
    biases: DevBuffer,
    /// Cztery albo sześć. Jeden model potrafi mieć oba: Q4_K_M kładzie sześć
    /// bitów na attn_v, ffn_down i głowie, a cztery na reszcie.
    bits: u32,
    /// Wag na jedną skalę. Też własność TEJ wagi, nie modelu — Q4_K daje 32,
    /// Q6_K szesnaście, a MLX 64, i wszystkie trzy mogą wystąpić naraz.
    group: u32,
    rows: u32,
    cols: u32,
}

/// Waga w postaci, w której wykonawca ją trzyma. Model widzi tylko `WeightId`.
enum Weight {
    Quant(Quantized),
    Plain(DevBuffer),
}

/// Cztery warianty jednej rodziny kerneli.
struct QuantPipes {
    /// [szerokość kodu: 4→0, 6→1][wyjście: f32→0, f16→1]
    by: [[KernelHandle; 2]; 2],
}

impl QuantPipes {
    fn get(&self, bits: u32, f16_out: bool) -> &KernelHandle {
        &self.by[usize::from(bits == 6)][usize::from(f16_out)]
    }
}

struct Pipelines {
    /// Jedna rodzina, cztery warianty: dwie szerokości kodu razy dwa typy
    /// wyjścia. Trzymane w tablicy, a nie w czterech polach, bo wybór jest
    /// wyliczany, a nie pisany ręcznie w każdym miejscu wywołania.
    qmv: QuantPipes,
    qmm: QuantPipes,
    qmg: QuantPipes,
    rmsnorm: KernelHandle,
    silu_mul: KernelHandle,
    rope: KernelHandle,
    attn: KernelHandle,
    flash: KernelHandle,
    embed: KernelHandle,
    residual: KernelHandle,
    kv_append: KernelHandle,
    argmax: KernelHandle,
}

struct Scratch {
    h: DevBuffer,
    norm: DevBuffer,
    q: DevBuffer,
    k: DevBuffer,
    v: DevBuffer,
    attn: DevBuffer,
    proj: DevBuffer,
    gate: DevBuffer,
    up: DevBuffer,
    act: DevBuffer,
    logits: DevBuffer,
    token: DevBuffer,
    /// Identyfikatory tokenów kafla, czytane przez kernel osadzeń.
    ids: DevBuffer,
}

/// The dense vocabulary on a Metal device.
pub struct MetalExec {
    /// Wszystkie wagi modelu. Model nosi indeksy, nie bufory — i to jest cała
    /// różnica między „opisem architektury" a „modelem dla tej karty".
    weights: Vec<Weight>,
    /// Cache klucza i wartości, po parze na warstwę.
    kv: Vec<(DevBuffer, DevBuffer)>,
    device: Arc<dyn Device>,
    stream: Stream,
    pipes: Pipelines,
    scratch: Scratch,
    /// Scratch for the CPU's share of a split product.
    cpu: RefCell<CpuMatmul>,
    /// Names the command buffer a split submitted.
    split_event: Event,
    cpu_share: bool,
    shape: DenseShape,
    seq_cap: u32,
    /// Typ, w jakim skompilowano kernele kwantyzowane. Trzymany, żeby waga
    /// wgrana z innym typem parametrów odbiła się TU, a nie objawiła jako
    /// płynny, zły tekst.
    quant_params: DType,
}

impl MetalExec {
    /// Compiles the kernels this shape needs and allocates everything a step
    /// writes into. No weights yet — those arrive through `WeightStore`.
    pub fn new(device: Arc<dyn Device>, spec: ExecSpec) -> Result<Self> {
        let shape = spec.shape;
        let scales_dtype = quant_param_dtype(spec.quant_params)?;
        let norm_dtype = norm_weight_dtype(spec.norm_weights)?;
        let seq_cap = msl::ATTN_MAX_SEQ;

        let mut compile = |source: &str, entry: &str| -> Result<KernelHandle> {
            device.load_module(source.as_bytes())?.kernel(entry)
        };
        let pipes = Pipelines {
            qmv: quant_pipes(
                &mut compile,
                msl::qmv_affine_source,
                msl::qmv_affine_name,
                scales_dtype,
            )?,
            qmm: quant_pipes(
                &mut compile,
                msl::qmm_affine_source,
                msl::qmm_affine_name,
                scales_dtype,
            )?,
            qmg: quant_pipes(
                &mut compile,
                msl::qmg_affine_source,
                msl::qmg_affine_name,
                scales_dtype,
            )?,
            rmsnorm: compile(
                &msl::rmsnorm_source(norm_dtype),
                &msl::rmsnorm_name(norm_dtype),
            )?,
            silu_mul: compile(msl::SILU_MUL_SOURCE, msl::SILU_MUL_NAME)?,
            rope: compile(msl::ROPE_HALF_SPLIT_SOURCE, msl::ROPE_HALF_SPLIT_NAME)?,
            attn: compile(
                &msl::attn_decode_source(shape.head_dim),
                &msl::attn_decode_name(shape.head_dim),
            )?,
            flash: compile(
                &msl::flash_attn_source(shape.head_dim),
                &msl::flash_attn_name(shape.head_dim),
            )?,
            embed: compile(
                &msl::embed_gather_source(scales_dtype),
                &msl::embed_gather_name(scales_dtype),
            )?,
            residual: compile(msl::RESIDUAL_ADD_SOURCE, msl::RESIDUAL_ADD_NAME)?,
            kv_append: compile(msl::KV_APPEND_SOURCE, msl::KV_APPEND_NAME)?,
            argmax: compile(msl::ARGMAX_SOURCE, msl::ARGMAX_NAME)?,
        };

        let f16b =
            |elems: u32| device.alloc(elems as usize * 2, MemKind::Device, Pool::Activations);
        let f32b =
            |elems: u32| device.alloc(elems as usize * 4, MemKind::Device, Pool::Activations);
        // Wszystko poza logitami ma miejsce na cały kafel prefillu: dekodowanie
        // używa pierwszego wiersza tych samych buforów. Logity liczymy tylko dla
        // ostatniego tokenu, więc zostają jednym wierszem — 32 tys. kolumn razy
        // 128 byłoby 16 MB na coś, z czego czytamy 1/128.
        let n = PREFILL_CHUNK;
        let scratch = Scratch {
            h: f16b(n * shape.hidden)?,
            norm: f16b(n * shape.hidden)?,
            q: f16b(n * shape.hidden)?,
            k: f16b(n * shape.kv_width())?,
            v: f16b(n * shape.kv_width())?,
            attn: f16b(n * shape.hidden)?,
            proj: f32b(n * shape.hidden)?,
            gate: f16b(n * shape.inter)?,
            up: f16b(n * shape.inter)?,
            act: f16b(n * shape.inter)?,
            logits: f32b(shape.vocab)?,
            token: f32b(1)?,
            ids: device.alloc(n as usize * 4, MemKind::Device, Pool::Activations)?,
        };

        let kv_bytes = (shape.kv_heads * seq_cap * shape.head_dim) as usize * 2;
        let mut kv = Vec::with_capacity(shape.layers as usize);
        for _ in 0..shape.layers {
            kv.push((
                device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?,
                device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?,
            ));
        }

        let stream = device.create_stream()?;
        let split_event = device.create_event()?;
        Ok(Self {
            weights: Vec::new(),
            kv,
            device,
            stream,
            pipes,
            scratch,
            cpu: RefCell::new(CpuMatmul::new()),
            split_event,
            cpu_share: true,
            shape,
            seq_cap,
            quant_params: spec.quant_params,
        })
    }

    /// Turns the CPU's share of a large product on or off. On by default.
    ///
    /// Default ON because it is measured to win on every Apple part this runs
    /// on: prefill leaves the GPU at 77% of its matrix ceiling with bandwidth
    /// to spare, so the CPU adds throughput instead of taking it
    /// (docs/pomiary/eks-a7-cpu-gpu-wspolbieznie-m4.md).
    ///
    /// Reasons a caller might still turn it off:
    ///
    ///   * the CPU is wanted for something else — the two DO compete, and a
    ///     concurrent load costs this path up to a third of its rate;
    ///   * power, not speed, is the budget: the split trades watts for latency;
    ///   * pinning down where a numerical difference comes from, which is what
    ///     the correctness gate uses it for.
    ///
    /// Decode never takes this path whatever this is set to — the registry only
    /// picks the shared form for batches large enough to pay for it, and decode
    /// is bandwidth bound anyway: adding compute there measured -14%.
    pub fn set_cpu_share(&mut self, on: bool) {
        self.cpu_share = on;
    }
}

impl WeightStore for MetalExec {
    /// Kernele Metalowe indeksują trzy osobne tablice, więc źródło oddające
    /// bloki jest przepisywane TU. Model tego nie robi, bo nie zna kerneli —
    /// a to samo źródło idzie na CUDA bez ani jednego przepisania.
    fn put_quant(&mut self, w: QuantWeight) -> Result<WeightId> {
        let t = match w {
            QuantWeight::Affine(t) => t,
            QuantWeight::Packed(p) => {
                to_affine_triple(&p.planes.codes, p.quant, p.rows, p.cols)?
            }
        };
        self.put_affine(t)
    }

    fn put_plain(&mut self, bytes: Vec<u8>) -> Result<WeightId> {
        let buf = upload(&*self.device, &bytes)?;
        self.weights.push(Weight::Plain(buf));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }
}

impl MetalExec {
    fn put_affine(&mut self, t: AffineTriple) -> Result<WeightId> {
        // Trzy właściwości sprawdzane TU, przy użyciu, a nie zakładane przy
        // wołaniu. Każda z nich była już raz źródłem poprawnie wyglądającego,
        // złego tekstu.
        if t.param_dtype != self.quant_params {
            return Err(ForgeError::Format(format!(
                "waga ma parametry {:?}, a kernele skompilowano dla {:?}",
                t.param_dtype, self.quant_params
            )));
        }
        if !t.cols.is_multiple_of(t.group) {
            return Err(ForgeError::Unsupported(format!(
                "{} kolumn nie dzieli się na grupy po {}",
                t.cols, t.group
            )));
        }
        let high = match t.bits {
            4 => None,
            6 => Some(upload(&*self.device, bytemuck::cast_slice(&t.high))?),
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "{other} bitów na wagę, a kernele znają cztery i sześć"
                )))
            }
        };
        self.weights.push(Weight::Quant(Quantized {
            packed: upload(&*self.device, bytemuck::cast_slice(&t.packed))?,
            high,
            scales: upload(&*self.device, &t.scales)?,
            biases: upload(&*self.device, &t.biases)?,
            bits: t.bits,
            group: t.group as u32,
            rows: t.rows as u32,
            cols: t.cols as u32,
        }));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }
}

impl Executor for MetalExec {
    /// Jedno wejście dla całego słownictwa. Rozgałęzienie jest TU, w wykonawcy,
    /// a nie w modelu — dzięki temu backend, który czegoś nie umie, odmawia w
    /// jednym miejscu, zamiast implementować zaślepkę.
    fn run(&self, op: &Op) -> Result<()> {
        let step = match op {
            Op::Embed { step, .. }
            | Op::RmsNorm { step, .. }
            | Op::MatMul { step, .. }
            | Op::Rope { step, .. }
            | Op::KvAppend { step, .. }
            | Op::Attention { step, .. }
            | Op::SiluMul { step }
            | Op::Residual { step, .. }
            | Op::LogitsOfLast { step, .. } => step,
        };
        // Cache jest jedną ciągłą połacią na warstwę, więc ten wykonawca trzyma
        // JEDNĄ sekwencję i mówi to przez `Tile::max_lanes`. Krok z wieloma
        // lane'ami odbija się TU, zamiast policzyć się nad cudzym kontekstem —
        // stronicowanie po tej stronie to osobna praca i osobne kernele MSL.
        let pos = match step.lanes() {
            [lane] if lane.slot == 0 => lane.pos,
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "{} lane'ów, a ten wykonawca trzyma jeden ciągły cache",
                    other.len()
                )))
            }
        };
        let tokens = step.tokens();
        match op {
            Op::Embed { table, tokens, .. } => self.op_embed(*table, tokens),
            Op::RmsNorm { out, x, w, .. } => self.op_rmsnorm(*out, *x, *w, tokens),
            Op::MatMul { out, w, x, .. } => self.op_matmul(*out, *w, *x, tokens),
            Op::Rope { act, heads, .. } => self.op_rope(*act, *heads, pos, tokens),
            Op::KvAppend { layer, .. } => self.op_kv_append(*layer, pos, tokens),
            Op::Attention { layer, .. } => self.op_attention(*layer, pos + tokens, tokens),
            Op::SiluMul { .. } => self.op_silu_mul(tokens),
            Op::Residual { src, .. } => self.op_residual(*src, tokens),
            Op::LogitsOfLast { w, x, .. } => self.op_logits_of_last(*w, *x, tokens),
        }
    }

    fn sync(&self) -> Result<()> {
        self.stream.synchronize()
    }

    fn read(&self, act: Act, len: usize) -> Result<Vec<f32>> {
        self.stream.synchronize()?;
        if Self::is_half(act) {
            let mut raw = vec![0u8; len * 2];
            self.device.read(self.buf(act), 0, &mut raw)?;
            return Ok(raw
                .chunks_exact(2)
                .map(|c| f16::from_bits(u16::from_le_bytes([c[0], c[1]])).to_f32())
                .collect());
        }
        let mut raw = vec![0u8; len * 4];
        self.device.read(self.buf(act), 0, &mut raw)?;
        Ok(raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }

    fn argmax(&self, act: Act, lanes: usize) -> Result<Vec<u32>> {
        if lanes != 1 {
            return Err(ForgeError::Unsupported(format!(
                "wybór dla {lanes} lane'ów, a ten wykonawca trzyma jeden"
            )));
        }
        self.launch(
            &self.pipes.argmax,
            LaunchArgs::new()
                .buf(&self.scratch.token)
                .buf(self.buf(act))
                .scalar(self.shape.vocab),
            1,
            msl::ARGMAX_THREADS,
        )?;
        self.stream.synchronize()?;
        let mut raw = [0u8; 4];
        self.device.read(&self.scratch.token, 0, &mut raw)?;
        Ok(vec![u32::from_le_bytes(raw)])
    }

    fn seq_cap(&self) -> u32 {
        self.seq_cap
    }

    fn tile(&self) -> Tile {
        Tile {
            max_tokens: PREFILL_CHUNK,
            max_lanes: 1,
            align: msl::QMG_BM,
        }
    }
}

impl MetalExec {
    /// Bufor tego slotu.
    fn buf(&self, a: Act) -> &DevBuffer {
        match a {
            Act::Hidden => &self.scratch.h,
            Act::Norm => &self.scratch.norm,
            Act::Query => &self.scratch.q,
            Act::Key => &self.scratch.k,
            Act::Value => &self.scratch.v,
            Act::Attn => &self.scratch.attn,
            Act::Proj => &self.scratch.proj,
            Act::Gate => &self.scratch.gate,
            Act::Up => &self.scratch.up,
            Act::Activated => &self.scratch.act,
            Act::Logits => &self.scratch.logits,
        }
    }

    /// Czy slot trzyma połówkową precyzję. Własność slotu, nie wywołania.
    fn is_half(a: Act) -> bool {
        !matches!(a, Act::Proj | Act::Logits)
    }

    /// Osadzenia tokenów kafla do `Act::Hidden`.
    fn op_embed(&self, table: WeightId, tokens: &[u32]) -> Result<()> {
        let ids: Vec<u8> = tokens.iter().flat_map(|t| t.to_le_bytes()).collect();
        self.device.write(&ids, &self.scratch.ids, 0)?;
        let w = self.quant(table)?;
        let n = tokens.len() as u32;
        self.launch(
            &self.pipes.embed,
            LaunchArgs::new()
                .buf(self.buf(Act::Hidden))
                .buf(&w.packed)
                .buf(&w.scales)
                .buf(&w.biases)
                .buf(&self.scratch.ids)
                .scalar(self.shape.hidden)
                .scalar(w.group)
                .scalar(n),
            msl::elementwise_groups(n * self.shape.hidden),
            msl::ELEMENTWISE_THREADS,
        )
    }

    /// Głowa wyjściowa dla ostatniego tokenu kafla.
    fn op_logits_of_last(&self, w: WeightId, x: Act, tokens: u32) -> Result<()> {
        let last = (tokens - 1) as usize * self.shape.hidden as usize * 2;
        let weight = self.quant(w)?;
        self.gemv(
            self.pipes.qmv.get(weight.bits, false),
            self.buf(Act::Logits),
            weight,
            self.buf(x),
            last,
        )
    }

    /// `out = norm(x) * waga`.
    fn op_rmsnorm(&self, out: Act, x: Act, w: WeightId, tokens: u32) -> Result<()> {
        self.launch(
            &self.pipes.rmsnorm,
            LaunchArgs::new()
                .buf(self.buf(out))
                .buf(self.buf(x))
                .buf(self.plain(w)?)
                .scalar(self.shape.hidden)
                .scalar(self.shape.eps),
            tokens,
            msl::RMSNORM_THREADS,
        )
    }

    /// `out = x * wagaᵀ`. Formę wybiera rejestr, typ zapisu wynika ze slotu.
    fn op_matmul(&self, out: Act, w: WeightId, x: Act, tokens: u32) -> Result<()> {
        self.matmul(
            self.buf(out),
            self.quant(w)?,
            self.buf(x),
            tokens,
            Self::is_half(out),
        )
    }

    fn op_rope(&self, a: Act, heads: u32, pos: u32, tokens: u32) -> Result<()> {
        let threads = msl::ELEMENTWISE_THREADS;
        self.launch(
            &self.pipes.rope,
            LaunchArgs::new()
                .buf(self.buf(a))
                .scalar(heads)
                .scalar(self.shape.head_dim)
                .scalar(pos)
                .scalar(self.shape.rope_theta)
                .scalar(tokens),
            msl::rope_groups(heads, self.shape.head_dim, tokens, threads),
            threads,
        )
    }

    /// Dopisuje klucz i wartość tego kafla do cache'u warstwy.
    fn op_kv_append(&self, layer: usize, pos: u32, tokens: u32) -> Result<()> {
        let (kc, vc) = self
            .kv
            .get(layer)
            .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
        self.kv_append(kc, self.buf(Act::Key), pos, tokens)?;
        self.kv_append(vc, self.buf(Act::Value), pos, tokens)
    }

    /// `Hidden += src`. The kernel reads and writes the same index, so aliasing
    /// the output onto the input is safe and saves a buffer that would
    /// otherwise be copied every layer.
    fn op_residual(&self, src: Act, tokens: u32) -> Result<()> {
        self.launch(
            &self.pipes.residual,
            LaunchArgs::new()
                .buf(&self.scratch.h)
                .buf(&self.scratch.h)
                .buf(self.buf(src))
                .scalar(tokens * self.shape.hidden),
            msl::elementwise_groups(tokens * self.shape.hidden),
            msl::ELEMENTWISE_THREADS,
        )
    }

    /// `Activated = silu(Gate) * Up`.
    fn op_silu_mul(&self, tokens: u32) -> Result<()> {
        let n = tokens * self.shape.inter;
        self.launch(
            &self.pipes.silu_mul,
            LaunchArgs::new()
                .buf(self.buf(Act::Activated))
                .buf(self.buf(Act::Gate))
                .buf(self.buf(Act::Up))
                .scalar(n),
            msl::silu_mul_groups(n),
            msl::SILU_MUL_THREADS,
        )
    }

    /// Uwaga nad cache'em warstwy. Formę wybiera rejestr, tak samo jak przy
    /// mnożeniu — model nie ma tu zdania.
    fn op_attention(&self, layer: usize, seq: u32, tokens: u32) -> Result<()> {
        let s = self.shape;
        let (kc, vc) = self
            .kv
            .get(layer)
            .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
        let form = ATTENTION_FORMS
            .pick(&Problem::new(tokens, s.heads, s.head_dim))
            .ok_or_else(|| ForgeError::Unsupported("brak wariantu uwagi".into()))?;
        let (kernel, groups, threads) = match form.form {
            AttentionForm::Blocked if msl::flash_fits(tokens, s.head_dim) => (
                &self.pipes.flash,
                msl::flash_attn_groups(s.heads, tokens),
                msl::FLASH_THREADS,
            ),
            _ => (
                &self.pipes.attn,
                msl::attn_groups(s.heads, tokens),
                msl::ATTN_THREADS,
            ),
        };
        self.launch(
            kernel,
            LaunchArgs::new()
                .buf(self.buf(Act::Attn))
                .buf(self.buf(Act::Query))
                .buf(kc)
                .buf(vc)
                .scalar(s.heads)
                .scalar(s.kv_heads)
                .scalar(seq)
                .scalar(self.seq_cap)
                .scalar(s.attn_scale())
                .scalar(tokens),
            groups,
            threads,
        )
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

    fn gemv(
        &self,
        kernel: &KernelHandle,
        out: &DevBuffer,
        w: &Quantized,
        x: &DevBuffer,
        x_offset: usize,
    ) -> Result<()> {
        self.launch(
            kernel,
            weight_args(LaunchArgs::new().buf(out), w, x, x_offset)?
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(w.group),
            msl::qmv_affine_4bit_groups(w.rows),
            msl::QMV_THREADS,
        )
    }

    /// Projection for a whole batch.
    ///
    /// Which form serves which batch is a decision of the registry, not of this
    /// function: the thresholds and the measurements behind them live in
    /// `crate::variant`, where they can be checked for totality and for the
    /// absence of a cliff. Here there is only the mapping from a chosen form to
    /// the pipeline that implements it.
    fn matmul(
        &self,
        out: &DevBuffer,
        w: &Quantized,
        x: &DevBuffer,
        tokens: u32,
        f16_out: bool,
    ) -> Result<()> {
        let problem = Problem {
            tokens,
            rows: w.rows,
            cols: w.cols,
            bits: w.bits,
        };
        let chosen = MATMUL_FORMS.pick(&problem).ok_or_else(|| {
            ForgeError::Unsupported(format!("brak wariantu mnożenia dla {problem:?}"))
        })?;
        match chosen.form {
            MatmulForm::Vector => self.gemv(self.pipes.qmv.get(w.bits, f16_out), out, w, x, 0),
            MatmulForm::RegisterBlocked => self.matmul_blocked(out, w, x, tokens, f16_out),
            MatmulForm::MatrixUnits => self.matmul_matrix_units(out, w, x, tokens, f16_out, None),
            MatmulForm::MatrixUnitsSharedWithCpu if !self.cpu_share => {
                self.matmul_matrix_units(out, w, x, tokens, f16_out, None)
            }
            MatmulForm::MatrixUnitsSharedWithCpu => {
                let split = variant::split_rows(&problem).ok_or_else(|| {
                    ForgeError::Other(format!("podział wybrany dla {problem:?}, ale niemożliwy"))
                })?;
                self.matmul_matrix_units(out, w, x, tokens, f16_out, Some(split))
            }
        }
    }

    /// The matrix-unit form, optionally sharing its rows with the CPU.
    ///
    /// The kernel always receives the FULL row count — that is the stride it
    /// writes with — and the grid is what decides which rows it touches. So
    /// giving it a shorter grid leaves the tail of every row untouched, which
    /// is exactly the window the CPU then fills.
    fn matmul_matrix_units(
        &self,
        out: &DevBuffer,
        w: &Quantized,
        x: &DevBuffer,
        tokens: u32,
        f16_out: bool,
        split: Option<RowSplit>,
    ) -> Result<()> {
        let k = self.pipes.qmg.get(w.bits, f16_out);
        let Some(split) = split else {
            let (gx, gy) = msl::qmg_affine_4bit_groups(w.rows, tokens);
            return self.launch_qmg(k, out, w, x, tokens, (gx, gy));
        };

        let operands = Operands {
            packed: host_slice(&w.packed)?,
            scales: host_slice(&w.scales)?,
            biases: host_slice(&w.biases)?,
            x: host_slice(x)?,
            out: out
                .host_ptr()
                .ok_or_else(|| ForgeError::Other("Metal: wyjście bez adresu hosta".into()))?,
            out_f16: f16_out,
            tokens,
            rows: w.rows,
            cols: w.cols,
            group: w.group,
            bits: w.bits,
        };
        let mut cpu = self.cpu.borrow_mut();
        cpu.check(&operands, split.gpu_rows, split.cpu_rows)?;

        // Unpacking FIRST, before the wait below. It needs only the weights,
        // which are static, so it costs nothing here: it runs in the window
        // where the CPU would otherwise be idle watching the GPU finish the
        // activations. Measured at 938 us a product, this is the difference
        // between paying for it and hiding it.
        cpu.unpack(&operands, split.gpu_rows as usize, split.cpu_rows as usize);

        // The CPU half reads `x` with its own load instructions, so `x` has to
        // BE there. Everything that produces it is sitting in the open command
        // buffer, queued and not yet run: the GPU half is ordered after it and
        // is therefore safe, but the CPU is not ordered against the GPU at all.
        // Without this wait the CPU multiplies whatever the buffer happened to
        // hold — which is not a crash, just a different model.
        self.stream.synchronize()?;

        let (gx, gy) = msl::qmg_affine_4bit_groups(split.gpu_rows, tokens);
        self.launch_qmg(k, out, w, x, tokens, (gx, gy))?;

        // Submit, or the dispatch would sit in the open command buffer and the
        // CPU would spend its share racing an idle GPU. This is the one place
        // that deliberately pays for a command buffer of its own — 19,6 us
        // against 0,61 (EKS-A3) — and `split_rows` only allows it where that is
        // a couple of percent of the work being overlapped.
        self.device.record_event(&self.split_event, &self.stream)?;

        // SAFETY: the GPU dispatch above writes rows below `gpu_rows` and this
        // writes from `gpu_rows` up, into the same shared allocation. The two
        // ranges are disjoint, so no ordering between them is needed — only the
        // wait below, before anything reads the whole result.
        unsafe { cpu.multiply(&operands, split.gpu_rows, split.cpu_rows)? };
        drop(cpu);
        self.split_event.synchronize()
    }

    /// The matrix-unit dispatch itself. The kernel always receives the FULL row
    /// count — that is the stride it writes with — and the grid is what decides
    /// which rows it touches.
    fn launch_qmg(
        &self,
        k: &KernelHandle,
        out: &DevBuffer,
        w: &Quantized,
        x: &DevBuffer,
        tokens: u32,
        grid: (u32, u32),
    ) -> Result<()> {
        self.device.launch(
            k,
            &LaunchConfig {
                grid: (grid.0, grid.1, 1),
                block: (msl::QMG_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &weight_args(LaunchArgs::new().buf(out), w, x, 0)?
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(w.group)
                .scalar(tokens),
            &self.stream,
        )
    }

    /// Register-blocked loop, for batches too small to fill a matrix block.
    fn matmul_blocked(
        &self,
        out: &DevBuffer,
        w: &Quantized,
        x: &DevBuffer,
        tokens: u32,
        f16_out: bool,
    ) -> Result<()> {
        let k = self.pipes.qmm.get(w.bits, f16_out);
        let (gx, gy) = msl::qmm_affine_4bit_groups(w.rows, tokens);
        self.device.launch(
            k,
            &LaunchConfig {
                grid: (gx, gy, 1),
                block: (msl::QMM_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &weight_args(LaunchArgs::new().buf(out), w, x, 0)?
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(w.group)
                .scalar(tokens),
            &self.stream,
        )
    }

    fn kv_append(&self, cache: &DevBuffer, src: &DevBuffer, pos: u32, tokens: u32) -> Result<()> {
        let s = self.shape;
        self.launch(
            &self.pipes.kv_append,
            LaunchArgs::new()
                .buf(cache)
                .buf(src)
                .scalar(s.kv_heads)
                .scalar(s.head_dim)
                .scalar(self.seq_cap)
                .scalar(pos)
                .scalar(tokens),
            msl::elementwise_groups(tokens * s.kv_width()),
            msl::ELEMENTWISE_THREADS,
        )
    }

    fn launch(
        &self,
        kernel: &KernelHandle,
        args: LaunchArgs,
        groups: u32,
        threads: u32,
    ) -> Result<()> {
        self.device.launch(
            kernel,
            &LaunchConfig {
                grid: (groups, 1, 1),
                block: (threads, 1, 1),
                shared_mem_bytes: 0,
            },
            &args,
            &self.stream,
        )
    }
}

/// Host view of a device buffer.
///
/// On Apple every allocation is shared, so this is literally the memory the GPU
/// reads — no copy and no transfer. It is only ever taken for buffers the GPU
/// is READING during a split (weights and activations); the one buffer both
/// units write is handed over as a raw pointer with its disjointness spelled
/// out at the call site.
fn host_slice<T>(buf: &DevBuffer) -> Result<&[T]> {
    let ptr = buf
        .host_ptr()
        .ok_or_else(|| ForgeError::Other("Metal: bufor bez adresu hosta".into()))?;
    if ptr as usize % std::mem::align_of::<T>() != 0 {
        return Err(ForgeError::Other(format!(
            "Metal: adres {ptr:p} nie jest wyrównany do {} B",
            std::mem::align_of::<T>()
        )));
    }
    // SAFETY: the allocation is `buf.len()` bytes of shared memory, alive for
    // as long as the buffer, and nothing writes it while the borrow lasts.
    Ok(
        unsafe {
            std::slice::from_raw_parts(ptr as *const T, buf.len() / std::mem::size_of::<T>())
        },
    )
}

/// Typ parametrów kwantyzacji w wersji, którą znają kernele.
fn quant_param_dtype(d: DType) -> Result<ScaleDtype> {
    match d {
        DType::F16 => Ok(ScaleDtype::F16),
        DType::BF16 => Ok(ScaleDtype::Bf16),
        other => Err(ForgeError::Unsupported(format!(
            "skale w {other:?}, a kernel zna f16 i bf16"
        ))),
    }
}

/// Typ wagi normalizacji. Osobno od skal, bo to osobna właściwość źródła: GGUF
/// trzyma normy w f32, a skale w f16.
fn norm_weight_dtype(d: DType) -> Result<ScaleDtype> {
    match d {
        DType::F16 => Ok(ScaleDtype::F16),
        DType::BF16 => Ok(ScaleDtype::Bf16),
        DType::F32 => Ok(ScaleDtype::F32),
        other => Err(ForgeError::Unsupported(format!(
            "waga normy ma typ {other:?}, a kernel zna f16, bf16 i f32"
        ))),
    }
}

/// Kompiluje cztery warianty jednej rodziny: dwie szerokości kodu razy dwa
/// typy wyjścia. Wypisywanie ich ręcznie znaczyłoby dwanaście wywołań, w
/// których łatwo pomylić jeden parametr i dostać kernel liczący co innego.
fn quant_pipes(
    compile: &mut impl FnMut(&str, &str) -> Result<KernelHandle>,
    source: fn(msl::Bits, ScaleDtype, OutDtype) -> String,
    name: fn(msl::Bits, ScaleDtype, OutDtype) -> String,
    scales: ScaleDtype,
) -> Result<QuantPipes> {
    let mut one = |bits, out| compile(&source(bits, scales, out), &name(bits, scales, out));
    Ok(QuantPipes {
        by: [
            [
                one(msl::Bits::Four, OutDtype::F32)?,
                one(msl::Bits::Four, OutDtype::F16)?,
            ],
            [
                one(msl::Bits::Six, OutDtype::F32)?,
                one(msl::Bits::Six, OutDtype::F16)?,
            ],
        ],
    })
}

/// Bufory wagi w kolejności, której oczekuje kernel.
///
/// Sześciobitowy deklaruje jeden bufor więcej, więc kolejność skalarów zależy
/// od szerokości kodu. Zbierane w jednym miejscu, bo rozjazd między tym a
/// deklaracją kernela nie jest błędem kompilacji — jest złym wynikiem.
fn weight_args(
    args: LaunchArgs,
    w: &Quantized,
    x: &DevBuffer,
    x_offset: usize,
) -> Result<LaunchArgs> {
    let args = args
        .buf(&w.packed)
        .buf(&w.scales)
        .buf(&w.biases)
        .buf_at(x, x_offset)?;
    match (&w.high, w.bits) {
        (Some(h), 6) => Ok(args.buf(h)),
        (None, 4) => Ok(args),
        _ => Err(ForgeError::Other(format!(
            "waga deklaruje {} bitów, a bufor wyższych bitów {}",
            w.bits,
            if w.high.is_some() {
                "jest"
            } else {
                "go nie ma"
            }
        ))),
    }
}

fn upload(device: &dyn Device, bytes: &[u8]) -> Result<DevBuffer> {
    let buf = device.alloc(bytes.len().max(1), MemKind::Device, Pool::Weights)?;
    device.write(bytes, &buf, 0)?;
    Ok(buf)
}
