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
use std::collections::HashMap;

use half::{bf16, f16};

use forge_formats::affine::AffineTriple;
use forge_formats::dequant::dequantize_to_f32;
use forge_graph::{
    Act, ExecSpec, Executor, Layout, Op, QuantWeight, SsmShape, Tile, WeightId, WeightStore,
};
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
    /// Typ zapisu — istotny wyłącznie dla wagi niekwantyzowanej, gdzie JEST
    /// jej formatem.
    dtype: DType,
    /// Skalar całego tensora, gdy format go ma. Dekoder bloków go nie zna, bo
    /// to własność tensora, a nie bloku.
    global: Option<f32>,
    rows: usize,
    cols: usize,
}

impl HostBlocks {
    /// Bytes of one row. Every block format here is row-major over whole
    /// blocks, which is the same fact the RoPE row permutation stands on.
    fn row_bytes(&self) -> Result<usize> {
        if self.quant == QuantKind::None {
            // Waga niekwantyzowana nie ma bloków — wiersz to kolumny razy
            // szerokość jej typu, i to ten typ jest tu formatem.
            return Ok(self.cols * plain_width(self.dtype)?);
        }
        Ok(self.cols / self.quant.block_elems() * self.quant.block_bytes())
    }

    fn row_into(&self, row: usize, out: &mut [f32]) -> Result<()> {
        let width = self.row_bytes()?;
        // Dla wagi niekwantyzowanej dekoderowi trzeba podać JEJ typ; podanie
        // f32 przeczytałoby pary bajtów f16 jako połówki innych liczb.
        let mut decoded = dequantize_to_f32(
            self.dtype,
            self.quant,
            &self.data[row * width..(row + 1) * width],
            self.cols,
        )?;
        // Skalar tensora wchodzi dopiero tutaj, bo dekoder bloków go nie zna —
        // to własność tensora, a nie bloku. MNOŻY, bo płaszczyzna niesie już
        // odwrotność `weight_global_scale`; to jest ta sama liczba, którą
        // kernele dostają jako `output_scale`, i obie strony muszą jej użyć tak
        // samo, inaczej różnią się o jej kwadrat.
        if let Some(global) = self.global {
            for v in decoded.iter_mut() {
                *v *= global;
            }
        }
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

const SLOT_COUNT: usize = 12;

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
        Act::AttnGate => 11,
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
    /// Convolution window and state matrix per (layer, slot). Grown on demand,
    /// like the caches above and for the same reason: which layers are
    /// recurrent is stated by the operations, not by the shape.
    recurrent: RefCell<HashMap<(usize, usize), (Vec<f32>, Vec<f32>)>>,
    shape: DenseShape,
    ssm: Option<SsmShape>,
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
            recurrent: RefCell::new(HashMap::new()),
            shape: spec.shape,
            ssm: spec.ssm,
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
            QuantWeight::Packed(p) => {
                // Wzorzec dekoduje bloki. Inny układ zatrzyma się tutaj, bo
                // jego bajty znaczą co innego niż to, co czyta dekoder.
                if p.layout != Layout::Blocks {
                    return Err(ForgeError::Unsupported(format!(
                        "{:?} w układzie {:?}, a wzorzec czyta bloki",
                        p.quant, p.layout
                    )));
                }
                // Osobnej PŁASZCZYZNY skal wzorzec nie czyta — jego dekoder
                // bierze je z wnętrza bloku; skalar tensora owszem.
                if p.planes.scales.is_some() {
                    return Err(ForgeError::Unsupported(format!(
                        "{:?} niesie skale poza kodami, a wzorzec czyta je z bloku",
                        p.quant
                    )));
                }
                // Sprawdzane TU, przy wgraniu: wiersz, który nie dzieli się na
                // całe bloki, adresowałby cudzy blok przy każdym odczycie.
                if !p.cols.is_multiple_of(p.quant.block_elems()) {
                    return Err(ForgeError::Unsupported(format!(
                        "{} kolumn nie dzieli się na bloki {:?} po {}",
                        p.cols,
                        p.quant,
                        p.quant.block_elems()
                    )));
                }
                self.weights.push(HostWeight::Blocks(HostBlocks {
                    data: p.planes.codes,
                    quant: p.quant,
                    dtype: p.dtype,
                    global: p.planes.global,
                    rows: p.rows,
                    cols: p.cols,
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

            // The same formula as above, except a row is a HEAD. Computed in
            // place, so the slot does not change size — and were it to, the
            // attention would read rows from a different split.
            Op::HeadNorm { act, w, heads, step } => {
                let weight = self.plain(*w)?.to_vec();
                let n = s.head_dim as usize;
                let rows = step.rows() as usize * *heads as usize;
                let mut acts = self.acts.borrow_mut();
                let dst = &mut acts.by[slot(*act)];
                if dst.len() < rows * n {
                    return Err(ForgeError::Other(format!(
                        "norma głowic chce {} wartości, a slot ma {}",
                        rows * n,
                        dst.len()
                    )));
                }
                for t in 0..rows {
                    let row = &mut dst[t * n..(t + 1) * n];
                    let mean = row.iter().map(|v| v * v).sum::<f32>() / n as f32;
                    let scale = 1.0 / (mean + s.eps).sqrt();
                    for (c, v) in row.iter_mut().enumerate() {
                        *v = *v * scale * weight[c];
                    }
                }
                Ok(())
            }

            Op::MatMul { out, w, x, step } => {
                self.matmul(*out, self.quant(*w)?, *x, step.rows() as usize)
            }

            Op::FusedNormMatMul {
                out,
                w,
                norm_w,
                x,
                step,
            } => {
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
            Op::FusedMatMulResidual { w, x, step } => {
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
                // A partial rotary turns only the first `rot` dimensions and
                // pairs j with j + rot/2 — NOT with j + head_dim/2. Deriving
                // the pairing from the head width instead would rotate real
                // pairs at plausible angles and answer about another position.
                let rot = s.rope_rot as usize;
                let half = rot / 2;
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
                                    * s.rope_theta.powf(-2.0 * i as f32 / rot as f32);
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

            // The oracle for the recurrent mixer. Every piece of it is a
            // function in `forge-formats::deltanet`, which is the SAME
            // definition the Mojo kernels were written against — so this arm is
            // composition, not a second derivation that could disagree.
            Op::DeltaNet {
                out,
                x,
                layer,
                w,
                step,
            } => {
                use forge_formats::deltanet as dn;
                let ssm = self.ssm.ok_or_else(|| {
                    ForgeError::Unsupported(
                        "DeltaNet: wzorzec powstał bez geometrii miksera".into(),
                    )
                })?;
                let hidden = s.hidden as usize;
                let (key, value) = (ssm.key_width() as usize, ssm.value_width() as usize);
                let (v_heads, d_state) = (ssm.v_heads as usize, ssm.d_state as usize);
                let taps = ssm.d_conv as usize;
                let k_heads = ssm.k_heads as usize;
                let rows = step.rows() as usize;
                let src = self.with(*x, <[f32]>::to_vec);
                self.resize(*out, rows * hidden);

                let project = |w: WeightId, n: usize, row: &[f32]| -> Result<Vec<f32>> {
                    let m = self.quant(w)?;
                    let mut scratch = vec![0.0f32; row.len()];
                    (0..n)
                        .map(|i| {
                            m.row_into(i, &mut scratch)?;
                            Ok(scratch.iter().zip(row).map(|(a, b)| a * b).sum())
                        })
                        .collect()
                };
                let conv_w = self.plain(w.conv)?;
                let dt_bias = self.plain(w.dt_bias)?;
                let a_scale = self.plain(w.a)?;
                let norm_w = self.plain(w.norm)?;
                let mixed_width = ssm.mixed_width() as usize;

                let mut held = self.recurrent.borrow_mut();
                let tokens = step.tokens() as usize;
                for (lane, l) in step.lanes().iter().enumerate() {
                    let carried = held.entry((*layer, l.slot as usize)).or_insert_with(|| {
                        (
                            vec![0.0f32; mixed_width * taps.saturating_sub(1)],
                            vec![0.0f32; v_heads * d_state * d_state],
                        )
                    });
                    // Position zero means the sequence starts here, so whatever
                    // the previous occupant of this slot folded in is not its
                    // history.
                    if l.pos == 0 {
                        carried.0.fill(0.0);
                        carried.1.fill(0.0);
                    }
                    for t in 0..tokens {
                        let row = &src[(lane * tokens + t) * hidden..][..hidden];
                        let mixed = project(w.qkv, mixed_width, row)?;
                        let z = project(w.gate, value, row)?;
                        let alpha = project(w.alpha, v_heads, row)?;
                        let beta_raw = project(w.beta, v_heads, row)?;

                        // Causal convolution + SiLU, one channel at a time, the
                        // window advancing behind it.
                        let win = taps - 1;
                        let mut convolved = vec![0.0f32; mixed_width];
                        for c in 0..mixed_width {
                            let window = &carried.0[c * win..(c + 1) * win];
                            let taps_c = &conv_w[c * taps..(c + 1) * taps];
                            convolved[c] = dn::silu(dn::causal_conv1d_step(window, mixed[c], taps_c));
                        }
                        for c in 0..mixed_width {
                            let window = &mut carried.0[c * win..(c + 1) * win];
                            dn::causal_conv1d_advance(window, mixed[c]);
                        }

                        // Query, key and value lie end to end; q and k are
                        // normalized per KEY head and then repeated to cover the
                        // value heads, which is what makes head h read key head
                        // h % k_heads.
                        let mut q = convolved[..key].to_vec();
                        let mut k = convolved[key..2 * key].to_vec();
                        let v = &convolved[2 * key..];
                        for head in 0..k_heads {
                            dn::l2_norm(&mut q[head * d_state..(head + 1) * d_state], s.eps);
                            dn::l2_norm(&mut k[head * d_state..(head + 1) * d_state], s.eps);
                        }

                        let mut answer = vec![0.0f32; value];
                        for head in 0..v_heads {
                            let from = (head % k_heads) * d_state;
                            let g = dn::delta_log_decay(alpha[head], dt_bias[head], a_scale[head]);
                            let beta = 1.0 / (1.0 + (-beta_raw[head]).exp());
                            let matrix =
                                &mut carried.1[head * d_state * d_state..][..d_state * d_state];
                            dn::gated_delta_step(
                                matrix,
                                d_state,
                                &q[from..from + d_state],
                                &k[from..from + d_state],
                                &v[head * d_state..(head + 1) * d_state],
                                g,
                                beta,
                                &mut answer[head * d_state..(head + 1) * d_state],
                            );
                        }

                        let mut normed = vec![0.0f32; value];
                        for head in 0..v_heads {
                            let span = head * d_state..(head + 1) * d_state;
                            dn::gated_rmsnorm(
                                &answer[span.clone()],
                                &z[span.clone()],
                                norm_w,
                                s.eps,
                                &mut normed[span],
                            );
                        }
                        let projected = project(w.out, hidden, &normed)?;
                        let mut acts = self.acts.borrow_mut();
                        let dst = &mut acts.by[slot(*out)];
                        let base = (lane * tokens + t) * hidden;
                        dst[base..base + hidden].copy_from_slice(&projected);
                    }
                }
                Ok(())
            }

            Op::SigmoidMul { act, gate, step } => {
                let n = step.rows() as usize * s.attn_width() as usize;
                let g = self.with(*gate, <[f32]>::to_vec);
                let mut acts = self.acts.borrow_mut();
                let dst = &mut acts.by[slot(*act)];
                if dst.len() < n || g.len() < n {
                    return Err(ForgeError::Other(format!(
                        "bramka {} i wejście {} nie pokrywają {n} wierszy",
                        g.len(),
                        dst.len()
                    )));
                }
                for i in 0..n {
                    dst[i] *= 1.0 / (1.0 + (-g[i]).exp());
                }
                Ok(())
            }

            // The oracle for the mixture: everything in f32 and everything
            // spelled out. The router multiplies, a softmax weights, the top
            // few are selected, and each chosen expert computes SwiGLU over ITS
            // OWN window of the stack. No residency and no device-side
            // indexing — those belong to an executor, while the reference only
            // has to answer which result is the right one.
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
                step,
            } => {
                let hidden = s.hidden as usize;
                let inter = s.inter as usize;
                let (experts, top_k) = (*experts as usize, *top_k as usize);
                let src = self.with(*x, <[f32]>::to_vec);
                let rows = step.rows() as usize;
                let (router, gate_w, up_w, down_w) = (
                    self.quant(*router)?,
                    self.quant(*gate)?,
                    self.quant(*up)?,
                    self.quant(*down)?,
                );
                self.resize(*out, rows * hidden);

                let mut logits = vec![0.0f32; experts];
                let mut row = vec![0.0f32; hidden.max(inter)];
                let mut acc = vec![0.0f32; hidden];
                let mut activated = vec![0.0f32; inter];
                for t in 0..rows {
                    let xt = &src[t * hidden..(t + 1) * hidden];
                    for (e, logit) in logits.iter_mut().enumerate() {
                        router.row_into(e, &mut row[..hidden])?;
                        *logit = row[..hidden].iter().zip(xt).map(|(w, v)| w * v).sum();
                    }
                    // Softmax over ALL experts and only then the selection.
                    // The other order yields different weights for the same
                    // choice.
                    let peak = logits.iter().copied().fold(f32::MIN, f32::max);
                    let mut probs: Vec<f32> = logits.iter().map(|l| (l - peak).exp()).collect();
                    let total: f32 = probs.iter().sum();
                    for p in probs.iter_mut() {
                        *p /= total;
                    }
                    let mut order: Vec<usize> = (0..experts).collect();
                    order.sort_by(|a, b| {
                        probs[*b]
                            .partial_cmp(&probs[*a])
                            .unwrap_or(std::cmp::Ordering::Equal)
                            .then(a.cmp(b))
                    });
                    let chosen = &order[..top_k];
                    let scale = if *norm_topk {
                        let sum: f32 = chosen.iter().map(|e| probs[*e]).sum();
                        1.0 / sum
                    } else {
                        1.0
                    };

                    acc.fill(0.0);
                    for &e in chosen {
                        let base = e * inter;
                        for i in 0..inter {
                            gate_w.row_into(base + i, &mut row[..hidden])?;
                            let g: f32 = row[..hidden].iter().zip(xt).map(|(w, v)| w * v).sum();
                            up_w.row_into(base + i, &mut row[..hidden])?;
                            let u: f32 = row[..hidden].iter().zip(xt).map(|(w, v)| w * v).sum();
                            activated[i] = g / (1.0 + (-g).exp()) * u;
                        }
                        let weight = probs[e] * scale;
                        let base = e * hidden;
                        for (h, a) in acc.iter_mut().enumerate() {
                            down_w.row_into(base + h, &mut row[..inter])?;
                            let v: f32 =
                                row[..inter].iter().zip(&activated).map(|(w, z)| w * z).sum();
                            *a += weight * v;
                        }
                    }
                    // The always-on expert lands ON TOP of the routed sum, with
                    // its own per-token gate rather than a routing weight.
                    if let Some(sh) = shared {
                        let (sg, su, sd) =
                            (self.quant(sh.gate)?, self.quant(sh.up)?, self.quant(sh.down)?);
                        // This expert's width is its own, and only the stacks'
                        // shapes state it. The shape's `inter` is the WIDEST
                        // feed-forward in the model, so it bounds the scratch
                        // without being this expert's width.
                        let (width, _) = sg.shape();
                        if width > inter || su.shape().0 != width || sd.shape().1 != width {
                            return Err(ForgeError::Unsupported(format!(
                                "ekspert współdzielony: gate {width}, up {}, down×{} \
                                 przy szerokości pośredniej {inter}",
                                su.shape().0,
                                sd.shape().1
                            )));
                        }
                        self.quant(sh.router)?.row_into(0, &mut row[..hidden])?;
                        let logit: f32 = row[..hidden].iter().zip(xt).map(|(w, v)| w * v).sum();
                        let weight = 1.0 / (1.0 + (-logit).exp());
                        for i in 0..width {
                            sg.row_into(i, &mut row[..hidden])?;
                            let g: f32 = row[..hidden].iter().zip(xt).map(|(w, v)| w * v).sum();
                            su.row_into(i, &mut row[..hidden])?;
                            let u: f32 = row[..hidden].iter().zip(xt).map(|(w, v)| w * v).sum();
                            activated[i] = g / (1.0 + (-g).exp()) * u;
                        }
                        for (h, a) in acc.iter_mut().enumerate() {
                            sd.row_into(h, &mut row[..width])?;
                            let v: f32 = row[..width]
                                .iter()
                                .zip(&activated[..width])
                                .map(|(w, z)| w * z)
                                .sum();
                            *a += weight * v;
                        }
                    }
                    let mut acts = self.acts.borrow_mut();
                    acts.by[slot(*out)][t * hidden..(t + 1) * hidden].copy_from_slice(&acc);
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

/// Bajtów na wartość w wadze niekwantyzowanej.
fn plain_width(dtype: DType) -> Result<usize> {
    match dtype {
        DType::F16 | DType::BF16 => Ok(2),
        DType::F32 => Ok(4),
        other => Err(ForgeError::Unsupported(format!(
            "waga niekwantyzowana ma typ {other:?}"
        ))),
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
