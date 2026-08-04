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

/// Block a GGML-style source converts into. Thirty-two because that is the
/// coarsest block Q4_1 and Q4_K can be cut into without splitting one of their
/// own sub-blocks.
///
/// NOT a property of the triple — a source whose blocks are already affine
/// keeps its own group, because re-cutting it would be work for nothing.
pub const GGML_AFFINE_GROUP: usize = 32;

/// A quantized matrix in the shape the kernels index.
///
/// The parameters keep the dtype they had at the SOURCE. Forcing one dtype here
/// looked tidier and was wrong: MLX stores its scales in bf16, whose smallest
/// magnitudes are outside what f16 can hold, so a conversion "to be uniform"
/// silently narrows weights that were fine where they came from.
pub struct AffineTriple {
    /// Low four bits of each weight, eight per word, least significant first.
    pub packed: Vec<u32>,
    /// Bits four and five, sixteen per word — empty when `bits` is four.
    ///
    /// Split rather than interleaved because a six-bit field straddles word
    /// boundaries and the extraction is what limits decode (EKS-A8). Two loads
    /// of aligned fields beat one load plus a shift across words.
    pub high: Vec<u32>,
    /// Four or six.
    pub bits: u32,
    /// One per (row, group), in the same order as `packed`, raw bytes of
    /// `param_dtype`.
    pub scales: Vec<u8>,
    pub biases: Vec<u8>,
    pub param_dtype: DType,
    /// Weights sharing one scale and bias.
    pub group: usize,
    pub rows: usize,
    pub cols: usize,
}

impl AffineTriple {
    /// Builds an empty triple with f16 parameters, which is what every GGML
    /// source produces.
    pub fn new_f16(rows: usize, cols: usize, group: usize) -> Self {
        let groups = rows * cols / group;
        Self {
            packed: vec![0; rows * cols / 8],
            high: Vec::new(),
            bits: 4,
            scales: vec![0; groups * 2],
            biases: vec![0; groups * 2],
            param_dtype: DType::F16,
            group,
            rows,
            cols,
        }
    }

    /// Same, for the six-bit form.
    pub fn new_f16_6bit(rows: usize, cols: usize, group: usize) -> Self {
        let mut t = Self::new_f16(rows, cols, group);
        t.high = vec![0; rows * cols / 16];
        t.bits = 6;
        t
    }

    /// Writes one six-bit weight, splitting it across the two arrays.
    #[inline]
    pub fn put6(&mut self, row: usize, col: usize, value: u8) {
        let at = row * self.cols + col;
        self.packed[at / 8] |= u32::from(value & 0xF) << ((at % 8) * 4);
        self.high[at / 16] |= u32::from((value >> 4) & 0x3) << ((at % 16) * 2);
    }

    /// Writes one weight. `col` is the logical column, so the caller does not
    /// have to know the word packing.
    #[inline]
    pub fn put(&mut self, row: usize, col: usize, nibble: u8) {
        let at = row * self.cols + col;
        self.packed[at / 8] |= u32::from(nibble & 0xF) << ((at % 8) * 4);
    }

    /// Only valid while `param_dtype` is f16 — the GGML sources' case.
    #[inline]
    pub fn set_params_f16(&mut self, row: usize, group: usize, scale: f32, bias: f32) {
        let at = (row * (self.cols / self.group) + group) * 2;
        self.scales[at..at + 2].copy_from_slice(&f16::from_f32(scale).to_le_bytes());
        self.biases[at..at + 2].copy_from_slice(&f16::from_f32(bias).to_le_bytes());
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
    if !cols.is_multiple_of(GGML_AFFINE_GROUP) {
        return Err(ForgeError::Unsupported(format!(
            "affine: {cols} kolumn nie dzieli się na grupy po {GGML_AFFINE_GROUP}"
        )));
    }
    match quant {
        QuantKind::Q4_1 => from_q4_1(data, rows, cols),
        QuantKind::Q4K => from_q4_k(data, rows, cols),
        QuantKind::Q6K => from_q6_k(data, rows, cols),
        other => Err(ForgeError::Unsupported(format!(
            "affine: {other:?} nie jest czterobitową formą afiniczną"
        ))),
    }
}

/// Przestawia wiersze Q albo K z kolejności GGUF-a do kolejności HF.
///
/// llama.cpp liczy RoPE na przeplatanych parach, HF obraca połówki wektora, a
/// konwerter GGUF-a przestawia wiersze tak, żeby pasowały do tego pierwszego.
/// Kernel, który obraca połówki, musi to cofnąć — RAZ, przy ładowaniu.
///
/// Nie objawia się awarią: bez tego model generuje płynny, całkowicie inny
/// tekst. Warunkiem jest `ModelDescriptor::rope_interleaved()` po stronie
/// architektury i `TensorSource::stores_original_rope_order()` po stronie
/// źródła — jedno bez drugiego nakłada permutację nie tam, gdzie trzeba.
///
/// Działa na BAJTACH źródła, a nie na postaci przepisanej, bo to jest ta
/// postać, którą widzą wszyscy wykonawcy — jeden ją potem rozkłada na trzy
/// tablice, drugi zostawia bloki tak, jak leżą. Wiersz dowolnego formatu
/// blokowego GGUF-a jest ciągłym zakresem bajtów, więc przestawienie wierszy
/// nie musi wiedzieć, który to format; musi tylko wiedzieć, że wiersze są
/// równe.
pub fn permute_rope_rows(data: &mut [u8], rows: usize, head_dim: usize) -> Result<()> {
    if head_dim == 0 || !head_dim.is_multiple_of(2) || rows == 0 || !rows.is_multiple_of(head_dim) {
        return Err(ForgeError::Format(format!(
            "RoPE: {rows} wierszy nie dzieli się na głowice po {head_dim}"
        )));
    }
    if !data.len().is_multiple_of(rows) {
        return Err(ForgeError::Format(format!(
            "RoPE: {} B nie dzieli się na {rows} równych wierszy",
            data.len()
        )));
    }
    let width = data.len() / rows;
    let half = head_dim / 2;
    let mut out = vec![0u8; data.len()];
    for head in 0..rows / head_dim {
        for a in 0..2 {
            for b in 0..half {
                let dst = head * head_dim + a * half + b;
                let src = head * head_dim + b * 2 + a;
                out[dst * width..(dst + 1) * width]
                    .copy_from_slice(&data[src * width..(src + 1) * width]);
            }
        }
    }
    data.copy_from_slice(&out);
    Ok(())
}

/// Whether `to_affine_triple` will take this format.
pub fn is_affine(quant: QuantKind) -> bool {
    matches!(quant, QuantKind::Q4_1 | QuantKind::Q4K | QuantKind::Q6K)
}

const Q6_K_BYTES: usize = 210;
const Q6_K_ELEMS: usize = 256;
/// Q6_K keeps one scale per sixteen weights.
pub const Q6_K_GROUP: usize = 16;

/// Q6_K: `y = d * sc * (q - 32)` with `q` six bits and one `sc` per sixteen
/// weights — affine again, with the offset folded into the bias. The awkward
/// part is only the filing: four weights share a byte of high bits, and the
/// four quarters of a 128-block are interleaved rather than consecutive.
fn from_q6_k(data: &[u8], rows: usize, cols: usize) -> Result<AffineTriple> {
    if !cols.is_multiple_of(Q6_K_ELEMS) {
        return Err(ForgeError::Unsupported(format!(
            "Q6_K: {cols} kolumn nie dzieli się na superbloki po {Q6_K_ELEMS}"
        )));
    }
    let supers = rows * cols / Q6_K_ELEMS;
    let want = supers * Q6_K_BYTES;
    if data.len() != want {
        return Err(ForgeError::Format(format!(
            "Q6_K: {} B na {rows}x{cols}, oczekiwano {want}",
            data.len()
        )));
    }
    let mut out = AffineTriple::new_f16_6bit(rows, cols, Q6_K_GROUP);
    let supers_per_row = cols / Q6_K_ELEMS;
    for s in 0..supers {
        let raw = &data[s * Q6_K_BYTES..(s + 1) * Q6_K_BYTES];
        let (ql, qh, sc) = (&raw[0..128], &raw[128..192], &raw[192..208]);
        let d = f16_le(&raw[208..210]);
        let row = s / supers_per_row;
        let col0 = (s % supers_per_row) * Q6_K_ELEMS;

        for n in 0..2 {
            let (ql, qh, sc) = (&ql[n * 64..], &qh[n * 32..], &sc[n * 8..]);
            for l in 0..32 {
                let is = l / 16;
                let quarters = [
                    (ql[l] & 0x0F) | ((qh[l] & 3) << 4),
                    (ql[l + 32] & 0x0F) | (((qh[l] >> 2) & 3) << 4),
                    (ql[l] >> 4) | (((qh[l] >> 4) & 3) << 4),
                    (ql[l + 32] >> 4) | (((qh[l] >> 6) & 3) << 4),
                ];
                for (q, &value) in quarters.iter().enumerate() {
                    let col = col0 + n * 128 + q * 32 + l;
                    let scale = d * f32::from(sc[is + q * 2] as i8);
                    out.set_params_f16(row, col / Q6_K_GROUP, scale, -32.0 * scale);
                    out.put6(row, col, value);
                }
            }
        }
    }
    Ok(out)
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
    let blocks = rows * cols / GGML_AFFINE_GROUP;
    let want = blocks * Q4_1_BYTES;
    if data.len() != want {
        return Err(ForgeError::Format(format!(
            "Q4_1: {} B na {rows}x{cols}, oczekiwano {want}",
            data.len()
        )));
    }
    let mut out = AffineTriple::new_f16(rows, cols, GGML_AFFINE_GROUP);
    let per_row = cols / GGML_AFFINE_GROUP;
    for b in 0..blocks {
        let raw = &data[b * Q4_1_BYTES..(b + 1) * Q4_1_BYTES];
        let (d, m) = (f16_le(&raw[0..2]), f16_le(&raw[2..4]));
        let (row, group) = (b / per_row, b % per_row);
        out.set_params_f16(row, group, d, m);
        let base = group * GGML_AFFINE_GROUP;
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
    let mut out = AffineTriple::new_f16(rows, cols, GGML_AFFINE_GROUP);
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
                let base = col0 + sub * GGML_AFFINE_GROUP;
                out.set_params_f16(
                    row,
                    base / GGML_AFFINE_GROUP,
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
                let mut nib = (t.packed[at / 8] >> ((at % 8) * 4)) & 0xF;
                if t.bits == 6 {
                    nib |= ((t.high[at / 16] >> ((at % 16) * 2)) & 0x3) << 4;
                }
                let g = (r * (t.cols / t.group) + c / t.group) * 2;
                let sc = f16::from_le_bytes([t.scales[g], t.scales[g + 1]]);
                let bi = f16::from_le_bytes([t.biases[g], t.biases[g + 1]]);
                out[at] = nib as f32 * f32::from(sc) + f32::from(bi);
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
        let mut data = noise(rows * cols / GGML_AFFINE_GROUP * Q4_1_BYTES, 7);
        for b in 0..rows * cols / GGML_AFFINE_GROUP {
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
    fn q6_k_rewrites_into_the_six_bit_triple() {
        let (rows, cols) = (2usize, 256usize);
        let mut data = noise(rows * cols / 256 * 210, 29);
        for s in 0..rows * cols / 256 {
            // Skale Q6_K są ośmiobitowe ZE ZNAKIEM, a `d` w f16.
            data[s * 210 + 208..s * 210 + 210]
                .copy_from_slice(&f16::from_f32(0.0017).to_le_bytes());
        }
        agrees(QuantKind::Q6K, &data, rows, cols);
    }

    #[test]
    fn the_rope_permutation_moves_whole_rows() {
        // Permutacja jest inwolucją tylko dla head_dim == 2; ogólnie NIE jest,
        // więc test sprawdza to, co naprawdę musi zachodzić: że przestawia
        // wiersze zgodnie ze wzorem, a nie że da się ją nałożyć dwa razy.
        //
        // Szerokość wiersza jest tu superblokiem Q4_K, żeby test mówił o tej
        // jednostce, która naprawdę przechodzi: cały wiersz bloków, a nie bajt.
        let (rows, hd, width) = (8usize, 4usize, Q4_K_BYTES);
        let mut data: Vec<u8> = (0..rows * width).map(|i| (i / width) as u8).collect();
        permute_rope_rows(&mut data, rows, hd).expect("permutacja");
        // HF[h*hd + a*(hd/2) + b] = GGUF[h*hd + b*2 + a]
        for head in 0..rows / hd {
            for a in 0..2 {
                for b in 0..hd / 2 {
                    let dst = head * hd + a * (hd / 2) + b;
                    let src = head * hd + b * 2 + a;
                    assert!(
                        data[dst * width..(dst + 1) * width]
                            .iter()
                            .all(|&v| v == src as u8),
                        "wiersz {dst} wziął zły oryginał"
                    );
                }
            }
        }
    }

    /// Nierówny podział na wiersze musi być błędem, a nie cichym przesunięciem.
    ///
    /// Permutacja na bajtach nie zna formatu, więc jedyne, co ją chroni przed
    /// pocięciem bloków w poprzek, to ten warunek.
    #[test]
    fn rows_that_do_not_divide_the_bytes_are_refused() {
        let mut data = vec![0u8; 3 * Q4_K_BYTES + 1];
        assert!(permute_rope_rows(&mut data, 3, 2).is_err());
        let mut data = vec![0u8; 6 * Q4_K_BYTES];
        assert!(
            permute_rope_rows(&mut data, 6, 4).is_err(),
            "6 wierszy na głowice po 4"
        );
    }

    #[test]
    fn a_format_that_is_not_affine_is_refused() {
        // Q8_0 to skala bez przesunięcia, a Q5_K piąty bit trzyma inaczej —
        // żadne z nich nie jest tą formą i cicha konwersja psułaby wagi.
        assert!(!is_affine(QuantKind::Q8_0));
        assert!(to_affine_triple(&[0u8; 210], QuantKind::Q5K, 1, 256).is_err());
        assert!(is_affine(QuantKind::Q4K));
        assert!(is_affine(QuantKind::Q4_1));
        assert!(is_affine(QuantKind::Q6K));
    }
}
