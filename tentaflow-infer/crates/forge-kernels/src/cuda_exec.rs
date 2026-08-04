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
//   * The KV cache is one contiguous run per layer, `[pos][kv_head][dim]`, and
//     `KvAppend` is a copy into it. Paging is a property of a scheduler that
//     serves many sequences; one sequence does not need it, and `Op` does not
//     yet say it.

use std::cell::RefCell;
use std::sync::Arc;

use half::f16;

use forge_formats::FfnActivation;
use forge_graph::{Act, ExecSpec, Executor, Op, QuantWeight, Tile, WeightId, WeightStore};
use forge_hal::{DevBuffer, Device, Pool, Stream};
use forge_types::{DType, DenseShape, ForgeError, MemKind, QuantKind, Result};

use crate::launchers::{Kernels, SAMPLE_SCRATCH_PAIRS};

/// Prompt tokens carried through the layers in one pass.
///
/// Bounded by the scratch it forces: at 4096 hidden and 11264 intermediate,
/// every extra token costs about 100 KB across the eleven slots.
const PREFILL_CHUNK: u32 = 512;

/// Context the cache will hold. An allocation, not a threshold — the cache is
/// contiguous, so its length is fixed when the executor is built.
const SEQ_CAP: u32 = 4096;

/// A quantized weight, in the blocks the source packed and the kernels read.
struct Quantized {
    blocks: DevBuffer,
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
    /// Token ids of the current tile, read by the embedding gather.
    ids: DevBuffer,
    /// Absolute positions of the current tile, read by RoPE.
    positions: DevBuffer,
    /// Chosen id (i32) plus its logprob (f32), written by the argmax.
    choice: DevBuffer,
    sample_vals: DevBuffer,
    sample_idx: DevBuffer,
}

/// The dense vocabulary on a CUDA device.
pub struct CudaExec {
    device: Arc<dyn Device>,
    kernels: Kernels,
    stream: Stream,
    weights: Vec<Weight>,
    /// Key and value per layer, `[pos][kv_head][dim]` f16 — the layout
    /// `attn_full_f16` reads and `Act::Key` already has, so appending is a copy.
    kv: Vec<(DevBuffer, DevBuffer)>,
    scratch: Scratch,
    /// Kopie tego, co stoi w `ids` i `positions` na urządzeniu. Patrz
    /// `stage_i32` — bez nich każdy zapis sterujący byłby wyścigiem z tym, co
    /// już stoi w kolejce.
    ids_host: RefCell<Vec<i32>>,
    positions_host: RefCell<Vec<i32>>,
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

        let n = PREFILL_CHUNK;
        let f16b =
            |elems: u32| device.alloc(elems as usize * 2, MemKind::Device, Pool::Activations);
        let i32b =
            |elems: u32| device.alloc(elems as usize * 4, MemKind::Device, Pool::Activations);
        let scratch = Scratch {
            h: f16b(n * shape.hidden)?,
            norm: f16b(n * shape.hidden)?,
            q: f16b(n * shape.hidden)?,
            k: f16b(n * shape.kv_width())?,
            v: f16b(n * shape.kv_width())?,
            attn: f16b(n * shape.hidden)?,
            proj: f16b(n * shape.hidden)?,
            gate: f16b(n * shape.inter)?,
            up: f16b(n * shape.inter)?,
            act: f16b(n * shape.inter)?,
            // Logity liczymy tylko dla ostatniego tokenu kafla, więc jeden
            // wiersz — słownik razy kafel to megabajty, z których czytamy 1/n.
            logits: i32b(shape.vocab)?,
            ids: i32b(n)?,
            positions: i32b(n)?,
            choice: i32b(2)?,
            sample_vals: i32b(SAMPLE_SCRATCH_PAIRS as u32)?,
            sample_idx: i32b(SAMPLE_SCRATCH_PAIRS as u32)?,
        };

        let kv_bytes = (SEQ_CAP * shape.kv_width()) as usize * 2;
        let mut kv = Vec::with_capacity(shape.layers as usize);
        for _ in 0..shape.layers {
            kv.push((
                device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?,
                device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?,
            ));
        }

        Ok(Self {
            device,
            kernels,
            stream,
            weights: Vec::new(),
            kv,
            scratch,
            ids_host: RefCell::new(Vec::new()),
            positions_host: RefCell::new(Vec::new()),
            shape,
            norm_weights: spec.norm_weights,
        })
    }
}

impl WeightStore for CudaExec {
    /// The blocks go up as they came off disk.
    ///
    /// A source that only offers the affine triple is refused rather than
    /// repacked: packing six-bit weights back into nibbles is lossy, and a
    /// quantized model that quietly loses two bits per weight still produces
    /// fluent text.
    fn put_quant(&mut self, w: QuantWeight) -> Result<WeightId> {
        let QuantWeight::Blocks {
            data,
            quant,
            rows,
            cols,
        } = w
        else {
            return Err(ForgeError::Unsupported(
                "kernele CUDA czytają bloki źródła, a to źródło oddaje wyłącznie \
                 postać afiniczną"
                    .into(),
            ));
        };
        if !matches!(quant, QuantKind::Q4K | QuantKind::Q6K) {
            return Err(ForgeError::Unsupported(format!(
                "{quant:?}: ten wykonawca zna Q4_K i Q6_K"
            )));
        }
        // 256 wartości na superblok w obu formatach. Wiersz krótszy niż
        // superblok adresowałby cudzy blok, więc sprawdzane TU, przy wgraniu,
        // a nie zakładane przy każdym mnożeniu.
        if !cols.is_multiple_of(256) {
            return Err(ForgeError::Unsupported(format!(
                "{cols} kolumn nie dzieli się na superbloki po 256"
            )));
        }
        self.weights.push(Weight::Quant(Quantized {
            blocks: upload(&*self.device, &data)?,
            quant,
            rows,
            cols,
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
    fn run(&self, op: &Op) -> Result<()> {
        match op {
            Op::Embed { table, tokens } => self.op_embed(*table, tokens),
            Op::RmsNorm { out, x, w, tokens } => self.op_rmsnorm(*out, *x, *w, *tokens),
            Op::MatMul { out, w, x, tokens } => {
                let w = self.quant(*w)?;
                self.matmul(self.buf(*out), w, self.buf(*x), *tokens)
            }
            Op::Rope {
                act,
                heads,
                pos,
                tokens,
            } => self.op_rope(*act, *heads, *pos, *tokens),
            Op::KvAppend { layer, pos, tokens } => self.op_kv_append(*layer, *pos, *tokens),
            Op::Attention { layer, seq, tokens } => self.op_attention(*layer, *seq, *tokens),
            Op::SiluMul { tokens } => self.kernels.glu_mul_f16(
                FfnActivation::SiLU,
                &self.scratch.act,
                &self.scratch.gate,
                &self.scratch.up,
                *tokens as usize * self.shape.inter as usize,
                &self.stream,
            ),
            Op::Residual { src, tokens } => self.kernels.residual_add_f16(
                &self.scratch.h,
                self.buf(*src),
                *tokens as usize * self.shape.hidden as usize,
                &self.stream,
            ),
            Op::LogitsOfLast { w, x, tokens } => self.op_logits_of_last(*w, *x, *tokens),
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
    fn argmax(&self, act: Act) -> Result<u32> {
        self.kernels.sample_argmax_f32(
            &self.scratch.choice,
            &self.scratch.sample_vals,
            &self.scratch.sample_idx,
            self.buf(act),
            self.shape.vocab as usize,
            &self.stream,
        )?;
        self.stream.synchronize()?;
        let mut raw = [0u8; 4];
        self.device.read(&self.scratch.choice, 0, &mut raw)?;
        let id = i32::from_le_bytes(raw);
        u32::try_from(id).map_err(|_| ForgeError::Other(format!("wybór zwrócił token {id}")))
    }

    fn seq_cap(&self) -> u32 {
        SEQ_CAP
    }

    fn tile(&self) -> Tile {
        Tile {
            max_tokens: PREFILL_CHUNK,
            // Kernele biorą liczbę tokenów jako argument i same domykają ogon,
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
            Act::Proj => &self.scratch.proj,
            Act::Gate => &self.scratch.gate,
            Act::Up => &self.scratch.up,
            Act::Activated => &self.scratch.act,
            Act::Logits => &self.scratch.logits,
        }
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

    /// Stages the small i32 control data — token ids, positions — a tile needs.
    ///
    /// A HAL write lands on the legacy stream while this executor's stream is
    /// non-blocking, so it is NOT ordered against work already queued: a write
    /// issued now can overwrite bytes a queued kernel has not read yet. That
    /// content changes exactly once per tile, so the answer is to notice when
    /// it changes and drain then — the alternative, draining on every call,
    /// would be eighty-one drains a token for data that never moved.
    fn stage_i32(
        &self,
        dst: &DevBuffer,
        mirror: &RefCell<Vec<i32>>,
        values: Vec<i32>,
    ) -> Result<()> {
        if *mirror.borrow() == values {
            return Ok(());
        }
        self.stream.synchronize()?;
        let raw: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.device.write(&raw, dst, 0)?;
        *mirror.borrow_mut() = values;
        Ok(())
    }

    fn op_embed(&self, table: WeightId, tokens: &[u32]) -> Result<()> {
        let w = self.quant(table)?;
        if w.quant != QuantKind::Q4K {
            return Err(ForgeError::Unsupported(format!(
                "tablica osadzeń jest {:?}, a jedyny kernel zbierający wiersze czyta Q4_K",
                w.quant
            )));
        }
        self.stage_i32(
            &self.scratch.ids,
            &self.ids_host,
            tokens.iter().map(|&t| t as i32).collect(),
        )?;
        self.kernels.gather_q4_k_rows_f16(
            &self.scratch.h,
            &w.blocks,
            &self.scratch.ids,
            tokens.len(),
            w.rows,
            w.cols,
            &self.stream,
        )
    }

    fn op_rmsnorm(&self, out: Act, x: Act, w: WeightId, tokens: u32) -> Result<()> {
        self.kernels.rmsnorm_f16(
            self.buf(out),
            self.buf(x),
            self.plain(w)?,
            tokens as usize,
            self.shape.hidden as usize,
            self.shape.eps,
            &self.stream,
        )
    }

    fn op_rope(&self, act: Act, heads: u32, pos: u32, tokens: u32) -> Result<()> {
        // Pozycje są bezwzględne i idą przez bufor urządzenia, bo kernel czyta
        // je stamtąd — jeden zapis na kafel, nie jeden na token.
        self.stage_i32(
            &self.scratch.positions,
            &self.positions_host,
            (0..tokens).map(|t| (pos + t) as i32).collect(),
        )?;
        self.kernels.rope_neox_f16(
            self.buf(act),
            &self.scratch.positions,
            tokens as usize,
            heads as usize,
            self.shape.head_dim as usize,
            self.shape.rope_theta,
            None,
            &self.stream,
        )
    }

    /// Klucz i wartość kafla na swoje miejsce w cache'u warstwy.
    ///
    /// Kopia, nie kernel: `Act::Key` ma już układ `[token][kv_head][dim]`, a
    /// cache jest tym samym układem rozciągniętym na kontekst, więc kafel jest
    /// ciągłym zakresem bajtów pod pozycją `pos`.
    fn op_kv_append(&self, layer: usize, pos: u32, tokens: u32) -> Result<()> {
        let (kc, vc) = self
            .kv
            .get(layer)
            .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
        let width = self.shape.kv_width() as usize * 2;
        let (at, bytes) = (pos as usize * width, tokens as usize * width);
        self.device
            .copy(&self.scratch.k, 0, kc, at, bytes, &self.stream)?;
        self.device
            .copy(&self.scratch.v, 0, vc, at, bytes, &self.stream)
    }

    fn op_attention(&self, layer: usize, seq: u32, tokens: u32) -> Result<()> {
        let s = self.shape;
        let (kc, vc) = self
            .kv
            .get(layer)
            .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
        self.kernels.attn_full_f16(
            &self.scratch.attn,
            &self.scratch.q,
            kc,
            vc,
            tokens as usize,
            s.heads as usize,
            s.kv_heads as usize,
            s.head_dim as usize,
            seq as usize,
            true,
            // Wiersz zapytania `t` siedzi na pozycji bezwzględnej `seq - tokens
            // + t`, i to ona decyduje, dokąd sięga maska przyczynowa.
            (seq - tokens) as usize,
            s.attn_scale(),
            &self.stream,
        )
    }

    /// The output head, for the LAST token of the tile only.
    fn op_logits_of_last(&self, w: WeightId, x: Act, tokens: u32) -> Result<()> {
        let weight = self.quant(w)?;
        let last = (tokens - 1) as usize * self.shape.hidden as usize * 2;
        match weight.quant {
            QuantKind::Q4K => self.kernels.gemv_q4_k_out_f32(
                &self.scratch.logits,
                0,
                &weight.blocks,
                self.buf(x),
                last,
                weight.rows,
                weight.cols,
                &self.stream,
            ),
            _ => self.kernels.gemv_q6_k_out_f32(
                &self.scratch.logits,
                0,
                &weight.blocks,
                self.buf(x),
                last,
                weight.rows,
                weight.cols,
                &self.stream,
            ),
        }
    }

    /// `out = x * wagaᵀ`, in the form this batch size wants.
    ///
    /// One token reads the whole matrix for one column of results, so it is
    /// bandwidth bound and takes the vector form; a tile reuses each weight
    /// across its rows and takes the blocked one. The threshold is the batch
    /// size itself because that is what the two kernels differ over — there is
    /// no third choice to arbitrate here yet.
    fn matmul(&self, out: &DevBuffer, w: &Quantized, x: &DevBuffer, tokens: u32) -> Result<()> {
        let (rows, cols) = (w.rows, w.cols);
        match (w.quant, tokens) {
            (QuantKind::Q4K, 1) => {
                self.kernels
                    .gemv_q4_k_f16(out, &w.blocks, x, rows, cols, &self.stream)
            }
            (QuantKind::Q4K, n) => {
                self.kernels
                    .gemm_q4_k_f16(out, &w.blocks, x, rows, cols, n as usize, &self.stream)
            }
            (_, 1) => self
                .kernels
                .gemv_q6_k_f16(out, &w.blocks, x, rows, cols, &self.stream),
            (_, n) => {
                self.kernels
                    .gemm_q6_k_f16(out, &w.blocks, x, rows, cols, n as usize, &self.stream)
            }
        }
    }
}

fn upload(device: &dyn Device, bytes: &[u8]) -> Result<DevBuffer> {
    let buf = device.alloc(bytes.len().max(1), MemKind::Device, Pool::Weights)?;
    device.write(bytes, &buf, 0)?;
    Ok(buf)
}
