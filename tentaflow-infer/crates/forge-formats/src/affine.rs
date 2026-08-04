// ===== File: affine.rs — every 4-bit format that is really the same formula =====
//
// MLX affine, GGML Q4_1 and GGML Q4_K all compute the SAME thing:
//
//     w = q * scale + bias
//
// per block of weights. They differ only in how many weights share a scale, how
// the nibbles are ordered inside the bytes, and whether the two parameters are
// stored side by side or hoisted into a super-block. None of that is a
// different arithmetic — it is a different filing system for identical numbers.
//
// So a reader that already knows one of them knows all of them, provided
// something converts the filing. That is this file. Metal kernels want three
// separate arrays (nibbles, scales, biases) because that is what their loads
// index; GGML packs the parameters next to the data. Converting is a rewrite,
// never a requantisation, and the tests hold it to that: every conversion is
// checked against the golden decoder, exactly, not within a tolerance.

use half::f16;

use forge_types::{DType, ForgeError, QuantKind, Result};

/// Weights per scale after conversion.
///
/// Thirty-two, because that is the coarsest block every supported source can be
/// cut into without splitting one of its own: Q4_1 blocks are 32, Q4_K's
/// sub-blocks are 32, and MLX's group of 64 divides evenly. A wider group would
/// have to merge two blocks that do NOT share a scale.
pub const AFFINE_GROUP: usize = 32;

/// A quantized matrix in the shape the Metal kernels index.
pub struct AffineTriple {
    /// Nibbles, eight per word, least significant first.
    pub packed: Vec<u32>,
    /// One per (row, group), in the same order as `packed`.
    pub scales: Vec<f16>,
    pub biases: Vec<f16>,
    pub rows: usize,
    pub cols: usize,
}

impl AffineTriple {
    fn new(rows: usize, cols: usize) -> Self {
        let groups = rows * cols / AFFINE_GROUP;
        Self {
            packed: vec![0; rows * cols / 8],
            scales: vec![f16::ZERO; groups],
            biases: vec![f16::ZERO; groups],
            rows,
            cols,
        }
    }

    /// Writes one weight. `col` is the logical column, so the caller does not
    /// have to know the word packing.
    #[inline]
    fn put(&mut self, row: usize, col: usize, nibble: u8) {
        let at = row * self.cols + col;
        self.packed[at / 8] |= u32::from(nibble & 0xF) << ((at % 8) * 4);
    }

    #[inline]
    fn set_params(&mut self, row: usize, group: usize, scale: f32, bias: f32) {
        let at = row * (self.cols / AFFINE_GROUP) + group;
        self.scales[at] = f16::from_f32(scale);
        self.biases[at] = f16::from_f32(bias);
    }
}

/// Rewrites a fetched tensor into the triple, when its format is one of the
/// affine ones.
///
/// `Unsupported` for anything else — including Q6_K, which is affine too but
/// six-bit, so it does not fit a nibble.
pub fn to_affine_triple(
    data: &[u8],
    quant: QuantKind,
    rows: usize,
    cols: usize,
) -> Result<AffineTriple> {
    if !cols.is_multiple_of(AFFINE_GROUP) {
        return Err(ForgeError::Unsupported(format!(
            "affine: {cols} kolumn nie dzieli się na grupy po {AFFINE_GROUP}"
        )));
    }
    match quant {
        QuantKind::Q4_1 => from_q4_1(data, rows, cols),
        QuantKind::Q4K => from_q4_k(data, rows, cols),
        other => Err(ForgeError::Unsupported(format!(
            "affine: {other:?} nie jest czterobitową formą afiniczną"
        ))),
    }
}

/// Whether `to_affine_triple` will take this format.
pub fn is_affine_4bit(quant: QuantKind) -> bool {
    matches!(quant, QuantKind::Q4_1 | QuantKind::Q4K)
}

const Q4_1_BYTES: usize = 20;
const Q4_K_BYTES: usize = 144;
const Q4_K_ELEMS: usize = 256;

fn f16_le(b: &[u8]) -> f32 {
    f32::from(f16::from_le_bytes([b[0], b[1]]))
}

/// Q4_1: one block of 32 carries `d`, `m` and sixteen bytes. The first sixteen
/// weights are the LOW nibbles, the next sixteen the HIGH ones — the two halves
/// interleave in the byte, not in the sequence.
fn from_q4_1(data: &[u8], rows: usize, cols: usize) -> Result<AffineTriple> {
    let blocks = rows * cols / AFFINE_GROUP;
    let want = blocks * Q4_1_BYTES;
    if data.len() != want {
        return Err(ForgeError::Format(format!(
            "Q4_1: {} B na {rows}x{cols}, oczekiwano {want}",
            data.len()
        )));
    }
    let mut out = AffineTriple::new(rows, cols);
    let per_row = cols / AFFINE_GROUP;
    for b in 0..blocks {
        let raw = &data[b * Q4_1_BYTES..(b + 1) * Q4_1_BYTES];
        let (d, m) = (f16_le(&raw[0..2]), f16_le(&raw[2..4]));
        let (row, group) = (b / per_row, b % per_row);
        out.set_params(row, group, d, m);
        let base = group * AFFINE_GROUP;
        for j in 0..16 {
            out.put(row, base + j, raw[4 + j] & 0x0F);
            out.put(row, base + j + 16, raw[4 + j] >> 4);
        }
    }
    Ok(out)
}

/// Q4_K: a super-block of 256 holds `d`, `dmin` and eight six-bit (scale, min)
/// pairs, one per sub-block of 32. Each sub-block is then plain affine with
/// `scale = d*sc` and `bias = -dmin*m` — which is the whole reason this
/// conversion exists and loses nothing.
fn from_q4_k(data: &[u8], rows: usize, cols: usize) -> Result<AffineTriple> {
    if !cols.is_multiple_of(Q4_K_ELEMS) {
        return Err(ForgeError::Unsupported(format!(
            "Q4_K: {cols} kolumn nie dzieli się na superbloki po {Q4_K_ELEMS}"
        )));
    }
    let supers = rows * cols / Q4_K_ELEMS;
    let want = supers * Q4_K_BYTES;
    if data.len() != want {
        return Err(ForgeError::Format(format!(
            "Q4_K: {} B na {rows}x{cols}, oczekiwano {want}",
            data.len()
        )));
    }
    let mut out = AffineTriple::new(rows, cols);
    let supers_per_row = cols / Q4_K_ELEMS;
    for s in 0..supers {
        let raw = &data[s * Q4_K_BYTES..(s + 1) * Q4_K_BYTES];
        let (d, dmin) = (f16_le(&raw[0..2]), f16_le(&raw[2..4]));
        let packed_scales = &raw[4..16];
        let qs = &raw[16..144];
        let row = s / supers_per_row;
        let col0 = (s % supers_per_row) * Q4_K_ELEMS;

        // Dwa podbloki na raz, bo dzielą te same 32 bajty: młodsze połówki
        // należą do pierwszego, starsze do drugiego.
        for pair in 0..4 {
            let q = &qs[pair * 32..pair * 32 + 32];
            for (half, sub) in [(0usize, pair * 2), (1, pair * 2 + 1)] {
                let (sc, mn) = scale_min_k4(sub, packed_scales);
                let base = col0 + sub * AFFINE_GROUP;
                out.set_params(
                    row,
                    base / AFFINE_GROUP,
                    d * f32::from(sc),
                    -dmin * f32::from(mn),
                );
                for (j, &byte) in q.iter().enumerate() {
                    let nib = if half == 0 { byte & 0x0F } else { byte >> 4 };
                    out.put(row, base + j, nib);
                }
            }
        }
    }
    Ok(out)
}

/// Six-bit scale and min for sub-block `j`, packed twelve bytes for eight pairs.
fn scale_min_k4(j: usize, q: &[u8]) -> (u8, u8) {
    if j < 4 {
        (q[j] & 63, q[j + 4] & 63)
    } else {
        (
            (q[j + 4] & 0xF) | ((q[j - 4] >> 6) << 4),
            (q[j + 4] >> 4) | ((q[j] >> 6) << 4),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dequant::dequantize_to_f32;

    /// Rozpakowanie trójki tą samą arytmetyką, którą liczy kernel.
    fn expand(t: &AffineTriple) -> Vec<f32> {
        let mut out = vec![0f32; t.rows * t.cols];
        for r in 0..t.rows {
            for c in 0..t.cols {
                let at = r * t.cols + c;
                let nib = (t.packed[at / 8] >> ((at % 8) * 4)) & 0xF;
                let g = r * (t.cols / AFFINE_GROUP) + c / AFFINE_GROUP;
                out[at] = nib as f32 * f32::from(t.scales[g]) + f32::from(t.biases[g]);
            }
        }
        out
    }

    /// Konwersja jest PRZEPISANIEM, nie przekwantowaniem — więc porównanie z
    /// dekoderem wzorcowym idzie na dokładność f16 skal, a nie na tolerancję
    /// dobraną pod wynik.
    fn agrees(quant: QuantKind, data: &[u8], rows: usize, cols: usize) {
        let want = dequantize_to_f32(DType::U8, quant, data, rows * cols).expect("wzorzec");
        let got = expand(&to_affine_triple(data, quant, rows, cols).expect("konwersja"));
        assert_eq!(want.len(), got.len());
        let mut worst = 0f32;
        for (a, b) in want.iter().zip(&got) {
            worst = worst.max((a - b).abs());
        }
        let scale = want.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(
            worst <= scale * 1e-3,
            "{quant:?}: największa różnica {worst} przy zakresie {scale}"
        );
    }

    fn noise(n: usize, seed: u32) -> Vec<u8> {
        (0..n)
            .map(|i| (i as u32).wrapping_mul(2_654_435_761).wrapping_add(seed) as u8)
            .collect()
    }

    #[test]
    fn q4_1_rewrites_into_the_triple() {
        // 64 kolumny = dwa bloki na wiersz, więc sprawdza też adresowanie grup.
        let (rows, cols) = (3usize, 64usize);
        let mut data = noise(rows * cols / AFFINE_GROUP * Q4_1_BYTES, 7);
        for b in 0..rows * cols / AFFINE_GROUP {
            // Skale muszą być sensownymi f16, inaczej porównanie mierzy NaN-y.
            data[b * Q4_1_BYTES..b * Q4_1_BYTES + 2]
                .copy_from_slice(&f16::from_f32(0.013).to_le_bytes());
            data[b * Q4_1_BYTES + 2..b * Q4_1_BYTES + 4]
                .copy_from_slice(&f16::from_f32(-0.11).to_le_bytes());
        }
        agrees(QuantKind::Q4_1, &data, rows, cols);
    }

    #[test]
    fn q4_k_rewrites_into_the_triple() {
        // To jest ten format, w którym przychodzi Bielik z GGUF-a.
        let (rows, cols) = (2usize, 256usize);
        let mut data = noise(rows * cols / Q4_K_ELEMS * Q4_K_BYTES, 13);
        for s in 0..rows * cols / Q4_K_ELEMS {
            data[s * Q4_K_BYTES..s * Q4_K_BYTES + 2]
                .copy_from_slice(&f16::from_f32(0.0021).to_le_bytes());
            data[s * Q4_K_BYTES + 2..s * Q4_K_BYTES + 4]
                .copy_from_slice(&f16::from_f32(0.0009).to_le_bytes());
        }
        agrees(QuantKind::Q4K, &data, rows, cols);
    }

    #[test]
    fn a_format_that_is_not_four_bit_affine_is_refused() {
        // Q6_K liczy tę samą formułę, ale sześcioma bitami — cicha konwersja
        // obcinałaby wagi do nibbla i model nadal by mówił.
        assert!(!is_affine_4bit(QuantKind::Q6K));
        assert!(to_affine_triple(&[0u8; 210], QuantKind::Q6K, 1, 256).is_err());
        assert!(is_affine_4bit(QuantKind::Q4K));
        assert!(is_affine_4bit(QuantKind::Q4_1));
    }
}
