// ===== File: dense.rs — a dense decoder as an order of operations =====
//
// One token in, one row of logits out. The layer order, the position handling
// and the mapping from a checkpoint's roles onto weights live here — and
// NOTHING else does. There is no buffer, no stream and no device in this file,
// and that is not tidiness: a model that holds buffers is a model for one card,
// which is how this repository ended up with two files describing the same
// forty layers (docs/PRZEGLAD_UKLADU.md).
//
// So the model emits `Op` and holds `WeightId`. Whether the multiply underneath
// is a vector kernel, a matrix-unit tile or a tile shared with the CPU is a
// decision of the executor and of the variant registry, taken from measurements
// this file is not allowed to see.

use std::path::Path;

use forge_formats::affine::{permute_rope_rows, to_affine_triple, AffineTriple};
use forge_formats::checkpoint::Checkpoint;
use forge_formats::source::TensorSource;
use forge_formats::WeightRole;
use forge_graph::{Act, ExecSpec, Executor, Op, Tile, WeightId, WeightStore};
use forge_types::{DType, DenseShape, ForgeError, Result};

/// A dense decoder running on `E`.
pub struct Dense<E> {
    exec: E,
    shape: DenseShape,
    seq_cap: u32,
    tile: Tile,
    embed: WeightId,
    layers: Vec<LayerIds>,
    final_norm: WeightId,
    lm_head: WeightId,
    position: u32,
}

/// Role wag jednej warstwy, po indeksie. Bez ani jednego bufora.
struct LayerIds {
    attn_norm: WeightId,
    q: WeightId,
    k: WeightId,
    v: WeightId,
    o: WeightId,
    ffn_norm: WeightId,
    gate: WeightId,
    up: WeightId,
    down: WeightId,
}

impl<E: Executor + WeightStore> Dense<E> {
    /// Reads a checkpoint and hands its weights to an executor built by `make`.
    ///
    /// The executor arrives as a factory rather than as an argument because it
    /// cannot exist before the checkpoint has been read: how many layers to
    /// give a KV cache, and which dtype to compile the kernels for, are answers
    /// this function has and the caller does not.
    pub fn load<F>(path: &Path, make: F) -> Result<Self>
    where
        F: FnOnce(ExecSpec) -> Result<E>,
    {
        // Format checkpointu jest pytaniem do warstwy formatów, nie do modelu.
        // Katalog safetensors, eksport MLX i pojedynczy GGUF wchodzą TĄ SAMĄ
        // drogą; model widzi tylko role i bajty, więc nie ma tu ani jednej
        // gałęzi „jeśli GGUF".
        let ckpt = Checkpoint::open(path)?;
        let desc = ckpt.descriptor();
        let src = ckpt.source();
        let p = &desc.params;

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
        };
        if shape.heads * shape.head_dim != shape.hidden {
            return Err(ForgeError::Unsupported(format!(
                "hidden {} nie jest iloczynem {} głowic po {}",
                shape.hidden, shape.heads, shape.head_dim
            )));
        }

        // DWA RÓŻNE typy, dotąd mylone w jeden, bo w MLX oba są bf16. Typ
        // parametrów kwantyzacji jest własnością źródła, więc wystarczy zapytać
        // PIERWSZĄ wagę; źródło, które nie oddaje postaci afinicznej wprost,
        // przejdzie przez przepisanie, a to zawsze daje f16.
        let embd = &desc.globals[&WeightRole::TokenEmbd];
        let quant_params = match src.fetch_affine(embd)? {
            Some(t) => t.param_dtype,
            None => DType::F16,
        };
        let norm_name = &desc.globals[&WeightRole::OutputNorm];
        let norm_weights = src.fetch(norm_name)?.1;

        let mut exec = make(ExecSpec {
            shape,
            quant_params,
            norm_weights,
        })?;
        let seq_cap = exec.seq_cap();
        let tile = exec.tile();

        // GGUF trzyma wiersze Q i K w kolejności llama.cpp, bo tam RoPE liczy na
        // przeplatanych parach. Nasz kernel obraca połówki, więc dla takiego
        // źródła trzeba je przestawić RAZ, przy ładowaniu. Warunek stawiają
        // wspólne warstwy: architektura mówi, że tego wymaga, a źródło mówi, że
        // jeszcze tego nie zrobiło.
        let rope = (desc.rope_interleaved() && src.stores_original_rope_order())
            .then_some(p.head_dim);

        let (h, kvw, inter) = (shape.hidden, shape.kv_width(), shape.inter);
        let src = &*src;
        let embed = quant(&mut exec, src, embd, shape.vocab, h, None)?;
        let final_norm = plain(&mut exec, src, norm_name)?;
        let head = &desc.globals[&WeightRole::LmHead];
        let lm_head = quant(&mut exec, src, head, shape.vocab, h, None)?;

        let mut layers = Vec::with_capacity(desc.layers.len());
        for l in &desc.layers {
            let name = |role: WeightRole| -> &str { l[&role].as_str() };
            layers.push(LayerIds {
                attn_norm: plain(&mut exec, src, name(WeightRole::AttnNorm))?,
                q: quant(&mut exec, src, name(WeightRole::AttnQ), h, h, rope)?,
                k: quant(&mut exec, src, name(WeightRole::AttnK), kvw, h, rope)?,
                v: quant(&mut exec, src, name(WeightRole::AttnV), kvw, h, None)?,
                o: quant(&mut exec, src, name(WeightRole::AttnO), h, h, None)?,
                ffn_norm: plain(&mut exec, src, name(WeightRole::FfnNorm))?,
                gate: quant(&mut exec, src, name(WeightRole::FfnGate), inter, h, None)?,
                up: quant(&mut exec, src, name(WeightRole::FfnUp), inter, h, None)?,
                down: quant(&mut exec, src, name(WeightRole::FfnDown), h, inter, None)?,
            });
        }

        Ok(Self {
            exec,
            shape,
            seq_cap,
            tile,
            embed,
            layers,
            final_norm,
            lm_head,
            position: 0,
        })
    }

    pub fn shape(&self) -> DenseShape {
        self.shape
    }

    pub fn position(&self) -> u32 {
        self.position
    }

    /// Tokens this model feeds through the layers in one pass.
    pub fn max_tokens(&self) -> u32 {
        self.tile.max_tokens
    }

    /// The executor underneath. Its knobs are its own — how a product is split
    /// between the units of one chip is a property of that chip, and a model
    /// that could set it would be a model for that chip.
    pub fn exec_mut(&mut self) -> &mut E {
        &mut self.exec
    }

    /// Current hidden state, in f32. Exists for bisecting a wrong result: with
    /// forty layers between the input and the logits, "the answer is wrong" is
    /// not a lead, and reading the state after a chosen number of layers turns
    /// it into one.
    pub fn hidden_state(&self) -> Result<Vec<f32>> {
        self.exec.read(Act::Hidden, self.shape.hidden as usize)
    }

    /// The logits currently in scratch — whatever the last forward pass left
    /// there, whether it was one token or a whole chunk.
    pub fn logits(&self) -> Result<Vec<f32>> {
        self.exec.read(Act::Logits, self.shape.vocab as usize)
    }

    /// Runs the embedding and the first `layers` blocks, then stops. The token
    /// position is NOT advanced: this is a probe, not a step.
    pub fn probe(&mut self, token: u32, layers: usize) -> Result<Vec<f32>> {
        let (pos, seq) = (self.position, self.position + 1);
        self.exec.run(&Op::Embed {
            table: self.embed,
            tokens: vec![token],
        })?;
        for index in 0..layers.min(self.layers.len()) {
            for op in self.layer_ops(index, pos, seq, 1) {
                self.exec.run(&op)?;
            }
        }
        self.hidden_state()
    }

    /// Forgets the conversation. The cache is not cleared: every read is
    /// bounded by the current position, so stale bytes past it are unreachable.
    pub fn reset(&mut self) {
        self.position = 0;
    }

    /// Feeds one token and returns the logits for the next one.
    pub fn step(&mut self, token: u32) -> Result<Vec<f32>> {
        self.forward(&[token])?;
        self.logits()
    }

    /// Greedy choice done on the device, so the vocabulary never crosses the
    /// bus just to be scanned for its maximum.
    pub fn step_argmax(&mut self, token: u32) -> Result<u32> {
        self.forward(&[token])?;
        self.exec.argmax(Act::Logits)
    }

    /// Feeds a prompt in one pass per chunk and returns the token that follows it.
    ///
    /// This is where a prompt stops costing one full read of the weights per
    /// token. The chunk is bounded by the executor, because how many tokens are
    /// worth carrying at once is a property of its kernels, not of the model.
    pub fn prefill(&mut self, prompt: &[u32]) -> Result<u32> {
        if prompt.is_empty() {
            return Err(ForgeError::Format("pusty prompt".into()));
        }
        // Podział WYRÓWNANY do bloku, o który prosi wykonawca. Bez tego prompt
        // o jeden token dłuższy od wielokrotności bloku każe policzyć drugi
        // blok prawie pusty. Reszta idzie osobnym, krótszym kaflem.
        let block = self.tile.align as usize;
        let chunk = self.tile.max_tokens as usize;
        let aligned = prompt.len() / block * block;
        for part in prompt[..aligned]
            .chunks(chunk)
            .chain(prompt[aligned..].chunks(chunk))
        {
            self.forward(part)?;
            self.exec.sync()?;
        }
        self.exec.argmax(Act::Logits)
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

    /// Runs `tokens` through the whole model, leaving the logits for the LAST
    /// of them in scratch. Positions run from the current one upwards.
    ///
    /// One code path for a prompt chunk and for a single decode step. The batch
    /// size reaches the kernels as an argument, and at one token every kernel
    /// does exactly what it did before this existed — which is what makes the
    /// bit-for-bit agreement between the two forms testable rather than assumed.
    fn forward(&mut self, tokens: &[u32]) -> Result<()> {
        let n = tokens.len() as u32;
        if n == 0 || n > self.tile.max_tokens {
            return Err(ForgeError::Unsupported(format!(
                "kafel {n} tokenów poza zakresem 1..={}",
                self.tile.max_tokens
            )));
        }
        if self.position + n > self.seq_cap {
            return Err(ForgeError::Unsupported(format!(
                "kontekst przekroczył pojemność cache'u ({})",
                self.seq_cap
            )));
        }
        let (pos, seq) = (self.position, self.position + n);

        self.exec.run(&Op::Embed {
            table: self.embed,
            tokens: tokens.to_vec(),
        })?;
        for index in 0..self.layers.len() {
            for op in self.layer_ops(index, pos, seq, n) {
                self.exec.run(&op)?;
            }
        }
        self.exec.run(&Op::RmsNorm {
            out: Act::Norm,
            x: Act::Hidden,
            w: self.final_norm,
            tokens: n,
        })?;
        // Logity tylko dla ostatniego tokenu kafla: pozostałe wiersze służą
        // wyłącznie zapełnieniu cache'u, a głowa wyjściowa jest z 32 tysiącami
        // wierszy najdroższą pojedynczą macierzą w modelu.
        self.exec.run(&Op::LogitsOfLast {
            w: self.lm_head,
            x: Act::Norm,
            tokens: n,
        })?;
        // Licznik pozycji przechodzi przez to jedno miejsce. Drugi kafel prefillu
        // musi zacząć tam, gdzie skończył pierwszy, a odejmowanie jedynki „na
        // koniec" jest poprawne wyłącznie dla ostatniego z nich.
        self.position += n;
        Ok(())
    }

    /// Jedna warstwa jako CIĄG OPERACJI.
    ///
    /// Zwraca dane, a nie wykonuje — dzięki temu ten sam opis da się później
    /// przepisać przed wykonaniem (złączyć, przestawić, dobrać wariant) bez
    /// dotykania modelu, i dzięki temu model nie ma czym nazwać bufora.
    fn layer_ops(&self, index: usize, pos: u32, seq: u32, tokens: u32) -> Vec<Op> {
        let s = self.shape;
        let l = &self.layers[index];
        vec![
            Op::RmsNorm { out: Act::Norm, x: Act::Hidden, w: l.attn_norm, tokens },
            Op::MatMul { out: Act::Query, w: l.q, x: Act::Norm, tokens },
            Op::MatMul { out: Act::Key, w: l.k, x: Act::Norm, tokens },
            Op::MatMul { out: Act::Value, w: l.v, x: Act::Norm, tokens },
            Op::Rope { act: Act::Query, heads: s.heads, pos, tokens },
            Op::Rope { act: Act::Key, heads: s.kv_heads, pos, tokens },
            Op::KvAppend { layer: index, pos, tokens },
            Op::Attention { layer: index, seq, tokens },
            Op::MatMul { out: Act::Proj, w: l.o, x: Act::Attn, tokens },
            Op::Residual { src: Act::Proj, tokens },
            Op::RmsNorm { out: Act::Norm, x: Act::Hidden, w: l.ffn_norm, tokens },
            Op::MatMul { out: Act::Gate, w: l.gate, x: Act::Norm, tokens },
            Op::MatMul { out: Act::Up, w: l.up, x: Act::Norm, tokens },
            Op::SiluMul { tokens },
            Op::MatMul { out: Act::Proj, w: l.down, x: Act::Activated, tokens },
            Op::Residual { src: Act::Proj, tokens },
        ]
    }
}

/// Hands one quantized weight to the executor and keeps its id.
///
/// The executor's own checks say what is wrong; only the caller knows WHICH
/// tensor it was, so the name is put back on the way out. With forty layers of
/// nine weights each, "the parameters are the wrong dtype" without a name is
/// half a diagnosis.
fn quant<E: WeightStore>(
    exec: &mut E,
    src: &dyn TensorSource,
    name: &str,
    rows: u32,
    cols: u32,
    rope: Option<usize>,
) -> Result<WeightId> {
    let t = affine(src, name, rows, cols, rope)?;
    exec.put_affine(&t)
        .map_err(|e| ForgeError::Format(format!("{name}: {e}")))
}

/// The same, for a weight that is not quantized — the norms.
fn plain<E: WeightStore>(exec: &mut E, src: &dyn TensorSource, name: &str) -> Result<WeightId> {
    exec.put_plain(&src.fetch(name)?.0)
}

/// One weight in the affine form every backend indexes.
///
/// A source that already stores it that way hands it over; the rest go through
/// a rewrite. Either way the executor above gets the same thing, which is the
/// whole reason a second format costs no branch in the model.
///
/// `rope` carries the head width when the rows still need permuting, and
/// nothing when they do not — an `Option` rather than a `bool` plus a width,
/// because those two can disagree and this cannot.
fn affine(
    src: &dyn TensorSource,
    name: &str,
    rows: u32,
    cols: u32,
    rope: Option<usize>,
) -> Result<AffineTriple> {
    let mut t = match src.fetch_affine(name)? {
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
    if let Some(head_dim) = rope {
        permute_rope_rows(&mut t, head_dim)?;
    }
    Ok(t)
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
