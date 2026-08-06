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

use std::collections::HashMap;
use std::path::Path;

use forge_formats::affine::{permute_rope_rows, split_head_interleaved};
use forge_formats::checkpoint::Checkpoint;
use forge_formats::nvfp4::nvfp4_ct_to_gguf_blocks;
use forge_formats::source::TensorSource;
use forge_formats::WeightRole;
use forge_graph::{
    fuse::fuse, Act, ExecSpec, Executor, Lane, Layout, Op, PackedWeight, Planes, QuantWeight,
    Shared, Step, Tile, WeightId, WeightStore,
};
use forge_types::{DType, DenseShape, ForgeError, QuantKind, Result};

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
    /// Pozycja każdego slotu cache'u — ile tokenów już w nim leży.
    ///
    /// Tablica, a nie jedno pole, bo wykonawca trzyma tyle niezależnych
    /// sekwencji, ile slotów zadeklarował. Dopóki było jedno pole, wsad nie
    /// miał gdzie zapisać, że sekwencje stoją w różnych miejscach.
    positions: Vec<u32>,
}

/// Jeden token do dołożenia do sekwencji siedzącej w tym slocie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feed {
    pub slot: usize,
    pub token: u32,
}

/// Role wag jednej warstwy, po indeksie. Bez ani jednego bufora.
struct LayerIds {
    attn_norm: WeightId,
    q: WeightId,
    /// The second half of a gated query projection: what attention returns is
    /// scaled by its sigmoid. `None` for every architecture whose attention
    /// answers unmodified.
    q_gate: Option<WeightId>,
    k: WeightId,
    /// Normy Q i K per głowica. `None` dla architektur, które ich nie mają —
    /// llama i Bielik normalizują wyłącznie strumień rezydualny.
    q_norm: Option<WeightId>,
    k_norm: Option<WeightId>,
    v: WeightId,
    o: WeightId,
    ffn_norm: WeightId,
    ffn: Ffn,
}

/// The two shapes a feed-forward block comes in.
///
/// An enum rather than optional fields, because the two are alternatives and
/// never a mixture: a layer has one FFN or the other, and optional fields would
/// let a checkpoint describe a layer with both, or with neither.
///
/// It is per LAYER and not per model on purpose. Qwen3.6 interleaves attention
/// with DeltaNet and would interleave these too, so the choice belongs where
/// the layer is.
enum Ffn {
    Dense {
        gate: WeightId,
        up: WeightId,
        down: WeightId,
    },
    Moe {
        router: WeightId,
        gate: WeightId,
        up: WeightId,
        down: WeightId,
        experts: u32,
        top_k: u32,
        norm_topk: bool,
        shared: Option<Shared>,
    },
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
            // How much of each head turns. The M-RoPE sections state it for the
            // architectures that rotate only part of a head; everything else
            // rotates all of it.
            rope_rot: p
                .rope_sections
                .map(|s| s.iter().sum::<u32>() * 2)
                .unwrap_or(p.head_dim as u32),
        };
        // Głowice zapytań dzielą się na grupy po jednej głowicy KV, więc kernele
        // uwagi adresują `heads / kv_heads` zapytań na klucz. Niepodzielność
        // rozjeżdża to adresowanie, a nie zatrzymuje kernela.
        //
        // Iloczyn `heads * head_dim` NIE musi natomiast równać się `hidden` —
        // przez ten warunek stała tu wcześniej odmowa. W llamie i w Bieliku obie
        // liczby są równe, ale Qwen3-MoE ma 4096 wobec 2048, więc równość jest
        // własnością tamtych checkpointów, a nie architektury gęstej.
        if shape.kv_heads == 0 || !shape.heads.is_multiple_of(shape.kv_heads) {
            return Err(ForgeError::Unsupported(format!(
                "{} głowic nie dzieli się na {} głowic KV",
                shape.heads, shape.kv_heads
            )));
        }
        // A rotary that turns an odd number of dimensions, or more of a head
        // than the head has, would address past its own pairing. Checked here
        // because every executor derives it from the shape and none of them
        // would notice.
        if shape.rope_rot == 0
            || !shape.rope_rot.is_multiple_of(2)
            || shape.rope_rot > shape.head_dim
        {
            return Err(ForgeError::Unsupported(format!(
                "obrót po {} wymiarach głowicy o szerokości {}",
                shape.rope_rot, shape.head_dim
            )));
        }

        check_roles(&desc.globals, &desc.layers)?;

        // Mixture parameters, refused rather than approximated where this model
        // has no answer yet.
        let moe = desc.params.moe.as_ref();
        if let Some(m) = moe {
            // The executor sizes its expert scratch from `inter`, so a stack
            // built for another width would be read with the wrong stride. The
            // shared expert may be NARROWER — its own stacks say how wide — but
            // never wider than the scratch it writes into.
            if m.moe_intermediate_size != shape.inter as usize
                || m.shared_intermediate_size > shape.inter as usize
            {
                return Err(ForgeError::Unsupported(format!(
                    "eksperci mają szerokość {} i {}, a kształt mówi {}",
                    m.moe_intermediate_size, m.shared_intermediate_size, shape.inter
                )));
            }
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
        let rope =
            (desc.rope_interleaved() && src.stores_original_rope_order()).then_some(p.head_dim);

        // Cztery szerokości, nie dwie: strumień rezydualny, projekcja Q wraz z
        // wyjściem uwagi, para K/V i FFN. Q i O są PROSTOKĄTNE, gdy głowice nie
        // wypełniają dokładnie `hidden`.
        let (h, qw, kvw, inter) = (
            shape.hidden,
            shape.attn_width(),
            shape.kv_width(),
            shape.inter,
        );
        let src = &*src;
        let embed = quant(&mut exec, src, embd, shape.vocab, h, None)?;
        let final_norm = plain(&mut exec, src, norm_name)?;
        let head = &desc.globals[&WeightRole::LmHead];
        let lm_head = quant(&mut exec, src, head, shape.vocab, h, None)?;

        let mut layers = Vec::with_capacity(desc.layers.len());
        for l in &desc.layers {
            let name = |role: WeightRole| -> &str { l[&role].as_str() };
            let (q, q_gate) = query(
                &mut exec,
                src,
                name(WeightRole::AttnQ),
                qw,
                h,
                rope,
                p.attn_gated.then_some(shape.head_dim),
            )?;
            layers.push(LayerIds {
                attn_norm: plain(&mut exec, src, name(WeightRole::AttnNorm))?,
                q,
                q_gate,
                k: quant(&mut exec, src, name(WeightRole::AttnK), kvw, h, rope)?,
                q_norm: optional(&mut exec, src, l, WeightRole::AttnQNorm)?,
                k_norm: optional(&mut exec, src, l, WeightRole::AttnKNorm)?,
                v: quant(&mut exec, src, name(WeightRole::AttnV), kvw, h, None)?,
                o: quant(&mut exec, src, name(WeightRole::AttnO), h, qw, None)?,
                ffn_norm: plain(&mut exec, src, name(WeightRole::FfnNorm))?,
                ffn: if l.contains_key(&WeightRole::FfnGateInp) {
                    let m = moe.ok_or_else(|| {
                        ForgeError::Unsupported(
                            "warstwa niesie router, a checkpoint nie podał parametrów mieszanki"
                                .into(),
                        )
                    })?;
                    let n = m.n_experts as u32;
                    Ffn::Moe {
                        router: quant(
                            &mut exec,
                            src,
                            name(WeightRole::FfnGateInp),
                            n,
                            h,
                            None,
                        )?,
                        // The stacks arrive three-dimensional and are addressed
                        // as one flat row range per expert, which is how the
                        // kernels index them.
                        gate: quant(
                            &mut exec,
                            src,
                            name(WeightRole::FfnGateExps),
                            n * inter,
                            h,
                            None,
                        )?,
                        up: quant(
                            &mut exec,
                            src,
                            name(WeightRole::FfnUpExps),
                            n * inter,
                            h,
                            None,
                        )?,
                        down: quant(
                            &mut exec,
                            src,
                            name(WeightRole::FfnDownExps),
                            n * h,
                            inter,
                            None,
                        )?,
                        experts: n,
                        top_k: m.n_experts_used as u32,
                        norm_topk: m.norm_topk_prob,
                        shared: shared_expert(&mut exec, src, l, m.shared_intermediate_size, h)?,
                    }
                } else {
                    Ffn::Dense {
                        gate: quant(&mut exec, src, name(WeightRole::FfnGate), inter, h, None)?,
                        up: quant(&mut exec, src, name(WeightRole::FfnUp), inter, h, None)?,
                        down: quant(&mut exec, src, name(WeightRole::FfnDown), h, inter, None)?,
                    }
                },
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
            positions: vec![0; tile.max_lanes as usize],
        })
    }

    pub fn shape(&self) -> DenseShape {
        self.shape
    }

    /// How many tokens the sequence in this slot already holds.
    pub fn position(&self, slot: usize) -> Result<u32> {
        self.positions
            .get(slot)
            .copied()
            .ok_or_else(|| Self::no_such_slot(slot, self.positions.len()))
    }

    /// Tokens this model feeds through the layers in one pass.
    pub fn max_tokens(&self) -> u32 {
        self.tile.max_tokens
    }

    /// Sequences this model can hold at once.
    pub fn max_lanes(&self) -> u32 {
        self.tile.max_lanes
    }

    /// The executor underneath. Its knobs are its own — how a product is split
    /// between the units of one chip is a property of that chip, and a model
    /// that could set it would be a model for that chip.
    pub fn exec_mut(&mut self) -> &mut E {
        &mut self.exec
    }

    /// Current hidden state of the first lane, in f32. Exists for bisecting a
    /// wrong result: with forty layers between the input and the logits, "the
    /// answer is wrong" is not a lead, and reading the state after a chosen
    /// number of layers turns it into one.
    pub fn hidden_state(&self) -> Result<Vec<f32>> {
        self.exec.read(Act::Hidden, self.shape.hidden as usize)
    }

    /// Logity jednego lane'a ostatniego przebiegu.
    ///
    /// Numer lane'a, a nie slotu: głowa wyjściowa pisze wiersz na lane w
    /// KOLEJNOŚCI kroku, więc adresuje ją to samo, co adresowało wiersze
    /// aktywacji.
    pub fn logits(&self, lane: usize) -> Result<Vec<f32>> {
        let vocab = self.shape.vocab as usize;
        let all = self.exec.read(Act::Logits, (lane + 1) * vocab)?;
        Ok(all[lane * vocab..].to_vec())
    }

    /// Runs the embedding and the first `layers` blocks of one slot, then
    /// stops. The token position is NOT advanced: this is a probe, not a step.
    pub fn probe(&mut self, slot: usize, token: u32, layers: usize) -> Result<Vec<f32>> {
        let step = Step::single(slot as u32, self.position(slot)?, 1)?;
        self.exec.run(&Op::Embed {
            table: self.embed,
            tokens: vec![token],
            step: step.clone(),
        })?;
        for index in 0..layers.min(self.layers.len()) {
            for op in fuse(&self.layer_ops(index, &step)) {
                self.exec.run(&op)?;
            }
        }
        self.hidden_state()
    }

    /// Forgets one conversation. The cache is not cleared: every read is
    /// bounded by the current position, so stale bytes past it are unreachable.
    pub fn reset(&mut self, slot: usize) -> Result<()> {
        let len = self.positions.len();
        *self
            .positions
            .get_mut(slot)
            .ok_or_else(|| Self::no_such_slot(slot, len))? = 0;
        Ok(())
    }

    /// Feeds one token to each named slot and returns each one's next token.
    ///
    /// This is the batched step: every sequence contributes one row to the same
    /// projections, so the weights are read ONCE for all of them instead of
    /// once each. That is the whole reason lanes exist — decoding is bandwidth
    /// bound, and its bandwidth is the weights.
    pub fn decode(&mut self, feed: &[Feed]) -> Result<Vec<u32>> {
        let lanes = self.lanes_of(feed, 1)?;
        let step = Step::new(lanes, 1)?;
        let tokens: Vec<u32> = feed.iter().map(|f| f.token).collect();
        self.forward(&tokens, &step)?;
        self.exec.argmax(Act::Logits, feed.len())
    }

    /// Feeds a prompt into one slot and returns the token that follows it.
    ///
    /// This is where a prompt stops costing one full read of the weights per
    /// token. The chunk is bounded by the executor, because how many tokens are
    /// worth carrying at once is a property of its kernels, not of the model.
    pub fn prefill(&mut self, slot: usize, prompt: &[u32]) -> Result<u32> {
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
            let step = Step::single(slot as u32, self.position(slot)?, part.len() as u32)?;
            self.forward(part, &step)?;
            self.exec.sync()?;
        }
        Ok(self.exec.argmax(Act::Logits, 1)?[0])
    }

    /// Feeds a prompt into one slot and continues it greedily.
    pub fn generate(&mut self, slot: usize, prompt: &[u32], max_new: usize) -> Result<Vec<u32>> {
        let mut next = self.prefill(slot, prompt)?;
        let mut out = Vec::with_capacity(max_new);
        out.push(next);
        for _ in 1..max_new {
            next = self.decode(&[Feed { slot, token: next }])?[0];
            out.push(next);
        }
        Ok(out)
    }

    /// Turns a feed into lanes, checking everything that would otherwise show
    /// up as another sequence's context in the answer.
    fn lanes_of(&self, feed: &[Feed], tokens: u32) -> Result<Vec<Lane>> {
        if feed.len() > self.tile.max_lanes as usize {
            return Err(ForgeError::Unsupported(format!(
                "{} sekwencji naraz, a wykonawca trzyma {}",
                feed.len(),
                self.tile.max_lanes
            )));
        }
        feed.iter()
            .map(|f| {
                let pos = self.position(f.slot)?;
                if pos + tokens > self.seq_cap {
                    return Err(ForgeError::Unsupported(format!(
                        "slot {} przekroczył pojemność cache'u ({})",
                        f.slot, self.seq_cap
                    )));
                }
                Ok(Lane {
                    slot: f.slot as u32,
                    pos,
                })
            })
            .collect()
    }

    fn no_such_slot(slot: usize, held: usize) -> ForgeError {
        ForgeError::Unsupported(format!("slot {slot}, a wykonawca trzyma {held}"))
    }

    /// Runs one step through the whole model, leaving each lane's last-token
    /// logits in scratch.
    ///
    /// One code path for a prompt chunk and for a batched decode step. Both the
    /// lane count and the token count reach the kernels as arguments, and at one
    /// lane of one token every kernel does exactly what it did before lanes
    /// existed — which is what makes the agreement between the forms testable
    /// rather than assumed.
    fn forward(&mut self, tokens: &[u32], step: &Step) -> Result<()> {
        if step.tokens() > self.tile.max_tokens {
            return Err(ForgeError::Unsupported(format!(
                "kafel {} tokenów poza zakresem 1..={}",
                step.tokens(),
                self.tile.max_tokens
            )));
        }
        if tokens.len() as u32 != step.rows() {
            return Err(ForgeError::Format(format!(
                "{} tokenów na {} wierszy kroku",
                tokens.len(),
                step.rows()
            )));
        }

        self.exec.run(&Op::Embed {
            table: self.embed,
            tokens: tokens.to_vec(),
            step: step.clone(),
        })?;
        for index in 0..self.layers.len() {
            for op in fuse(&self.layer_ops(index, step)) {
                self.exec.run(&op)?;
            }
        }
        self.exec.run(&Op::RmsNorm {
            out: Act::Norm,
            x: Act::Hidden,
            w: self.final_norm,
            step: step.clone(),
        })?;
        // Logity tylko dla ostatniego tokenu każdego lane'a: pozostałe wiersze
        // służą wyłącznie zapełnieniu cache'u, a głowa wyjściowa jest z 32
        // tysiącami wierszy najdroższą pojedynczą macierzą w modelu.
        self.exec.run(&Op::LogitsOfLast {
            w: self.lm_head,
            x: Act::Norm,
            step: step.clone(),
        })?;
        // Liczniki pozycji przechodzą przez to jedno miejsce. Drugi kafel
        // prefillu musi zacząć tam, gdzie skończył pierwszy, a odejmowanie
        // jedynki „na koniec" jest poprawne wyłącznie dla ostatniego z nich.
        for lane in step.lanes() {
            self.positions[lane.slot as usize] = lane.pos + step.tokens();
        }
        Ok(())
    }

    /// Jedna warstwa jako CIĄG OPERACJI.
    ///
    /// Zwraca dane, a nie wykonuje — dzięki temu ten sam opis da się później
    /// przepisać przed wykonaniem (złączyć, przestawić, dobrać wariant) bez
    /// dotykania modelu, i dzięki temu model nie ma czym nazwać bufora.
    fn layer_ops(&self, index: usize, step: &Step) -> Vec<Op> {
        let s = self.shape;
        let l = &self.layers[index];
        let at = |w: WeightId, out: Act, x: Act| Op::MatMul {
            out,
            w,
            x,
            step: step.clone(),
        };
        // Normy głowic wchodzą MIĘDZY projekcję a RoPE i tylko wtedy, gdy
        // architektura je ma. Kolejność nie jest dowolna: RoPE obraca pary
        // wymiarów głowicy, więc normalizacja po obrocie normalizowałaby liczby,
        // które już niosą pozycję — nadal poprawnie wyglądające, i nadal złe.
        let head_norms = [
            l.q_norm.map(|w| (w, Act::Query, s.heads)),
            l.k_norm.map(|w| (w, Act::Key, s.kv_heads)),
        ];
        let mut ops = vec![
            Op::RmsNorm {
                out: Act::Norm,
                x: Act::Hidden,
                w: l.attn_norm,
                step: step.clone(),
            },
            at(l.q, Act::Query, Act::Norm),
            at(l.k, Act::Key, Act::Norm),
            at(l.v, Act::Value, Act::Norm),
        ];
        // The gate is a projection of the SAME normed input, so it is computed
        // here and applied much later. It deliberately skips the head norm and
        // the rotary below: those carry position into the query, and the gate
        // is not a query.
        ops.extend(l.q_gate.map(|w| at(w, Act::AttnGate, Act::Norm)));
        ops.extend(head_norms.into_iter().flatten().map(|(w, act, heads)| {
            Op::HeadNorm {
                act,
                w,
                heads,
                step: step.clone(),
            }
        }));
        ops.extend([
            Op::Rope {
                act: Act::Query,
                heads: s.heads,
                step: step.clone(),
            },
            Op::Rope {
                act: Act::Key,
                heads: s.kv_heads,
                step: step.clone(),
            },
            Op::KvAppend {
                layer: index,
                step: step.clone(),
            },
            Op::Attention {
                layer: index,
                step: step.clone(),
            },
        ]);
        // AFTER attention and before the output projection. Applied earlier it
        // would scale the query rather than the answer — still fluent, still
        // another model.
        ops.extend(l.q_gate.map(|_| Op::SigmoidMul {
            act: Act::Attn,
            gate: Act::AttnGate,
            step: step.clone(),
        }));
        ops.extend([
            at(l.o, Act::Proj, Act::Attn),
            Op::Residual {
                src: Act::Proj,
                step: step.clone(),
            },
            Op::RmsNorm {
                out: Act::Norm,
                x: Act::Hidden,
                w: l.ffn_norm,
                step: step.clone(),
            },
        ]);
        // Both forms end the same way — a projection into `Proj` and one
        // residual add — so the mixture is a drop-in for the four operations a
        // dense block spends there.
        match l.ffn {
            Ffn::Dense { gate, up, down } => ops.extend([
                at(gate, Act::Gate, Act::Norm),
                at(up, Act::Up, Act::Norm),
                Op::SiluMul { step: step.clone() },
                at(down, Act::Proj, Act::Activated),
            ]),
            Ffn::Moe {
                router,
                gate,
                up,
                down,
                experts,
                top_k,
                norm_topk,
                shared,
            } => ops.push(Op::MoeFfn {
                out: Act::Proj,
                x: Act::Norm,
                router,
                gate,
                up,
                down,
                experts,
                top_k,
                norm_topk,
                shared,
                step: step.clone(),
            }),
        }
        ops.push(Op::Residual {
            src: Act::Proj,
            step: step.clone(),
        });
        ops
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
    let w = fetch_quant(src, name, rows, cols, rope)?;
    exec.put_quant(w)
        .map_err(|e| ForgeError::Format(format!("{name}: {e}")))
}

/// The query projection, and its output gate when the checkpoint stores one.
///
/// A gated architecture keeps both in ONE tensor of twice the width,
/// interleaved per head — so this is where the file's single tensor becomes the
/// model's two projections. `head_dim` arrives as `Some` exactly when the
/// architecture gates, rather than as a flag plus a width, because those two
/// can disagree and this cannot.
fn query<E: WeightStore>(
    exec: &mut E,
    src: &dyn TensorSource,
    name: &str,
    rows: u32,
    cols: u32,
    rope: Option<usize>,
    head_dim: Option<u32>,
) -> Result<(WeightId, Option<WeightId>)> {
    let Some(head_dim) = head_dim else {
        return Ok((quant(exec, src, name, rows, cols, rope)?, None));
    };
    // The split works on the source's bytes, so a source that hands back an
    // already-rewritten form has nothing here to split. No such checkpoint
    // exists yet; the point is that it stops rather than being reinterpreted.
    if src.fetch_affine(name)?.is_some() || src.fetch_nvfp4(name)?.is_some() {
        return Err(ForgeError::Unsupported(format!(
            "{name}: bramkowana projekcja Q wymaga bajtów źródła, a to źródło oddaje postać przepisaną"
        )));
    }
    if rope.is_some() {
        return Err(ForgeError::Unsupported(format!(
            "{name}: permutacja wierszy RoPE i rozdzielenie bramki naraz"
        )));
    }
    let (data, dtype, quant_kind, dims) = src.fetch(name)?;
    let stored = 2 * rows;
    if dims.last() != Some(&(cols as usize)) || dims.first() != Some(&(stored as usize)) {
        return Err(ForgeError::Format(format!(
            "{name}: kształt {dims:?}, oczekiwano [{stored}, {cols}] dla bramkowanego Q"
        )));
    }
    let (q, gate) = split_head_interleaved(&data, stored as usize, head_dim as usize)?;
    let mut hand = |codes: Vec<u8>| -> Result<WeightId> {
        exec.put_quant(QuantWeight::Packed(PackedWeight {
            planes: Planes {
                codes,
                ..Planes::default()
            },
            quant: quant_kind,
            layout: Layout::Blocks,
            dtype,
            rows: rows as usize,
            cols: cols as usize,
        }))
        .map_err(|e| ForgeError::Format(format!("{name}: {e}")))
    };
    Ok((hand(q)?, Some(hand(gate)?)))
}

/// The same, for a weight that is not quantized — the norms.
fn plain<E: WeightStore>(exec: &mut E, src: &dyn TensorSource, name: &str) -> Result<WeightId> {
    exec.put_plain(src.fetch(name)?.0)
}

/// The always-on expert of one mixture layer, when the architecture has one.
///
/// All four weights or none: a layer carrying three of them is refused rather
/// than computed as a mixture without a shared expert. That refusal covers the
/// dangerous case too — a shared expert whose per-token gate is missing is not
/// a shared expert with weight 1.0, and the two would otherwise be one `None`.
fn shared_expert<E: WeightStore>(
    exec: &mut E,
    src: &dyn TensorSource,
    layer: &HashMap<WeightRole, String>,
    width: usize,
    hidden: u32,
) -> Result<Option<Shared>> {
    const ROLES: [WeightRole; 4] = [
        WeightRole::FfnGateShExp,
        WeightRole::FfnUpShExp,
        WeightRole::FfnDownShExp,
        WeightRole::FfnGateInpShExp,
    ];
    let held = ROLES.iter().filter(|r| layer.contains_key(r)).count();
    if held == 0 {
        return Ok(None);
    }
    if held != ROLES.len() || width == 0 {
        return Err(ForgeError::Unsupported(format!(
            "ekspert współdzielony ma {held} z {} wag przy szerokości {width}",
            ROLES.len()
        )));
    }
    let name = |role: WeightRole| -> &str { layer[&role].as_str() };
    let w = width as u32;
    Ok(Some(Shared {
        gate: quant(exec, src, name(WeightRole::FfnGateShExp), w, hidden, None)?,
        up: quant(exec, src, name(WeightRole::FfnUpShExp), w, hidden, None)?,
        down: quant(exec, src, name(WeightRole::FfnDownShExp), hidden, w, None)?,
        // One row: the gate is a per-token logit, not a per-expert one.
        router: quant(exec, src, name(WeightRole::FfnGateInpShExp), 1, hidden, None)?,
    }))
}

/// A weight only some architectures carry.
///
/// Absence is an answer here, not a failure: llama has no per-head norm and
/// Qwen3 does. The role is looked up in the layer's own map, so a checkpoint
/// that declares it gets it loaded and one that does not gets `None` — and
/// `check_roles` has already refused anything in between.
fn optional<E: WeightStore>(
    exec: &mut E,
    src: &dyn TensorSource,
    layer: &HashMap<WeightRole, String>,
    role: WeightRole,
) -> Result<Option<WeightId>> {
    layer
        .get(&role)
        .map(|name| plain(exec, src, name))
        .transpose()
}

/// One weight, in whatever form its source keeps it.
///
/// The model does NOT rewrite it. Which layout the multiply wants is a property
/// of the kernels, so the rewrite belongs to the executor: one backend wants
/// three separate arrays, another wants the source's blocks untouched, and a
/// model choosing for both would be choosing against one of them.
///
/// `rope` carries the head width when the rows still need permuting, and
/// nothing when they do not — an `Option` rather than a `bool` plus a width,
/// because those two can disagree and this cannot.
fn fetch_quant(
    src: &dyn TensorSource,
    name: &str,
    rows: u32,
    cols: u32,
    rope: Option<usize>,
) -> Result<QuantWeight> {
    let w = match src.fetch_affine(name)? {
        // Źródło, które trzyma postać afiniczną natywnie, przestawia wiersze
        // już przy konwersji — `stores_original_rope_order` jest tam fałszem, a
        // to ono włącza `rope`. Gdyby kiedyś powstało źródło z obiema tymi
        // właściwościami, ma się zatrzymać TUTAJ, a nie po cichu pominąć
        // permutację: brak permutacji daje płynny, całkowicie inny tekst.
        Some(t) => {
            if rope.is_some() {
                return Err(ForgeError::Unsupported(format!(
                    "{name}: postać afiniczna źródła wymaga permutacji wierszy RoPE, \
                     a ta działa na bajtach źródła"
                )));
            }
            QuantWeight::Affine(t)
        }
        // NVFP4 z compressed-tensors przychodzi w trzech tensorach. Sprowadzamy
        // je przy WCZYTANIU do jednobuforowych bloków GGUF, bo to ta sama
        // kwantyzacja i te same liczby co do bitu — a wtedy jest zwykłym
        // wierszem tabeli wykonawcy zamiast własną ścieżką. Instrukcja FP4
        // Blackwella jest jedna dla obu układów, więc konwersja niczego nie
        // zamyka; `Layout` zostaje w typie, żeby dało się kiedyś NIE konwertować.
        None if src.fetch_nvfp4(name)?.is_some() => {
            let nv = src.fetch_nvfp4(name)?.expect("sprawdzone wyżej");
            if rope.is_some() {
                return Err(ForgeError::Unsupported(format!(
                    "{name}: NVFP4 wymagałby permutacji wierszy RoPE przed przepakowaniem"
                )));
            }
            QuantWeight::Packed(PackedWeight {
                planes: Planes {
                    codes: nvfp4_ct_to_gguf_blocks(&nv.packed, &nv.scales, nv.rows, nv.cols)?,
                    scales: None,
                    // Kernele mnożą przez ODWROTNOŚĆ: `weight_global_scale` to
                    // dzielnik użyty przy kwantyzacji. Pomylenie strony daje
                    // rozjazd o kwadrat skalara — i logity NaN.
                    global: Some(1.0 / nv.global_scale),
                },
                quant: QuantKind::NVFP4Gguf,
                layout: Layout::Blocks,
                dtype: DType::U8,
                rows: nv.rows,
                cols: nv.cols,
            })
        }
        None => {
            let (mut data, dtype, quant, dims) = src.fetch(name)?;
            // A stack of experts arrives with a dimension per expert, and the
            // leading dimensions FOLD into the row count rather than needing a
            // reshape: the source is row-major, so an expert's rows are already
            // contiguous and `[experts, inter, hidden]` and
            // `[experts * inter, hidden]` are the same bytes in the same order.
            //
            // The row WIDTH is the part that may not fold, so it is checked on
            // its own: a tensor whose last dimension is not `cols` would be
            // read with the wrong stride at every row.
            let folds = dims.last() == Some(&(cols as usize))
                && dims.iter().take(dims.len() - 1).product::<usize>() == rows as usize;
            if !folds {
                return Err(ForgeError::Format(format!(
                    "{name}: kształt {dims:?}, oczekiwano [{rows}, {cols}]"
                )));
            }
            if let Some(head_dim) = rope {
                permute_rope_rows(&mut data, rows as usize, head_dim)?;
            }
            QuantWeight::Packed(PackedWeight {
                planes: Planes {
                    codes: data,
                    ..Planes::default()
                },
                quant,
                layout: Layout::Blocks,
                dtype,
                rows: rows as usize,
                cols: cols as usize,
            })
        }
    };
    let (r, c) = w.shape();
    if r != rows as usize || c != cols as usize {
        return Err(ForgeError::Format(format!(
            "{name}: kształt [{r}, {c}], oczekiwano [{rows}, {cols}]"
        )));
    }
    Ok(w)
}

/// What this model COMPUTES, held against what the checkpoint DECLARES —
/// before a single weight goes to the device.
///
/// Both directions matter and only one of them is obvious.
///
/// A missing role is the ordinary answer "this architecture has a different
/// FFN", and it used to be a bare `no entry found for key` from indexing the
/// role map — a panic that names neither the role nor the layer.
///
/// A role this model does NOT read is the dangerous one, because it used to be
/// nothing at all. Qwen3 requires a per-head normalization of Q and K; `Dense`
/// never asks for it, so such a checkpoint would load and compute WITHOUT it.
/// The result is fluent, wrong text — the same class as the RoPE permutation,
/// and the same reason it is checked rather than remembered.
///
/// Both gaps are reported together, because a loader that reveals one missing
/// role per run turns a straightforward "this needs work" into a queue.
fn check_roles(
    globals: &HashMap<WeightRole, String>,
    layers: &[HashMap<WeightRole, String>],
) -> Result<()> {
    for (index, layer) in layers.iter().enumerate() {
        // A layer's feed-forward block is one shape or the other, and which one
        // is answered by the router: only a mixture has one. Asking that first
        // means a dense checkpoint is never told it is missing expert stacks.
        let ffn = if layer.contains_key(&WeightRole::FfnGateInp) {
            MOE_FFN_ROLES
        } else {
            DENSE_FFN_ROLES
        };
        let wanted = || required_roles().iter().chain(ffn);
        let missing: Vec<_> = wanted()
            .filter(|role| !globals.contains_key(*role) && !layer.contains_key(*role))
            .collect();
        let ignored: Vec<_> = layer
            .keys()
            .filter(|role| !wanted().any(|w| w == *role) && !optional_roles().contains(role))
            .collect();
        if missing.is_empty() && ignored.is_empty() {
            continue;
        }
        let mut why = format!("warstwa {index}: architektura gęsta nie liczy tego checkpointu");
        if !missing.is_empty() {
            why.push_str(&format!("; brakuje ról {missing:?}"));
        }
        if !ignored.is_empty() {
            why.push_str(&format!(
                "; niesie role {ignored:?}, których ten model nie czyta — policzenie go \
                 bez nich dałoby inny model, a nie błąd"
            ));
        }
        return Err(ForgeError::Unsupported(why));
    }
    Ok(())
}

/// The feed-forward roles of a dense block.
const DENSE_FFN_ROLES: &[WeightRole] = &[
    WeightRole::FfnGate,
    WeightRole::FfnUp,
    WeightRole::FfnDown,
];

/// The feed-forward roles of a mixture block. The router is what tells the two
/// apart, so it is the role looked at first.
const MOE_FFN_ROLES: &[WeightRole] = &[
    WeightRole::FfnGateInp,
    WeightRole::FfnGateExps,
    WeightRole::FfnUpExps,
    WeightRole::FfnDownExps,
];

/// Weights this model computes with WHEN a checkpoint carries them.
///
/// Separate from `required_roles` because the distinction is real: a missing
/// required role means this model cannot compute the file, while a missing
/// optional one means the architecture does not have it. Folding the two lists
/// together would make every dense checkpoint look broken for lacking a norm
/// that llama never had.
pub fn optional_roles() -> &'static [WeightRole] {
    &[
        WeightRole::AttnQNorm,
        WeightRole::AttnKNorm,
        WeightRole::FfnGateShExp,
        WeightRole::FfnUpShExp,
        WeightRole::FfnDownShExp,
        WeightRole::FfnGateInpShExp,
    ]
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
            rope_rot: 128,
        };
        assert_eq!(s.kv_width(), 1024);
        assert!((s.attn_scale() - 0.088_388).abs() < 1e-5);
        // Bielik wypełnia głowicami całą szerokość ukrytą, więc obie szerokości
        // są tu równe — i właśnie dlatego trzeba je liczyć osobno. Póki ta
        // równość była asercją, żaden checkpoint bez niej nie mógł wejść.
        assert_eq!(s.attn_width(), 4096);
        assert_eq!(s.attn_width(), s.hidden);

        let wide = DenseShape {
            hidden: 2048,
            heads: 32,
            head_dim: 128,
            ..s
        };
        assert_eq!(wide.attn_width(), 4096, "Q jest szersze niż strumień");
    }

    /// A checkpoint this model cannot compute must be REFUSED, in both of the
    /// two ways it can be uncomputable, and for both shapes of feed-forward
    /// block.
    ///
    /// The silent direction is the one worth the test: a role the loop never
    /// reads costs nothing at load and produces a model that runs. Qwen3 needs
    /// its per-head Q/K norm, and without this check it would be skipped.
    #[test]
    fn a_checkpoint_this_model_cannot_compute_is_refused() {
        let layer = |roles: &[WeightRole]| -> HashMap<WeightRole, String> {
            roles
                .iter()
                .map(|r| (*r, format!("{r:?}")))
                .collect::<HashMap<_, _>>()
        };
        let attn: Vec<WeightRole> = required_roles()
            .iter()
            .copied()
            .filter(|r| {
                !matches!(
                    r,
                    WeightRole::TokenEmbd | WeightRole::OutputNorm | WeightRole::LmHead
                )
            })
            .collect();
        let with = |extra: &[WeightRole]| -> Vec<WeightRole> {
            attn.iter().copied().chain(extra.iter().copied()).collect()
        };
        let globals = layer(&[
            WeightRole::TokenEmbd,
            WeightRole::OutputNorm,
            WeightRole::LmHead,
        ]);
        let check = |roles: &[WeightRole]| check_roles(&globals, &[layer(roles)]);

        // Both shapes of block are computable, and neither is told it is
        // missing the other one's weights.
        check(&with(DENSE_FFN_ROLES)).expect("dense FFN");
        check(&with(MOE_FFN_ROLES)).expect("mixture FFN");

        let mut short = with(DENSE_FFN_ROLES);
        short.retain(|r| *r != WeightRole::FfnGate);
        let err = check(&short).expect_err("brak roli przeszedł");
        assert!(format!("{err}").contains("FfnGate"), "{err}");

        // A mixture missing one stack must not fall back to being read as a
        // dense block — the router is what decides which shape it is.
        let mut lame = with(MOE_FFN_ROLES);
        lame.retain(|r| *r != WeightRole::FfnUpExps);
        let err = check(&lame).expect_err("niepełna mieszanka przeszła");
        assert!(format!("{err}").contains("FfnUpExps"), "{err}");

        // The head norms are optional and READ, so a checkpoint carrying them
        // passes. That is the whole content of milestone 4a-1.
        check(&with(&[
            WeightRole::FfnGate,
            WeightRole::FfnUp,
            WeightRole::FfnDown,
            WeightRole::AttnQNorm,
            WeightRole::AttnKNorm,
        ]))
        .expect("normy głowic mają być liczone, nie odrzucane");

        // A role this model genuinely does not compute still has to bounce.
        let err = check(&with(&[
            WeightRole::FfnGate,
            WeightRole::FfnUp,
            WeightRole::FfnDown,
            WeightRole::SsmInProj,
        ]))
        .expect_err("cicho pominięta rola");
        assert!(format!("{err}").contains("SsmInProj"), "{err}");
    }

    #[test]
    fn every_role_the_loop_reads_is_declared_required() {
        for role in [
            WeightRole::AttnQ,
            WeightRole::AttnK,
            WeightRole::AttnV,
            WeightRole::AttnO,
            WeightRole::LmHead,
        ] {
            assert!(required_roles().contains(&role), "{role:?}");
        }
        for role in DENSE_FFN_ROLES.iter().chain(MOE_FFN_ROLES) {
            assert!(
                !required_roles().contains(role),
                "{role:?} należy do jednego kształtu FFN, nie do wspólnych"
            );
        }
    }
}
