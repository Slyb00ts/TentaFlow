// ===== File: host_exec.rs — the same operations, on the host, as the definition =====
//
// A second implementer of `Executor`, in plain Rust over host memory. It is
// slow on purpose: every op here is written as its DEFINITION, not as the
// blocking, staging and fusion that make a GPU fast.
//
// Two jobs, and both of them are the reason this is not busywork:
//
//   * An interface with one implementer is an interface on paper. Everything
//     the Metal executor happens to assume — a layout, a rounding, a slot that
//     is secretly half precision — either shows up as a compile error here or
//     shows up as a wrong number, and both are better than finding it on the
//     next card.
//   * It is the oracle. `forge-formats` already checks its quantization
//     decoders against a CPU reference; this is the same idea one level up, for
//     the whole forward pass. A backend that cannot be run on the machine doing
//     the work can still be held to something that can.
//
// So the arithmetic is f32 throughout and the softmax is the textbook one. This
// file is allowed to be the slow, obvious version — that is what makes it worth
// comparing against.

use std::cell::RefCell;

use half::{bf16, f16};

use forge_formats::affine::AffineTriple;
use forge_formats::dequant::dequantize_to_f32;
use forge_graph::{Act, ExecSpec, Executor, Op, QuantWeight, Tile, WeightId, WeightStore};
use forge_types::{DType, DenseShape, ForgeError, QuantKind, Result};

/// Tokens carried through the layers in one pass.
///
/// Nothing here stages anything, so this is not a tile geometry — it only
/// bounds how much scratch one call may need.
const MAX_TOKENS: u32 = 512;

/// Context this executor will hold PER SEQUENCE. Not an allocation: the caches
/// grow with the tokens that actually arrive, so the number is a refusal
/// threshold rather than a promise of memory.
const SEQ_CAP: u32 = 32768;

/// Sequences held at once.
///
/// The reference has nothing to size here — its caches grow — so this is only
/// the count it agrees to be asked about. It exists at all because a reference
/// that could not hold a batch could not be the oracle for one.
const MAX_LANES: u32 = 8;

/// A quantized weight, exactly as the source packed it.
///
/// Kept packed rather than expanded, for the same reason the GPU keeps it
/// packed: a dequantized copy of a 7B checkpoint is 28 GB in f32, and the
/// reference is supposed to be runnable, not merely correct.
struct HostQuant {
    packed: Vec<u32>,
    /// Bits four and five, sixteen per word. Empty at four bits.
    high: Vec<u32>,
    bits: u32,
    scales: Vec<f32>,
    biases: Vec<f32>,
    group: usize,
    rows: usize,
    cols: usize,
}

impl HostQuant {
    /// One whole row, dequantized. This IS the affine formula — every other
    /// decoder in this repository is an optimization of these four lines.
    fn row_into(&self, row: usize, out: &mut [f32]) {
        let groups_per_row = self.cols / self.group;
        for (col, o) in out.iter_mut().enumerate().take(self.cols) {
            let idx = row * self.cols + col;
            let low = (self.packed[idx / 8] >> ((idx % 8) * 4)) & 0xF;
            let q = match self.bits {
                6 => low | (((self.high[idx / 16] >> ((idx % 16) * 2)) & 0x3) << 4),
                _ => low,
            };
            let g = row * groups_per_row + col / self.group;
            *o = q as f32 * self.scales[g] + self.biases[g];
        }
    }
}

/// A quantized weight in the source's own blocks.
///
/// The reference decodes them with `forge-formats`, which knows every
/// quantization the checkpoints use — so the oracle covers all of them instead
/// of the three that fit the affine triple. That matters because a format is
/// only migrated to the new path once something can say the GPU got it right.
struct HostBlocks {
    data: Vec<u8>,
    quant: QuantKind,
    rows: usize,
    cols: usize,
}

impl HostBlocks {
    /// Bytes of one row. Every block format here is row-major over whole
    /// blocks, which is the same fact the RoPE row permutation stands on.
    fn row_bytes(&self) -> usize {
        self.cols / self.quant.block_elems() * self.quant.block_bytes()
    }

    fn row_into(&self, row: usize, out: &mut [f32]) -> Result<()> {
        let width = self.row_bytes();
        let decoded = dequantize_to_f32(
            DType::F32,
            self.quant,
            &self.data[row * width..(row + 1) * width],
            self.cols,
        )?;
        out[..self.cols].copy_from_slice(&decoded);
        Ok(())
    }
}

enum HostWeight {
    /// Postać afiniczna, gdy źródło ma tylko ją — tak wygląda eksport MLX.
    Affine(HostQuant),
    Blocks(HostBlocks),
    Plain(Vec<f32>),
}

impl HostWeight {
    fn shape(&self) -> (usize, usize) {
        match self {
            Self::Affine(q) => (q.rows, q.cols),
            Self::Blocks(b) => (b.rows, b.cols),
            Self::Plain(_) => (0, 0),
        }
    }

    fn row_into(&self, row: usize, out: &mut [f32]) -> Result<()> {
        match self {
            Self::Affine(q) => {
                q.row_into(row, out);
                Ok(())
            }
            Self::Blocks(b) => b.row_into(row, out),
            Self::Plain(_) => Err(ForgeError::Other("waga zwykła nie ma wierszy".into())),
        }
    }
}

/// Named activation slots, all f32.
///
/// The Metal executor keeps most of these in half precision because that is
/// what its matrix units read. This one does not imitate that: a reference that
/// reproduces the fast path's rounding cannot tell you whether the rounding was
/// the problem.
struct Slots {
    by: Vec<Vec<f32>>,
}

impl Slots {
    fn new() -> Self {
        Self {
            by: (0..SLOT_COUNT).map(|_| Vec::new()).collect(),
        }
    }
}

const SLOT_COUNT: usize = 11;

fn slot(a: Act) -> usize {
    match a {
        Act::Hidden => 0,
        Act::Norm => 1,
        Act::Query => 2,
        Act::Key => 3,
        Act::Value => 4,
        Act::Attn => 5,
        Act::Proj => 6,
        Act::Gate => 7,
        Act::Up => 8,
        Act::Activated => 9,
        Act::Logits => 10,
    }
}

/// The dense vocabulary, computed on the host.
pub struct HostExec {
    weights: Vec<HostWeight>,
    /// Key and value per (layer, slot), token-major: `[pos][kv_head][dim]`.
    ///
    /// Token-major and GROWN rather than head-major and preallocated, because
    /// the reference has no kernel whose addressing this has to suit — and a
    /// head-major cache would have to reserve the whole context up front, which
    /// at this precision is gigabytes of untouched memory. One run per slot,
    /// for the same reason: nothing here is paged, so a slot IS its own run.
    kv: RefCell<Vec<Vec<(Vec<f32>, Vec<f32>)>>>,
    acts: RefCell<Slots>,
    shape: DenseShape,
    quant_params: DType,
    norm_weights: DType,
}

impl HostExec {
    pub fn new(spec: ExecSpec) -> Result<Self> {
        Ok(Self {
            weights: Vec::new(),
            kv: RefCell::new(
                (0..spec.shape.layers)
                    .map(|_| (0..MAX_LANES).map(|_| (Vec::new(), Vec::new())).collect())
                    .collect(),
            ),
            acts: RefCell::new(Slots::new()),
            shape: spec.shape,
            quant_params: spec.quant_params,
            norm_weights: spec.norm_weights,
        })
    }

    fn quant(&self, id: WeightId) -> Result<&HostWeight> {
        match self.weights.get(id.0 as usize) {
            Some(w @ (HostWeight::Affine(_) | HostWeight::Blocks(_))) => Ok(w),
            _ => Err(ForgeError::Other(format!(
                "waga {} nie jest kwantyzowana",
                id.0
            ))),
        }
    }

    fn no_such_slot(slot: u32) -> ForgeError {
        ForgeError::Other(format!("brak slotu {slot} w cache'u wzorca"))
    }

    fn plain(&self, id: WeightId) -> Result<&[f32]> {
        match self.weights.get(id.0 as usize) {
            Some(HostWeight::Plain(v)) => Ok(v),
            _ => Err(ForgeError::Other(format!("waga {} nie jest zwykła", id.0))),
        }
    }

    /// Makes a slot hold exactly `len` elements, zeroed.
    fn resize(&self, a: Act, len: usize) {
        let mut s = self.acts.borrow_mut();
        let v = &mut s.by[slot(a)];
        v.clear();
        v.resize(len, 0.0);
    }

    fn with<T>(&self, a: Act, f: impl FnOnce(&[f32]) -> T) -> T {
        let s = self.acts.borrow();
        f(&s.by[slot(a)])
    }

    /// `out = x * wagaᵀ`, plain and unblocked.
    fn matmul(&self, out: Act, w: &HostWeight, x: Act, tokens: usize) -> Result<()> {
        let (rows, cols) = w.shape();
        let src = self.with(x, <[f32]>::to_vec);
        if src.len() < tokens * cols {
            return Err(ForgeError::Other(format!(
                "wejście ma {} wartości, a mnożenie chce {}",
                src.len(),
                tokens * cols
            )));
        }
        self.resize(out, tokens * rows);
        let mut dst = self.acts.borrow_mut();
        let dst = &mut dst.by[slot(out)];
        let mut row = vec![0.0f32; cols];
        for r in 0..rows {
            w.row_into(r, &mut row)?;
            for t in 0..tokens {
                let xs = &src[t * cols..(t + 1) * cols];
                let mut acc = 0.0f32;
                for c in 0..cols {
                    acc = row[c].mul_add(xs[c], acc);
                }
                dst[t * rows + r] = acc;
            }
        }
        Ok(())
    }
}

impl WeightStore for HostExec {
    /// The reference indexes the affine triple, so a source that hands over
    /// blocks is rewritten HERE — by the executor that wants that shape, not by
    /// the model, which would then be choosing a layout on every backend's
    /// behalf.
    fn put_quant(&mut self, w: QuantWeight) -> Result<WeightId> {
        match w {
            QuantWeight::Affine(t) => self.put_affine(t),
            QuantWeight::Blocks {
                data,
                quant,
                rows,
                cols,
            } => {
                // Sprawdzane TU, przy wgraniu: wiersz, który nie dzieli się na
                // całe bloki, adresowałby cudzy blok przy każdym odczycie.
                if !cols.is_multiple_of(quant.block_elems()) {
                    return Err(ForgeError::Unsupported(format!(
                        "{cols} kolumn nie dzieli się na bloki {quant:?} po {}",
                        quant.block_elems()
                    )));
                }
                self.weights.push(HostWeight::Blocks(HostBlocks {
                    data,
                    quant,
                    rows,
                    cols,
                }));
                Ok(WeightId(self.weights.len() as u32 - 1))
            }
        }
    }

    fn put_plain(&mut self, bytes: Vec<u8>) -> Result<WeightId> {
        let v = widen(&bytes, self.norm_weights)?;
        self.weights.push(HostWeight::Plain(v));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }
}

impl HostExec {
    fn put_affine(&mut self, t: AffineTriple) -> Result<WeightId> {
        if t.param_dtype != self.quant_params {
            return Err(ForgeError::Format(format!(
                "waga ma parametry {:?}, a wykonawca dostał {:?}",
                t.param_dtype, self.quant_params
            )));
        }
        if !t.cols.is_multiple_of(t.group) {
            return Err(ForgeError::Unsupported(format!(
                "{} kolumn nie dzieli się na grupy po {}",
                t.cols, t.group
            )));
        }
        if !matches!(t.bits, 4 | 6) {
            return Err(ForgeError::Unsupported(format!(
                "{} bitów na wagę, a wzorzec zna cztery i sześć",
                t.bits
            )));
        }
        self.weights.push(HostWeight::Affine(HostQuant {
            packed: t.packed,
            high: t.high,
            bits: t.bits,
            scales: widen(&t.scales, t.param_dtype)?,
            biases: widen(&t.biases, t.param_dtype)?,
            group: t.group,
            rows: t.rows,
            cols: t.cols,
        }));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }
}

impl Executor for HostExec {
    fn run(&self, op: &Op) -> Result<()> {
        let s = self.shape;
        match op {
            Op::Embed {
                table,
                tokens,
                step,
            } => {
                let w = self.quant(*table)?;
                let (vocab, _) = w.shape();
                self.resize(Act::Hidden, tokens.len() * s.hidden as usize);
                if tokens.len() as u32 != step.rows() {
                    return Err(ForgeError::Format(format!(
                        "{} osadzeń na {} wierszy kroku",
                        tokens.len(),
                        step.rows()
                    )));
                }
                let mut acts = self.acts.borrow_mut();
                let h = &mut acts.by[slot(Act::Hidden)];
                for (t, &id) in tokens.iter().enumerate() {
                    if id as usize >= vocab {
                        return Err(ForgeError::Format(format!(
                            "token {id} poza słownikiem {vocab}"
                        )));
                    }
                    let base = t * s.hidden as usize;
                    w.row_into(id as usize, &mut h[base..base + s.hidden as usize])?;
                }
                Ok(())
            }

            Op::RmsNorm { out, x, w, step } => {
                let weight = self.plain(*w)?.to_vec();
                let src = self.with(*x, <[f32]>::to_vec);
                let n = s.hidden as usize;
                let rows = step.rows() as usize;
                self.resize(*out, rows * n);
                let mut acts = self.acts.borrow_mut();
                let dst = &mut acts.by[slot(*out)];
                for t in 0..rows {
                    let row = &src[t * n..(t + 1) * n];
                    let mean = row.iter().map(|v| v * v).sum::<f32>() / n as f32;
                    let scale = 1.0 / (mean + s.eps).sqrt();
                    for c in 0..n {
                        dst[t * n + c] = row[c] * scale * weight[c];
                    }
                }
                Ok(())
            }

            Op::MatMul { out, w, x, step } => {
                self.matmul(*out, self.quant(*w)?, *x, step.rows() as usize)
            }

            Op::LogitsOfLast { w, x, step } => {
                // Ostatni token KAŻDEGO lane'a, każdy w swoim wierszu wyniku.
                let w = self.quant(*w)?;
                let (w_rows, w_cols) = w.shape();
                let tokens = step.tokens() as usize;
                let last = |lane: usize| (lane * tokens + tokens - 1) * w_cols;
                let src = self.with(*x, |v| {
                    (0..step.lanes().len())
                        .flat_map(|lane| v[last(lane)..last(lane) + w_cols].to_vec())
                        .collect::<Vec<f32>>()
                });
                self.resize(Act::Logits, step.lanes().len() * w_rows);
                let mut acts = self.acts.borrow_mut();
                let dst = &mut acts.by[slot(Act::Logits)];
                let mut row = vec![0.0f32; w_cols];
                for r in 0..w_rows {
                    w.row_into(r, &mut row)?;
                    for lane in 0..step.lanes().len() {
                        let xs = &src[lane * w_cols..(lane + 1) * w_cols];
                        dst[lane * w_rows + r] = row
                            .iter()
                            .zip(xs)
                            .fold(0.0f32, |a, (x, y)| x.mul_add(*y, a));
                    }
                }
                Ok(())
            }

            Op::Rope { act, heads, step } => {
                let dims = s.head_dim as usize;
                let half = dims / 2;
                let tokens = step.tokens() as usize;
                let mut acts = self.acts.borrow_mut();
                let v = &mut acts.by[slot(*act)];
                for (lane, l) in step.lanes().iter().enumerate() {
                    for t in 0..tokens {
                        for h in 0..*heads as usize {
                            let base = ((lane * tokens + t) * *heads as usize + h) * dims;
                            for i in 0..half {
                                // Częstotliwość w f32 i podstawa z kształtu: przy
                                // base 1e6 i dims 128 wykładnik schodzi do 1e-6.
                                let freq = (l.pos as usize + t) as f32
                                    * s.rope_theta.powf(-2.0 * i as f32 / dims as f32);
                                let (sin, cos) = freq.sin_cos();
                                let x0 = v[base + i];
                                let x1 = v[base + i + half];
                                v[base + i] = x0 * cos - x1 * sin;
                                v[base + i + half] = x0 * sin + x1 * cos;
                            }
                        }
                    }
                }
                Ok(())
            }

            Op::KvAppend { layer, step } => {
                let width = s.kv_width() as usize;
                let tokens = step.tokens() as usize;
                let (k, v) = (
                    self.with(Act::Key, <[f32]>::to_vec),
                    self.with(Act::Value, <[f32]>::to_vec),
                );
                let mut kv = self.kv.borrow_mut();
                let slots = kv
                    .get_mut(*layer)
                    .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
                for (lane, l) in step.lanes().iter().enumerate() {
                    let (kc, vc) = slots
                        .get_mut(l.slot as usize)
                        .ok_or_else(|| Self::no_such_slot(l.slot))?;
                    // Cache rośnie na końcu, ale krok może zaczynać się TAM,
                    // GDZIE JUŻ COŚ JEST — po cofnięciu pozycji przez `reset`.
                    // Nadpisanie jest wtedy poprawne, doklejenie nie.
                    let (from, end) = (l.pos as usize * width, (l.pos as usize + tokens) * width);
                    if kc.len() < end {
                        kc.resize(end, 0.0);
                        vc.resize(end, 0.0);
                    }
                    let src = lane * tokens * width;
                    kc[from..end].copy_from_slice(&k[src..src + tokens * width]);
                    vc[from..end].copy_from_slice(&v[src..src + tokens * width]);
                }
                Ok(())
            }

            Op::Attention { layer, step } => {
                let (heads, kvh) = (s.heads as usize, s.kv_heads as usize);
                let dims = s.head_dim as usize;
                let width = kvh * dims;
                let tokens = step.tokens() as usize;
                let q = self.with(Act::Query, <[f32]>::to_vec);
                let kv = self.kv.borrow();
                let slots = kv
                    .get(*layer)
                    .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
                let per_kv = heads / kvh;
                self.resize(Act::Attn, step.rows() as usize * heads * dims);
                let mut acts = self.acts.borrow_mut();
                let out = &mut acts.by[slot(Act::Attn)];
                let scale = s.attn_scale();
                for (lane, l) in step.lanes().iter().enumerate() {
                    let (kc, vc) = slots
                        .get(l.slot as usize)
                        .ok_or_else(|| Self::no_such_slot(l.slot))?;
                    for t in 0..tokens {
                        // Przyczynowość bez maski: zapytanie siedzi na pozycji
                        // `pos + t`, więc pętla kończy się na niej.
                        let len = l.pos as usize + t + 1;
                        for h in 0..heads {
                            let kv_head = h / per_kv;
                            let base = ((lane * tokens + t) * heads + h) * dims;
                            let qh = &q[base..][..dims];
                            let mut scores = vec![0.0f32; len];
                            for (j, sc) in scores.iter_mut().enumerate() {
                                let kj = &kc[j * width + kv_head * dims..][..dims];
                                *sc = qh.iter().zip(kj).fold(0.0f32, |a, (x, y)| x.mul_add(*y, a))
                                    * scale;
                            }
                            let m = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                            let mut total = 0.0f32;
                            for sc in scores.iter_mut() {
                                *sc = (*sc - m).exp();
                                total += *sc;
                            }
                            for (j, sc) in scores.iter().enumerate() {
                                let vj = &vc[j * width + kv_head * dims..][..dims];
                                let p = sc / total;
                                for c in 0..dims {
                                    out[base + c] = p.mul_add(vj[c], out[base + c]);
                                }
                            }
                        }
                    }
                }
                Ok(())
            }

            Op::SiluMul { step } => {
                let n = step.rows() as usize * s.inter as usize;
                let (gate, up) = (
                    self.with(Act::Gate, <[f32]>::to_vec),
                    self.with(Act::Up, <[f32]>::to_vec),
                );
                self.resize(Act::Activated, n);
                let mut acts = self.acts.borrow_mut();
                let dst = &mut acts.by[slot(Act::Activated)];
                for i in 0..n {
                    let g = gate[i];
                    dst[i] = g / (1.0 + (-g).exp()) * up[i];
                }
                Ok(())
            }

            Op::Residual { src, step } => {
                let delta = self.with(*src, <[f32]>::to_vec);
                let n = step.rows() as usize * s.hidden as usize;
                let mut acts = self.acts.borrow_mut();
                let h = &mut acts.by[slot(Act::Hidden)];
                for i in 0..n {
                    h[i] += delta[i];
                }
                Ok(())
            }
        }
    }

    /// Nothing is queued, so there is nothing to wait for.
    fn sync(&self) -> Result<()> {
        Ok(())
    }

    fn read(&self, act: Act, len: usize) -> Result<Vec<f32>> {
        self.with(act, |v| {
            if v.len() < len {
                return Err(ForgeError::Other(format!(
                    "slot ma {} wartości, a odczyt chce {len}",
                    v.len()
                )));
            }
            Ok(v[..len].to_vec())
        })
    }

    fn argmax(&self, act: Act, lanes: usize) -> Result<Vec<u32>> {
        let vocab = self.shape.vocab as usize;
        self.with(act, |v| {
            if v.len() < lanes * vocab {
                return Err(ForgeError::Other(format!(
                    "slot ma {} wartości, a wybór dla {lanes} lane'ów chce {}",
                    v.len(),
                    lanes * vocab
                )));
            }
            Ok((0..lanes)
                .map(|lane| {
                    let row = &v[lane * vocab..(lane + 1) * vocab];
                    let mut best = 0usize;
                    for (i, &x) in row.iter().enumerate() {
                        if x > row[best] {
                            best = i;
                        }
                    }
                    best as u32
                })
                .collect())
        })
    }

    fn seq_cap(&self) -> u32 {
        SEQ_CAP
    }

    fn tile(&self) -> Tile {
        Tile {
            max_tokens: MAX_TOKENS,
            max_lanes: MAX_LANES,
            align: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every slot has a place, and no two share one.
    ///
    /// The slot count is written down separately from the mapping, so a new
    /// activation could be given an index past the end of the table. That is an
    /// out-of-bounds panic on the first use, in the middle of a forward pass;
    /// here it is a failing test with the name of the thing that moved.
    #[test]
    fn every_activation_has_its_own_slot() {
        let all = [
            Act::Hidden,
            Act::Norm,
            Act::Query,
            Act::Key,
            Act::Value,
            Act::Attn,
            Act::Proj,
            Act::Gate,
            Act::Up,
            Act::Activated,
            Act::Logits,
        ];
        assert_eq!(
            all.len(),
            SLOT_COUNT,
            "liczba slotów rozjechała się z listą"
        );
        let mut seen = vec![false; SLOT_COUNT];
        for a in all {
            let i = slot(a);
            assert!(i < SLOT_COUNT, "{a:?} ma indeks {i} poza tablicą");
            assert!(!seen[i], "{a:?} dzieli slot {i} z inną aktywacją");
            seen[i] = true;
        }
    }
}

/// Widens raw parameter bytes to f32.
fn widen(bytes: &[u8], dtype: DType) -> Result<Vec<f32>> {
    match dtype {
        DType::F16 => Ok(bytes
            .chunks_exact(2)
            .map(|c| f16::from_le_bytes([c[0], c[1]]).to_f32())
            .collect()),
        DType::BF16 => Ok(bytes
            .chunks_exact(2)
            .map(|c| bf16::from_le_bytes([c[0], c[1]]).to_f32())
            .collect()),
        DType::F32 => Ok(bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect()),
        other => Err(ForgeError::Unsupported(format!(
            "wzorzec nie zna typu {other:?}"
        ))),
    }
}
