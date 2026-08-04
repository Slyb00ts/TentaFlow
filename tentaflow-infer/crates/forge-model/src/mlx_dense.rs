// ===== File: mlx_dense.rs — decode loop for a dense MLX checkpoint =====
//
// One token in, one row of logits out, on the GPU. The layer order, the KV
// cache and the position handling live here; every arithmetic step is a kernel
// from forge-kernels, already pinned against MLX on its own.
//
// Two properties are deliberate:
//
//   * A whole step goes into ONE command buffer. Around 500 dispatches at
//     0.61 us each is 0.3 ms of overhead per token; the same work as separate
//     command buffers would be 10 ms, and as host round trips, 47 ms
//     (docs/pomiary/eks-a1-a3-apple-m4.md).
//   * Weights are uploaded quantized and dequantized inside the kernels. A
//     dequantized copy of this checkpoint would be 16 GB against 4.2, and
//     reading the weights once IS the cost of a decode step.

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use forge_formats::affine::{to_affine_triple, GGML_AFFINE_GROUP};
use forge_formats::checkpoint::Checkpoint;
use forge_formats::WeightRole;
use forge_hal::{DevBuffer, Device, Event, KernelHandle, LaunchArgs, LaunchConfig, Pool, Stream};
use forge_kernels::msl::{self, OutDtype, ScaleDtype};
use forge_kernels::variant::{
    self, AttentionForm, MatmulForm, Problem, RowSplit, ATTENTION_FORMS, MATMUL_FORMS,
};
use forge_types::{DType, ForgeError, MemKind, Result};

use crate::cpu_matmul::{CpuMatmul, Operands};

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
pub const PREFILL_CHUNK: u32 = 1024;

/// Everything the decode loop needs to know about the architecture.
#[derive(Debug, Clone, Copy)]
pub struct DenseShape {
    pub hidden: u32,
    pub layers: u32,
    pub heads: u32,
    pub kv_heads: u32,
    pub head_dim: u32,
    pub inter: u32,
    pub vocab: u32,
    pub eps: f32,
    pub rope_theta: f32,
    pub group: u32,
}

impl DenseShape {
    fn kv_width(&self) -> u32 {
        self.kv_heads * self.head_dim
    }

    fn attn_scale(&self) -> f32 {
        1.0 / (self.head_dim as f32).sqrt()
    }
}

/// A quantized weight: packed nibbles plus per-group scale and zero point.
struct Quantized {
    packed: DevBuffer,
    scales: DevBuffer,
    biases: DevBuffer,
    rows: u32,
    cols: u32,
}

struct Layer {
    attn_norm: DevBuffer,
    q: Quantized,
    k: Quantized,
    v: Quantized,
    o: Quantized,
    ffn_norm: DevBuffer,
    gate: Quantized,
    up: Quantized,
    down: Quantized,
    k_cache: DevBuffer,
    v_cache: DevBuffer,
}

struct Pipelines {
    qmv_f16: KernelHandle,
    qmv_f32: KernelHandle,
    qmm_f16: KernelHandle,
    qmm_f32: KernelHandle,
    qmg_f16: KernelHandle,
    qmg_f32: KernelHandle,
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

pub struct MlxDense {
    device: Arc<dyn Device>,
    stream: Stream,
    shape: DenseShape,
    seq_cap: u32,
    embed: Quantized,
    layers: Vec<Layer>,
    final_norm: DevBuffer,
    lm_head: Quantized,
    pipes: Pipelines,
    scratch: Scratch,
    position: u32,
    /// Scratch for the CPU's share of a split product. Behind a cell because
    /// the whole forward pass runs through `&self` and this is the only piece
    /// of it that has to be written.
    cpu: RefCell<CpuMatmul>,
    /// Names the command buffer a split submitted, so the wait is for THAT
    /// work rather than for the whole queue.
    split_event: Event,
    /// Whether prefill may hand part of a product to the CPU. See
    /// `set_cpu_prefill`.
    cpu_share: bool,
}

impl MlxDense {
    /// Loads a checkpoint onto `device`. The KV cache is sized once, at the
    /// kernel's declared bound, because a cache that grows mid-run would mean
    /// reallocating inside a decode step.
    pub fn load(device: Arc<dyn Device>, path: &Path) -> Result<Self> {
        // Format checkpointu jest pytaniem do warstwy formatów, nie do modelu.
        // Katalog safetensors, eksport MLX i pojedynczy GGUF wchodzą TĄ SAMĄ
        // drogą; model widzi tylko role i bajty, więc nie ma tu ani jednej
        // gałęzi „jeśli GGUF".
        let ckpt = Checkpoint::open(path)?;
        let desc = ckpt.descriptor();
        let src = ckpt.source();
        let p = &desc.params;

        // Grupa i typ skal to własność ŹRÓDŁA, nie modelu: MLX ma 64 i bf16,
        // GGML po przepisaniu 32 i f16. Odczytane z pierwszej wagi i potem
        // egzekwowane na każdej kolejnej, żeby model nie liczył dwóch układów
        // naraz.
        let probe = desc.globals[&WeightRole::TokenEmbd].clone();
        let (group, scales_dtype) = match src.fetch_affine(&probe)? {
            Some(t) => (t.group as u32, dtype_of(t.param_dtype)?),
            None => (GGML_AFFINE_GROUP as u32, ScaleDtype::F16),
        };

        let shape = DenseShape {
            hidden: p.hidden_size as u32,
            layers: desc.layers.len() as u32,
            heads: p.n_heads as u32,
            kv_heads: p.n_kv_heads as u32,
            head_dim: p.head_dim as u32,
            inter: p.intermediate_size as u32,
            vocab: p.vocab_size as u32,
            eps: p.rms_norm_eps,
            rope_theta: p.rope_theta,
            // Po konwersji każdy format ma jedną grupę — to właśnie ona jest
            // wspólnym mianownikiem, do którego sprowadza je `forge-formats`.
            group,
        };
        if shape.heads * shape.head_dim != shape.hidden {
            return Err(ForgeError::Unsupported(format!(
                "hidden {} nie jest iloczynem {} głowic po {}",
                shape.hidden, shape.heads, shape.head_dim
            )));
        }

        // DWA RÓŻNE typy, dotąd mylone w jeden, bo w MLX oba są bf16.
        // Skale kwantyzacji są f16, bo taka jest trójka po konwersji — to
        // własność wspólnego formatu, nie checkpointu. Waga normy zostaje
        // natomiast w typie, w jakim leży w pliku, i ten trzeba odczytać.
        let norm_dtype = match src.fetch(&desc.globals[&WeightRole::OutputNorm])?.1 {
            DType::F16 => ScaleDtype::F16,
            DType::BF16 => ScaleDtype::Bf16,
            other => {
                return Err(ForgeError::Unsupported(format!(
                    "waga normy ma typ {other:?}, a kernel zna f16 i bf16"
                )))
            }
        };
        let seq_cap = msl::ATTN_MAX_SEQ;

        let quantized = |name: &str, rows: u32, cols: u32| -> Result<Quantized> {
            // Źródło, które trzyma wagi afinicznie, oddaje je wprost; reszta
            // przechodzi przez przepisanie. W obu razach model dostaje to samo.
            let t = match src.fetch_affine(name)? {
                Some(t) => t,
                None => {
                    let (data, _, quant, dims) = src.fetch(name)?;
                    if dims != vec![rows as usize, cols as usize] {
                        return Err(ForgeError::Format(format!(
                            "{name}: kształt {dims:?}, oczekiwano [{rows}, {cols}]"
                        )));
                    }
                    to_affine_triple(&data, quant, rows as usize, cols as usize)?
                }
            };
            if t.rows != rows as usize || t.cols != cols as usize {
                return Err(ForgeError::Format(format!(
                    "{name}: kształt [{}, {}], oczekiwano [{rows}, {cols}]",
                    t.rows, t.cols
                )));
            }
            if t.group != group as usize {
                return Err(ForgeError::Unsupported(format!(
                    "{name}: grupa {} wobec {group} w reszcie modelu",
                    t.group
                )));
            }
            Ok(Quantized {
                packed: upload(&*device, bytemuck::cast_slice(&t.packed))?,
                scales: upload(&*device, &t.scales)?,
                biases: upload(&*device, &t.biases)?,
                rows,
                cols,
            })
        };
        let plain = |name: &str| -> Result<DevBuffer> {
            let (data, _, _, _) = src.fetch(name)?;
            upload(&*device, &data)
        };

        let embed = quantized(&desc.globals[&WeightRole::TokenEmbd], shape.vocab, shape.hidden)?;
        let final_norm = plain(&desc.globals[&WeightRole::OutputNorm])?;
        let lm_head = quantized(&desc.globals[&WeightRole::LmHead], shape.vocab, shape.hidden)?;

        let kv_bytes = (shape.kv_heads * seq_cap * shape.head_dim) as usize * 2;
        let mut layers = Vec::with_capacity(shape.layers as usize);
        for l in &desc.layers {
            let name = |role: WeightRole| -> &str { l[&role].as_str() };
            layers.push(Layer {
                attn_norm: plain(name(WeightRole::AttnNorm))?,
                q: quantized(name(WeightRole::AttnQ), shape.hidden, shape.hidden)?,
                k: quantized(name(WeightRole::AttnK), shape.kv_width(), shape.hidden)?,
                v: quantized(name(WeightRole::AttnV), shape.kv_width(), shape.hidden)?,
                o: quantized(name(WeightRole::AttnO), shape.hidden, shape.hidden)?,
                ffn_norm: plain(name(WeightRole::FfnNorm))?,
                gate: quantized(name(WeightRole::FfnGate), shape.inter, shape.hidden)?,
                up: quantized(name(WeightRole::FfnUp), shape.inter, shape.hidden)?,
                down: quantized(name(WeightRole::FfnDown), shape.hidden, shape.inter)?,
                k_cache: device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?,
                v_cache: device.alloc(kv_bytes, MemKind::Device, Pool::KvCache)?,
            });
        }

        let compile = |source: &str, entry: &str| -> Result<KernelHandle> {
            device.load_module(source.as_bytes())?.kernel(entry)
        };
        let pipes = Pipelines {
            qmv_f16: compile(
                &msl::qmv_affine_source(msl::Bits::Four, scales_dtype, OutDtype::F16),
                &msl::qmv_affine_name(msl::Bits::Four, scales_dtype, OutDtype::F16),
            )?,
            qmv_f32: compile(
                &msl::qmv_affine_source(msl::Bits::Four, scales_dtype, OutDtype::F32),
                &msl::qmv_affine_name(msl::Bits::Four, scales_dtype, OutDtype::F32),
            )?,
            qmm_f16: compile(
                &msl::qmm_affine_4bit_source(scales_dtype, OutDtype::F16),
                &msl::qmm_affine_4bit_name(scales_dtype, OutDtype::F16),
            )?,
            qmm_f32: compile(
                &msl::qmm_affine_4bit_source(scales_dtype, OutDtype::F32),
                &msl::qmm_affine_4bit_name(scales_dtype, OutDtype::F32),
            )?,
            qmg_f16: compile(
                &msl::qmg_affine_4bit_source(scales_dtype, OutDtype::F16),
                &msl::qmg_affine_4bit_name(scales_dtype, OutDtype::F16),
            )?,
            qmg_f32: compile(
                &msl::qmg_affine_4bit_source(scales_dtype, OutDtype::F32),
                &msl::qmg_affine_4bit_name(scales_dtype, OutDtype::F32),
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

        let f16 = |elems: u32| device.alloc(elems as usize * 2, MemKind::Device, Pool::Activations);
        let f32b = |elems: u32| device.alloc(elems as usize * 4, MemKind::Device, Pool::Activations);
        // Wszystko poza logitami ma miejsce na cały kafel prefillu: dekodowanie
        // używa pierwszego wiersza tych samych buforów. Logity liczymy tylko dla
        // ostatniego tokenu, więc zostają jednym wierszem — 32 tys. kolumn razy
        // 128 byłoby 16 MB na coś, z czego czytamy 1/128.
        let n = PREFILL_CHUNK;
        let scratch = Scratch {
            h: f16(n * shape.hidden)?,
            norm: f16(n * shape.hidden)?,
            q: f16(n * shape.hidden)?,
            k: f16(n * shape.kv_width())?,
            v: f16(n * shape.kv_width())?,
            attn: f16(n * shape.hidden)?,
            proj: f32b(n * shape.hidden)?,
            gate: f16(n * shape.inter)?,
            up: f16(n * shape.inter)?,
            act: f16(n * shape.inter)?,
            logits: f32b(shape.vocab)?,
            token: f32b(1)?,
            ids: device.alloc(n as usize * 4, MemKind::Device, Pool::Activations)?,
        };

        let stream = device.create_stream()?;
        let split_event = device.create_event()?;
        Ok(Self {
            device,
            stream,
            shape,
            seq_cap,
            embed,
            layers,
            final_norm,
            lm_head,
            pipes,
            scratch,
            position: 0,
            cpu: RefCell::new(CpuMatmul::new()),
            split_event,
            cpu_share: true,
        })
    }

    pub fn shape(&self) -> DenseShape {
        self.shape
    }

    pub fn position(&self) -> u32 {
        self.position
    }

    /// Current hidden state, in f32. Exists for bisecting a wrong result: with
    /// forty layers between the input and the logits, "the answer is wrong" is
    /// not a lead, and reading the state after a chosen number of layers turns
    /// it into one.
    pub fn hidden_state(&self) -> Result<Vec<f32>> {
        let mut raw = vec![0u8; self.shape.hidden as usize * 2];
        self.device.read(&self.scratch.h, 0, &mut raw)?;
        Ok(raw
            .chunks_exact(2)
            .map(|c| {
                let bits = u16::from_le_bytes([c[0], c[1]]);
                f16_to_f32(bits)
            })
            .collect())
    }

    /// Runs the embedding and the first `layers` blocks, then stops. The token
    /// position is NOT advanced: this is a probe, not a step.
    pub fn probe(&mut self, token: u32, layers: usize) -> Result<Vec<f32>> {
        let s = self.shape;
        let (pos, seq) = (self.position, self.position + 1);
        self.device.write(&token.to_le_bytes(), &self.scratch.ids, 0)?;
        self.launch(
            &self.pipes.embed,
            LaunchArgs::new()
                .buf(&self.scratch.h)
                .buf(&self.embed.packed)
                .buf(&self.embed.scales)
                .buf(&self.embed.biases)
                .buf(&self.scratch.ids)
                .scalar(s.hidden)
                .scalar(s.group)
                .scalar(1u32),
            msl::elementwise_groups(s.hidden),
            msl::ELEMENTWISE_THREADS,
        )?;
        for index in 0..layers.min(self.layers.len()) {
            self.layer(index, pos, seq, 1)?;
        }
        self.stream.synchronize()?;
        self.hidden_state()
    }

    /// Forgets the conversation. The cache is not cleared: every read is
    /// bounded by the current position, so stale bytes past it are unreachable.
    pub fn reset(&mut self) {
        self.position = 0;
    }

    /// Turns the CPU's share of prefill on or off. On by default.
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
    /// Decode never takes this path whatever this is set to — it is bandwidth
    /// bound, and adding compute there measured -14%.
    pub fn set_cpu_prefill(&mut self, on: bool) {
        self.cpu_share = on;
    }

    /// The logits currently in the scratch buffer — whatever the last forward
    /// pass left there, whether it was one token or a whole chunk.
    pub fn logits(&self) -> Result<Vec<f32>> {
        let mut raw = vec![0u8; self.shape.vocab as usize * 4];
        self.device.read(&self.scratch.logits, 0, &mut raw)?;
        Ok(raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect())
    }

    /// Feeds one token and returns the logits for the next one.
    ///
    /// Every dispatch lands in a single command buffer and the host waits once,
    /// at the end. That is the whole reason the backend exposes a command
    /// buffer as an object rather than hiding it inside "launch".
    pub fn step(&mut self, token: u32) -> Result<Vec<f32>> {
        self.forward(&[token])?;
        self.stream.synchronize()?;
        self.logits()
    }

    /// Greedy choice done on the device, so the vocabulary never crosses the
    /// bus just to be scanned for its maximum.
    pub fn step_argmax(&mut self, token: u32) -> Result<u32> {
        self.step_no_readback(token)?;
        self.argmax_of_logits()
    }

    /// Feeds a prompt in one pass per chunk and returns the token that follows it.
    ///
    /// This is where a prompt stops costing one full read of the weights per
    /// token. The chunk is bounded because past it the batched matmul stops
    /// paying off, so a long prompt is several chunks rather than one launch.
    pub fn prefill(&mut self, prompt: &[u32]) -> Result<u32> {
        if prompt.is_empty() {
            return Err(ForgeError::Format("pusty prompt".into()));
        }
        // Podział WYRÓWNANY do bloku formy macierzowej. Bez tego prompt o
        // jeden token dłuższy od wielokrotności bloku każe policzyć drugi blok
        // prawie pusty: zmierzone 5227 us/token przy 64 tokenach i 9318 przy 65,
        // czyli o jeden token więcej za prawie dwa razy tyle pracy. Reszta idzie
        // osobnym, krótszym kaflem, który rejestr obsłuży formą blokową.
        let block = msl::QMG_BM as usize;
        let aligned = prompt.len() / block * block;
        for chunk in prompt[..aligned].chunks(PREFILL_CHUNK as usize) {
            self.forward(chunk)?;
            self.stream.synchronize()?;
        }
        for chunk in prompt[aligned..].chunks(PREFILL_CHUNK as usize) {
            self.forward(chunk)?;
            self.stream.synchronize()?;
        }
        self.argmax_of_logits()
    }

    fn argmax_of_logits(&mut self) -> Result<u32> {
        self.launch(
            &self.pipes.argmax,
            LaunchArgs::new()
                .buf(&self.scratch.token)
                .buf(&self.scratch.logits)
                .scalar(self.shape.vocab),
            1,
            msl::ARGMAX_THREADS,
        )?;
        self.stream.synchronize()?;
        let mut raw = [0u8; 4];
        self.device.read(&self.scratch.token, 0, &mut raw)?;
        Ok(u32::from_le_bytes(raw))
    }

    /// Feeds a prompt and continues it greedily.
    pub fn generate(&mut self, prompt: &[u32], max_new: usize) -> Result<Vec<u32>> {
        let mut next = self.prefill(prompt)?;
        let mut out = Vec::with_capacity(max_new);
        out.push(next);
        for _ in 1..max_new {
            next = self.step_argmax(next)?;
            out.push(next);
        }
        Ok(out)
    }

    fn step_no_readback(&mut self, token: u32) -> Result<()> {
        self.forward(&[token])
    }

    /// Runs `tokens` through the whole model, leaving the logits for the LAST
    /// of them in scratch. Positions run from the current one upwards.
    ///
    /// One code path for a prompt chunk and for a single decode step. The batch
    /// size reaches the kernels as an argument, and at one token every kernel
    /// does exactly what it did before this existed — which is what makes the
    /// bit-for-bit agreement between the two forms testable rather than assumed.
    fn forward(&mut self, tokens: &[u32]) -> Result<()> {
        let n = tokens.len() as u32;
        if n == 0 || n > PREFILL_CHUNK {
            return Err(ForgeError::Unsupported(format!(
                "kafel {n} tokenów poza zakresem 1..={PREFILL_CHUNK}"
            )));
        }
        if self.position + n > self.seq_cap {
            return Err(ForgeError::Unsupported(format!(
                "kontekst przekroczył pojemność cache'u ({})",
                self.seq_cap
            )));
        }
        let s = self.shape;
        let (pos, seq) = (self.position, self.position + n);

        let ids: Vec<u8> = tokens.iter().flat_map(|t| t.to_le_bytes()).collect();
        self.device.write(&ids, &self.scratch.ids, 0)?;

        self.launch(
            &self.pipes.embed,
            LaunchArgs::new()
                .buf(&self.scratch.h)
                .buf(&self.embed.packed)
                .buf(&self.embed.scales)
                .buf(&self.embed.biases)
                .buf(&self.scratch.ids)
                .scalar(s.hidden)
                .scalar(s.group)
                .scalar(n),
            msl::elementwise_groups(n * s.hidden),
            msl::ELEMENTWISE_THREADS,
        )?;
        for index in 0..self.layers.len() {
            self.layer(index, pos, seq, n)?;
        }
        self.rmsnorm(&self.scratch.norm, &self.scratch.h, &self.final_norm, n)?;

        // Logity tylko dla ostatniego tokenu kafla: pozostałe wiersze służą
        // wyłącznie zapełnieniu cache'u, a głowa wyjściowa jest z 32 tysiącami
        // wierszy najdroższą pojedynczą macierzą w modelu.
        let last = (n - 1) as usize * s.hidden as usize * 2;
        self.gemv(
            &self.pipes.qmv_f32,
            &self.scratch.logits,
            &self.lm_head,
            &self.scratch.norm,
            last,
        )?;
        // Licznik pozycji przechodzi przez to jedno miejsce. Drugi kafel prefillu
        // musi zacząć tam, gdzie skończył pierwszy, a odejmowanie jedynki „na
        // koniec" jest poprawne wyłącznie dla ostatniego z nich.
        self.position += n;
        Ok(())
    }

    fn layer(&self, index: usize, pos: u32, seq: u32, tokens: u32) -> Result<()> {
        let s = self.shape;
        let l = &self.layers[index];

        self.rmsnorm(&self.scratch.norm, &self.scratch.h, &l.attn_norm, tokens)?;
        self.matmul(&self.scratch.q, &l.q, &self.scratch.norm, tokens, true)?;
        self.matmul(&self.scratch.k, &l.k, &self.scratch.norm, tokens, true)?;
        self.matmul(&self.scratch.v, &l.v, &self.scratch.norm, tokens, true)?;

        self.rope(&self.scratch.q, s.heads, pos, tokens)?;
        self.rope(&self.scratch.k, s.kv_heads, pos, tokens)?;
        self.kv_append(&l.k_cache, &self.scratch.k, pos, tokens)?;
        self.kv_append(&l.v_cache, &self.scratch.v, pos, tokens)?;

        // Która forma uwagi obsługuje który wsad — decyduje rejestr, tak samo
        // jak przy mnożeniu. Zgodność obu form jest przypięta testem, a forma
        // per-token jest przypięta do MLX.
        let attn = ATTENTION_FORMS
            .pick(&Problem {
                tokens,
                rows: s.heads,
                cols: s.head_dim,
            })
            .ok_or_else(|| ForgeError::Unsupported("brak wariantu uwagi".into()))?;
        let (kernel, groups, threads) = match attn.form {
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
                .buf(&self.scratch.attn)
                .buf(&self.scratch.q)
                .buf(&l.k_cache)
                .buf(&l.v_cache)
                .scalar(s.heads)
                .scalar(s.kv_heads)
                .scalar(seq)
                .scalar(self.seq_cap)
                .scalar(s.attn_scale())
                .scalar(tokens),
            groups,
            threads,
        )?;

        self.matmul(&self.scratch.proj, &l.o, &self.scratch.attn, tokens, false)?;
        self.residual(&self.scratch.proj, tokens)?;

        self.rmsnorm(&self.scratch.norm, &self.scratch.h, &l.ffn_norm, tokens)?;
        self.matmul(&self.scratch.gate, &l.gate, &self.scratch.norm, tokens, true)?;
        self.matmul(&self.scratch.up, &l.up, &self.scratch.norm, tokens, true)?;
        self.launch(
            &self.pipes.silu_mul,
            LaunchArgs::new()
                .buf(&self.scratch.act)
                .buf(&self.scratch.gate)
                .buf(&self.scratch.up)
                .scalar(tokens * s.inter),
            msl::silu_mul_groups(tokens * s.inter),
            msl::SILU_MUL_THREADS,
        )?;
        self.matmul(&self.scratch.proj, &l.down, &self.scratch.act, tokens, false)?;
        self.residual(&self.scratch.proj, tokens)
    }

    fn rmsnorm(
        &self,
        out: &DevBuffer,
        input: &DevBuffer,
        weight: &DevBuffer,
        tokens: u32,
    ) -> Result<()> {
        self.launch(
            &self.pipes.rmsnorm,
            LaunchArgs::new()
                .buf(out)
                .buf(input)
                .buf(weight)
                .scalar(self.shape.hidden)
                .scalar(self.shape.eps),
            tokens,
            msl::RMSNORM_THREADS,
        )
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
            LaunchArgs::new()
                .buf(out)
                .buf(&w.packed)
                .buf(&w.scales)
                .buf(&w.biases)
                .buf_at(x, x_offset)?
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(self.shape.group),
            msl::qmv_affine_4bit_groups(w.rows),
            msl::QMV_THREADS,
        )
    }

    /// Projection for a whole batch.
    ///
    /// Projection for a whole batch.
    ///
    /// Which form serves which batch is a decision of the registry, not of this
    /// function: the thresholds and the measurements behind them live in
    /// `forge_kernels::variant`, where they can be checked for totality and for
    /// the absence of a cliff. Here there is only the mapping from a chosen
    /// form to the pipeline that implements it.
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
        };
        let chosen = MATMUL_FORMS.pick(&problem).ok_or_else(|| {
            ForgeError::Unsupported(format!("brak wariantu mnożenia dla {problem:?}"))
        })?;
        match chosen.form {
            MatmulForm::Vector => {
                let k = if f16_out {
                    &self.pipes.qmv_f16
                } else {
                    &self.pipes.qmv_f32
                };
                self.gemv(k, out, w, x, 0)
            }
            MatmulForm::RegisterBlocked => self.matmul_blocked(out, w, x, tokens, f16_out),
            MatmulForm::MatrixUnits => {
                self.matmul_matrix_units(out, w, x, tokens, f16_out, None)
            }
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
        let k = if f16_out {
            &self.pipes.qmg_f16
        } else {
            &self.pipes.qmg_f32
        };
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
            group: self.shape.group,
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
            &LaunchArgs::new()
                .buf(out)
                .buf(&w.packed)
                .buf(&w.scales)
                .buf(&w.biases)
                .buf(x)
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(self.shape.group)
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
        let k = if f16_out {
            &self.pipes.qmm_f16
        } else {
            &self.pipes.qmm_f32
        };
        let (gx, gy) = msl::qmm_affine_4bit_groups(w.rows, tokens);
        self.device.launch(
            k,
            &LaunchConfig {
                grid: (gx, gy, 1),
                block: (msl::QMM_THREADS, 1, 1),
                shared_mem_bytes: 0,
            },
            &LaunchArgs::new()
                .buf(out)
                .buf(&w.packed)
                .buf(&w.scales)
                .buf(&w.biases)
                .buf(x)
                .scalar(w.rows)
                .scalar(w.cols)
                .scalar(self.shape.group)
                .scalar(tokens),
            &self.stream,
        )
    }

    fn rope(&self, buf: &DevBuffer, heads: u32, pos: u32, tokens: u32) -> Result<()> {
        let threads = msl::ELEMENTWISE_THREADS;
        self.launch(
            &self.pipes.rope,
            LaunchArgs::new()
                .buf(buf)
                .scalar(heads)
                .scalar(self.shape.head_dim)
                .scalar(pos)
                .scalar(self.shape.rope_theta)
                .scalar(tokens),
            msl::rope_groups(heads, self.shape.head_dim, tokens, threads),
            threads,
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

    /// `h += delta`, in place. The kernel reads and writes the same index, so
    /// aliasing the output onto the input is safe and saves a buffer that would
    /// otherwise be copied every layer.
    fn residual(&self, delta: &DevBuffer, tokens: u32) -> Result<()> {
        self.launch(
            &self.pipes.residual,
            LaunchArgs::new()
                .buf(&self.scratch.h)
                .buf(&self.scratch.h)
                .buf(delta)
                .scalar(tokens * self.shape.hidden),
            msl::elementwise_groups(tokens * self.shape.hidden),
            msl::ELEMENTWISE_THREADS,
        )
    }

    fn launch(&self, kernel: &KernelHandle, args: LaunchArgs, groups: u32, threads: u32)
        -> Result<()> {
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

/// Widens an f16 bit pattern without pulling a dependency in for it.
fn f16_to_f32(bits: u16) -> f32 {
    let sign = ((bits >> 15) & 1) as u32;
    let exp = ((bits >> 10) & 0x1f) as u32;
    let frac = (bits & 0x3ff) as u32;
    let out = match exp {
        0 if frac == 0 => sign << 31,
        0 => {
            // Subnormal: normalise by shifting until the implicit bit appears.
            let mut e = -1i32;
            let mut f = frac;
            while f & 0x400 == 0 {
                f <<= 1;
                e -= 1;
            }
            let exp32 = (127 - 15 + e + 1) as u32;
            (sign << 31) | (exp32 << 23) | ((f & 0x3ff) << 13)
        }
        0x1f => (sign << 31) | (0xff << 23) | (frac << 13),
        _ => (sign << 31) | ((exp + 127 - 15) << 23) | (frac << 13),
    };
    f32::from_bits(out)
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
    Ok(unsafe { std::slice::from_raw_parts(ptr as *const T, buf.len() / std::mem::size_of::<T>()) })
}

/// Typ parametrów kwantyzacji w wersji, którą znają kernele.
fn dtype_of(d: DType) -> Result<ScaleDtype> {
    match d {
        DType::F16 => Ok(ScaleDtype::F16),
        DType::BF16 => Ok(ScaleDtype::Bf16),
        other => Err(ForgeError::Unsupported(format!(
            "skale w {other:?}, a kernel zna f16 i bf16"
        ))),
    }
}

fn upload(device: &dyn Device, bytes: &[u8]) -> Result<DevBuffer> {
    let buf = device.alloc(bytes.len().max(1), MemKind::Device, Pool::Weights)?;
    device.write(bytes, &buf, 0)?;
    Ok(buf)
}

/// Names of the weights a dense checkpoint must provide, for callers that want
/// to check a file before paying for the upload.
pub fn required_roles() -> &'static [WeightRole] {
    &[
        WeightRole::TokenEmbd,
        WeightRole::OutputNorm,
        WeightRole::LmHead,
        WeightRole::AttnNorm,
        WeightRole::AttnQ,
        WeightRole::AttnK,
        WeightRole::AttnV,
        WeightRole::AttnO,
        WeightRole::FfnNorm,
        WeightRole::FfnGate,
        WeightRole::FfnUp,
        WeightRole::FfnDown,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_derives_the_widths_the_loop_uses() {
        let s = DenseShape {
            hidden: 4096,
            layers: 40,
            heads: 32,
            kv_heads: 8,
            head_dim: 128,
            inter: 11264,
            vocab: 32128,
            eps: 1e-5,
            rope_theta: 1e6,
            group: 64,
        };
        assert_eq!(s.kv_width(), 1024);
        assert!((s.attn_scale() - 0.088_388).abs() < 1e-5);
        // Głowice zapytań muszą wypełnić szerokość ukrytą — inaczej projekcja
        // Q liczy inną liczbę kanałów niż czyta uwaga.
        assert_eq!(s.heads * s.head_dim, s.hidden);
    }

    #[test]
    fn every_role_the_loop_reads_is_declared_required() {
        for role in [
            WeightRole::AttnQ,
            WeightRole::AttnK,
            WeightRole::AttnV,
            WeightRole::AttnO,
            WeightRole::FfnGate,
            WeightRole::FfnUp,
            WeightRole::FfnDown,
            WeightRole::LmHead,
        ] {
            assert!(required_roles().contains(&role), "{role:?}");
        }
    }
}
