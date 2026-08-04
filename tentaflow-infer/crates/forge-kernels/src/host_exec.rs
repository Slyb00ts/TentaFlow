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
use forge_graph::{Act, ExecSpec, Executor, Op, Tile, WeightId, WeightStore};
use forge_types::{DType, DenseShape, ForgeError, Result};

/// Tokens carried through the layers in one pass.
///
/// Nothing here stages anything, so this is not a tile geometry — it only
/// bounds how much scratch one call may need.
const MAX_TOKENS: u32 = 512;

/// Context this executor will hold. Not an allocation: the caches grow with the
/// tokens that actually arrive, so the number is a refusal threshold rather
/// than a promise of memory.
const SEQ_CAP: u32 = 32768;

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

enum HostWeight {
    Quant(HostQuant),
    Plain(Vec<f32>),
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
    /// Key and value per layer, token-major: `[pos][kv_head][dim]`.
    ///
    /// Token-major and GROWN rather than head-major and preallocated, because
    /// the reference has no kernel whose addressing this has to suit — and a
    /// head-major cache would have to reserve the whole context up front, which
    /// at this precision is gigabytes of untouched memory.
    kv: RefCell<Vec<(Vec<f32>, Vec<f32>)>>,
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
                    .map(|_| (Vec::new(), Vec::new()))
                    .collect(),
            ),
            acts: RefCell::new(Slots::new()),
            shape: spec.shape,
            quant_params: spec.quant_params,
            norm_weights: spec.norm_weights,
        })
    }

    fn quant(&self, id: WeightId) -> Result<&HostQuant> {
        match self.weights.get(id.0 as usize) {
            Some(HostWeight::Quant(q)) => Ok(q),
            _ => Err(ForgeError::Other(format!(
                "waga {} nie jest kwantyzowana",
                id.0
            ))),
        }
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
    fn matmul(&self, out: Act, w: &HostQuant, x: Act, tokens: usize) -> Result<()> {
        let src = self.with(x, <[f32]>::to_vec);
        if src.len() < tokens * w.cols {
            return Err(ForgeError::Other(format!(
                "wejście ma {} wartości, a mnożenie chce {}",
                src.len(),
                tokens * w.cols
            )));
        }
        self.resize(out, tokens * w.rows);
        let mut dst = self.acts.borrow_mut();
        let dst = &mut dst.by[slot(out)];
        let mut row = vec![0.0f32; w.cols];
        for r in 0..w.rows {
            w.row_into(r, &mut row);
            for t in 0..tokens {
                let xs = &src[t * w.cols..(t + 1) * w.cols];
                let mut acc = 0.0f32;
                for c in 0..w.cols {
                    acc = row[c].mul_add(xs[c], acc);
                }
                dst[t * w.rows + r] = acc;
            }
        }
        Ok(())
    }
}

impl WeightStore for HostExec {
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
        self.weights.push(HostWeight::Quant(HostQuant {
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

    fn put_plain(&mut self, bytes: Vec<u8>) -> Result<WeightId> {
        let v = widen(&bytes, self.norm_weights)?;
        self.weights.push(HostWeight::Plain(v));
        Ok(WeightId(self.weights.len() as u32 - 1))
    }
}

impl Executor for HostExec {
    fn run(&self, op: &Op) -> Result<()> {
        let s = self.shape;
        match op {
            Op::Embed { table, tokens } => {
                let w = self.quant(*table)?;
                self.resize(Act::Hidden, tokens.len() * s.hidden as usize);
                let mut acts = self.acts.borrow_mut();
                let h = &mut acts.by[slot(Act::Hidden)];
                for (t, &id) in tokens.iter().enumerate() {
                    if id as usize >= w.rows {
                        return Err(ForgeError::Format(format!(
                            "token {id} poza słownikiem {}",
                            w.rows
                        )));
                    }
                    let base = t * s.hidden as usize;
                    w.row_into(id as usize, &mut h[base..base + s.hidden as usize]);
                }
                Ok(())
            }

            Op::RmsNorm { out, x, w, tokens } => {
                let weight = self.plain(*w)?.to_vec();
                let src = self.with(*x, <[f32]>::to_vec);
                let n = s.hidden as usize;
                self.resize(*out, *tokens as usize * n);
                let mut acts = self.acts.borrow_mut();
                let dst = &mut acts.by[slot(*out)];
                for t in 0..*tokens as usize {
                    let row = &src[t * n..(t + 1) * n];
                    let mean = row.iter().map(|v| v * v).sum::<f32>() / n as f32;
                    let scale = 1.0 / (mean + s.eps).sqrt();
                    for c in 0..n {
                        dst[t * n + c] = row[c] * scale * weight[c];
                    }
                }
                Ok(())
            }

            Op::MatMul { out, w, x, tokens } => {
                self.matmul(*out, self.quant(*w)?, *x, *tokens as usize)
            }

            Op::LogitsOfLast { w, x, tokens } => {
                // Tylko ostatni token kafla, więc jego wiersz jest przepisywany
                // na początek i mnożony jako pojedynczy.
                let w = self.quant(*w)?;
                let last = (*tokens as usize - 1) * w.cols;
                let src = self.with(*x, |v| v[last..last + w.cols].to_vec());
                self.resize(Act::Logits, w.rows);
                let mut acts = self.acts.borrow_mut();
                let dst = &mut acts.by[slot(Act::Logits)];
                let mut row = vec![0.0f32; w.cols];
                for (r, out) in dst.iter_mut().enumerate() {
                    w.row_into(r, &mut row);
                    *out = row.iter().zip(&src).fold(0.0f32, |a, (x, y)| x.mul_add(*y, a));
                }
                Ok(())
            }

            Op::Rope {
                act,
                heads,
                pos,
                tokens,
            } => {
                let dims = s.head_dim as usize;
                let half = dims / 2;
                let mut acts = self.acts.borrow_mut();
                let v = &mut acts.by[slot(*act)];
                for t in 0..*tokens as usize {
                    for h in 0..*heads as usize {
                        let base = (t * *heads as usize + h) * dims;
                        for i in 0..half {
                            // Częstotliwość w f32 i podstawa z kształtu: przy
                            // base 1e6 i dims 128 wykładnik schodzi do 1e-6.
                            let freq = (*pos as usize + t) as f32
                                * s.rope_theta.powf(-2.0 * i as f32 / dims as f32);
                            let (sin, cos) = freq.sin_cos();
                            let x0 = v[base + i];
                            let x1 = v[base + i + half];
                            v[base + i] = x0 * cos - x1 * sin;
                            v[base + i + half] = x0 * sin + x1 * cos;
                        }
                    }
                }
                Ok(())
            }

            Op::KvAppend { layer, pos, tokens } => {
                let width = s.kv_width() as usize;
                let (k, v) = (
                    self.with(Act::Key, <[f32]>::to_vec),
                    self.with(Act::Value, <[f32]>::to_vec),
                );
                let mut kv = self.kv.borrow_mut();
                let (kc, vc) = kv
                    .get_mut(*layer)
                    .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
                // Cache rośnie na końcu, ale kafel może zaczynać się TAM, GDZIE
                // JUŻ COŚ JEST — po cofnięciu pozycji przez `reset`. Nadpisanie
                // jest wtedy poprawne, doklejenie nie.
                let (from, end) = (
                    *pos as usize * width,
                    (*pos as usize + *tokens as usize) * width,
                );
                if kc.len() < end {
                    kc.resize(end, 0.0);
                    vc.resize(end, 0.0);
                }
                kc[from..end].copy_from_slice(&k[..*tokens as usize * width]);
                vc[from..end].copy_from_slice(&v[..*tokens as usize * width]);
                Ok(())
            }

            Op::Attention { layer, seq, tokens } => {
                let (heads, kvh) = (s.heads as usize, s.kv_heads as usize);
                let dims = s.head_dim as usize;
                let width = kvh * dims;
                let q = self.with(Act::Query, <[f32]>::to_vec);
                let kv = self.kv.borrow();
                let (kc, vc) = kv
                    .get(*layer)
                    .ok_or_else(|| ForgeError::Other(format!("brak cache'u warstwy {layer}")))?;
                let per_kv = heads / kvh;
                self.resize(Act::Attn, *tokens as usize * heads * dims);
                let mut acts = self.acts.borrow_mut();
                let out = &mut acts.by[slot(Act::Attn)];
                let scale = s.attn_scale();
                for t in 0..*tokens as usize {
                    // Przyczynowość bez maski: zapytanie kafla siedzi na
                    // pozycji `seq - tokens + t`, więc pętla kończy się na niej.
                    let len = *seq as usize - *tokens as usize + t + 1;
                    for h in 0..heads {
                        let kv_head = h / per_kv;
                        let qh = &q[(t * heads + h) * dims..][..dims];
                        let mut scores = vec![0.0f32; len];
                        for (j, sc) in scores.iter_mut().enumerate() {
                            let kj = &kc[j * width + kv_head * dims..][..dims];
                            *sc = qh
                                .iter()
                                .zip(kj)
                                .fold(0.0f32, |a, (x, y)| x.mul_add(*y, a))
                                * scale;
                        }
                        let m = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
                        let mut total = 0.0f32;
                        for sc in scores.iter_mut() {
                            *sc = (*sc - m).exp();
                            total += *sc;
                        }
                        let base = (t * heads + h) * dims;
                        for (j, sc) in scores.iter().enumerate() {
                            let vj = &vc[j * width + kv_head * dims..][..dims];
                            let p = sc / total;
                            for c in 0..dims {
                                out[base + c] = p.mul_add(vj[c], out[base + c]);
                            }
                        }
                    }
                }
                Ok(())
            }

            Op::SiluMul { tokens } => {
                let n = *tokens as usize * s.inter as usize;
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

            Op::Residual { src, tokens } => {
                let delta = self.with(*src, <[f32]>::to_vec);
                let n = *tokens as usize * s.hidden as usize;
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

    fn argmax(&self, act: Act) -> Result<u32> {
        self.with(act, |v| {
            if v.is_empty() {
                return Err(ForgeError::Other("pusty slot do wyboru".into()));
            }
            let mut best = 0usize;
            for (i, &x) in v.iter().enumerate() {
                if x > v[best] {
                    best = i;
                }
            }
            Ok(best as u32)
        })
    }

    fn seq_cap(&self) -> u32 {
        SEQ_CAP
    }

    fn tile(&self) -> Tile {
        Tile {
            max_tokens: MAX_TOKENS,
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
        assert_eq!(all.len(), SLOT_COUNT, "liczba slotów rozjechała się z listą");
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
