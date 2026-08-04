// ===== File: mxfp4.rs — eksperci MXFP4 DeepSeeka w układzie NVFP4 =====
//
// DeepSeek publikuje ten sam model w dwóch kwantyzacjach rodziny FP4 i obie
// muszą działać. Różnią się wyłącznie skalą, nie wartościami:
//
//   NVFP4  kody e2m1, skala UE4M3 co 16 wartości, plus skalar na tensor
//   MXFP4  kody e2m1, skala  E8M0 co 32 wartości, bez skalara
//
// Kody są identyczne, więc MXFP4 nie potrzebuje własnych kerneli — wystarczy
// PRZELICZYĆ go na układ NVFP4, którego kernele już używają. Przeliczenie jest
// DOKŁADNE, bo skala E8M0 to czysta potęga dwójki, a UE4M3 reprezentuje potęgi
// dwójki bezbłędnie (mantysa zero). Jedno 32-elementowe pole skali MXFP4
// pokrywa dwa 16-elementowe pola NVFP4 o tej samej wartości.
//
// Ograniczeniem jest OKNO: UE4M3 z mantysą zero sięga od 2^-6 do 2^7, czyli
// trzyna≤cie wykładników, podczas gdy E8M0 ma cały zakres ośmiobitowy. Dlatego
// wykładniki tensora są przesuwane wspólnym czynnikiem, a ten ląduje w skali
// globalnej NVFP4 (`weight_scale_2`) — kernele i tak przez nią mnożą, więc
// konwencja się zgadza. Rozrzut zmierzony na wagach DeepSeek-V4-Flash-DSpark to
// 2..9 wykładników (200 tensorów), czyli mieści się z zapasem; gdyby jakiś
// tensor wypadł poza okno, jest to BŁĄD, a nie ciche obcięcie.

use forge_types::{ForgeError, Result};

use crate::nvfp4::DeepseekNvFp4Weight;

fn fmt_err(message: String) -> ForgeError {
    ForgeError::Format(message)
}

/// Nazwy tensorów eksperta MXFP4. DeepSeek nazywa skalę `.scale`, podczas gdy
/// eksport NVFP4 używa `.weight_scale` i `.weight_scale_2` — po tym właśnie
/// odróżniamy oba formaty przy ładowaniu.
pub struct DeepseekMxFp4Names {
    pub packed: String,
    pub scale: String,
}

impl DeepseekMxFp4Names {
    pub fn for_weight(weight_name: &str) -> Result<Self> {
        let base = weight_name.strip_suffix(".weight").ok_or_else(|| {
            fmt_err(format!(
                "mxfp4: '{weight_name}' nie jest nazwą tensora '.weight'"
            ))
        })?;
        Ok(Self {
            packed: weight_name.to_string(),
            scale: format!("{base}.scale"),
        })
    }
}

/// Ile wartości pokrywa jedna skala w każdym z formatów.
const MX_GROUP: usize = 32;
const NV_GROUP: usize = 16;
/// Blok GGUF NVFP4: cztery skale UE4M3, potem 32 bajty par E2M1.
const NV_BLOCK_VALUES: usize = 64;
const NV_BLOCK_BYTES: usize = 36;

/// Przelicza wykładnik E8M0 na pole wykładnika UE4M3 przy przesunięciu `shift`.
///
/// E8M0 o bajcie `e` znaczy 2^(e-127). Dekoder GGUF czyta UE4M3 jako
/// `(1 + m/8) * 2^(E-7) / 2`, a stałe kodów są podwojone, więc przy mantysie
/// zero wartość efektywna to 2^(E-7). Stąd E = e - 120 - shift.
fn ue4m3_exponent(e: u8, shift: i32) -> i32 {
    e as i32 - 120 - shift
}

/// Nibble elementu `index` w wierszu spakowanym po dwa na bajt: młodsza
/// półbajtówka to mniejszy indeks.
fn nibble(packed_row: &[u8], index: usize) -> u8 {
    let byte = packed_row[index / 2];
    if index % 2 == 0 {
        byte & 0x0F
    } else {
        byte >> 4
    }
}

/// Przepakowuje eksperta MXFP4 na jednobuforowy układ GGUF NVFP4.
///
/// `packed_shape` opisuje bajty pakietu (`[rows, cols/2]`), `scales` to
/// `rows * cols/32` bajtów E8M0.
pub fn deepseek_expert_mxfp4_to_gguf(
    packed: &[u8],
    packed_shape: &[usize],
    scales: &[u8],
) -> Result<DeepseekNvFp4Weight> {
    if packed_shape.len() != 2 {
        return Err(fmt_err(format!(
            "mxfp4: ekspert ma kształt {packed_shape:?}, oczekiwano dwóch wymiarów"
        )));
    }
    let rows = packed_shape[0];
    let cols = packed_shape[1] * 2;
    if cols % NV_BLOCK_VALUES != 0 {
        return Err(fmt_err(format!(
            "mxfp4: {cols} kolumn nie dzieli się przez {NV_BLOCK_VALUES}"
        )));
    }
    let scales_per_row = cols / MX_GROUP;
    if packed.len() != rows * cols / 2 {
        return Err(fmt_err(format!(
            "mxfp4: pakiet ma {} bajtów, oczekiwano {}",
            packed.len(),
            rows * cols / 2
        )));
    }
    if scales.len() != rows * scales_per_row {
        return Err(fmt_err(format!(
            "mxfp4: skale mają {} bajtów, oczekiwano {}",
            scales.len(),
            rows * scales_per_row
        )));
    }

    // Wspólne przesunięcie wykładników: takie, żeby CAŁY tensor zmieścił się w
    // polu wykładnika UE4M3 (1..14 przy mantysie zero). Reszta idzie do skali
    // globalnej.
    let lo = *scales.iter().min().expect("niepusty tensor skal") as i32;
    let hi = *scales.iter().max().expect("niepusty tensor skal") as i32;
    if hi - lo > 13 {
        return Err(fmt_err(format!(
            "mxfp4: rozrzut wykładników {} przekracza okno UE4M3; ten tensor \
             wymaga skali na wiersz, nie na tensor",
            hi - lo
        )));
    }
    // `lo` ma wylądować na najniższym legalnym wykładniku (1).
    let shift = lo - 121;
    let global = 2f32.powi(shift);
    if !global.is_finite() || global <= 0.0 {
        return Err(fmt_err(format!(
            "mxfp4: skala globalna 2^{shift} nie jest dodatnią liczbą skończoną"
        )));
    }

    let blocks_per_row = cols / NV_BLOCK_VALUES;
    let mut out = vec![0u8; rows * blocks_per_row * NV_BLOCK_BYTES];
    for row in 0..rows {
        let packed_row = &packed[row * cols / 2..(row + 1) * cols / 2];
        let scale_row = &scales[row * scales_per_row..(row + 1) * scales_per_row];
        for block in 0..blocks_per_row {
            let dst = &mut out[(row * blocks_per_row + block) * NV_BLOCK_BYTES..]
                [..NV_BLOCK_BYTES];
            for sub in 0..4 {
                let first = block * NV_BLOCK_VALUES + sub * NV_GROUP;
                // Dwa sąsiednie pola NVFP4 dzielą jedną skalę MXFP4.
                let e = scale_row[first / MX_GROUP];
                let exponent = ue4m3_exponent(e, shift);
                if !(1..=14).contains(&exponent) {
                    return Err(fmt_err(format!(
                        "mxfp4: wykładnik {exponent} poza polem UE4M3 mimo przesunięcia"
                    )));
                }
                dst[sub] = (exponent as u8) << 3;
                // Układ GGUF: w polu 16 elementów bajt `i` niesie element `i` w
                // młodszej półbajtówce i `i + 8` w starszej.
                for i in 0..8 {
                    let low = nibble(packed_row, first + i);
                    let high = nibble(packed_row, first + i + 8);
                    dst[4 + sub * 8 + i] = low | (high << 4);
                }
            }
        }
    }

    Ok(DeepseekNvFp4Weight {
        blocks: out,
        output_scale: global,
        rows,
        cols,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvfp4::e2m1_to_f32;

    /// Przeliczenie MXFP4 na NVFP4 musi być DOKŁADNE, nie przybliżone: te same
    /// wartości, tylko inaczej rozłożone. Porównanie idzie przez dekwantyzację
    /// wyniku i wymaga zgodności co do bitu z odczytem układu źródłowego.
    #[test]
    fn conversion_is_exact() {
        let (rows, cols) = (3usize, 128usize);
        let packed: Vec<u8> = (0..rows * cols / 2)
            .map(|i| ((i * 61 + 13) % 256) as u8)
            .collect();
        // Wykładniki w wąskim oknie, tak jak w prawdziwych wagach (rozrzut 2..9).
        let scales: Vec<u8> = (0..rows * cols / MX_GROUP)
            .map(|i| (119 + (i % 7)) as u8)
            .collect();

        let out = deepseek_expert_mxfp4_to_gguf(&packed, &[rows, cols / 2], &scales).unwrap();
        assert_eq!(out.blocks.len(), rows * (cols / 64) * 36);

        let got = crate::dequant::dequantize_to_f32(
            forge_types::DType::U8,
            forge_types::QuantKind::NVFP4Gguf,
            &out.blocks,
            rows * cols,
        )
        .unwrap();

        for row in 0..rows {
            for col in 0..cols {
                let byte = packed[row * cols / 2 + col / 2];
                let code = if col % 2 == 0 { byte & 0x0F } else { byte >> 4 };
                let e = scales[row * (cols / MX_GROUP) + col / MX_GROUP] as i32;
                let want = e2m1_to_f32(code) * 2f32.powi(e - 127);
                let have = got[row * cols + col] * out.output_scale;
                assert_eq!(have, want, "wiersz {row} kolumna {col}");
            }
        }
    }

    /// Tensor rozciągnięty poza okno UE4M3 musi być błędem. Cicha utrata
    /// wykładnika zmieniłaby wagi o potęgi dwójki i nie zostawiła śladu.
    #[test]
    fn spread_beyond_window_is_an_error() {
        let (rows, cols) = (1usize, 64usize);
        let packed = vec![0u8; rows * cols / 2];
        let mut scales = vec![120u8; rows * cols / MX_GROUP];
        scales[1] = 150;
        let Err(err) = deepseek_expert_mxfp4_to_gguf(&packed, &[rows, cols / 2], &scales) else {
            panic!("rozrzut poza oknem UE4M3 musi być błędem");
        };
        assert!(
            format!("{err}").contains("rozrzut"),
            "spodziewano się błędu o rozrzucie, było: {err}"
        );
    }
}
