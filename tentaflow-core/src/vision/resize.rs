// =============================================================================
// Plik: vision/resize.rs
// Opis: Najszybszy separowalny resizer obrazow RGB24 (downscale-first). Wagi
//       liczone raz w stalym przecinku (Q8), gorace petle branchless z AVX2 /
//       NEON i poprawnym fallbackiem skalarnym uzywanym do walidacji.
// =============================================================================

use std::fmt;

/// Bledy walidacji wejscia resizera. Nigdy nie panikujemy na zlych wymiarach —
/// caller (host function) mapuje to na kod ABI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResizeError {
    /// Ktorys z wymiarow byl zerowy.
    ZeroDimension,
    /// `src.len()` nie odpowiada `src_w * src_h * 3`.
    BufferSizeMismatch { expected: usize, got: usize },
    /// Mnozenie wymiarow przekroczylo `usize` (ochrona przed overflow).
    DimensionOverflow,
}

impl fmt::Display for ResizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResizeError::ZeroDimension => write!(f, "resize: wymiar zerowy"),
            ResizeError::BufferSizeMismatch { expected, got } => {
                write!(
                    f,
                    "resize: zly rozmiar bufora (oczekiwano {expected}, jest {got})"
                )
            }
            ResizeError::DimensionOverflow => write!(f, "resize: overflow wymiarow"),
        }
    }
}

impl std::error::Error for ResizeError {}

/// Liczba bitow ulamkowych wag w stalym przecinku. Q8 => waga 0..=256, suma wag
/// na piksel docelowy == 256 dokladnie, wiec normalizacja to przesuniecie >>8.
const WEIGHT_SHIFT: u32 = 8;
const WEIGHT_ONE: i32 = 1 << WEIGHT_SHIFT;

/// Prekomputowany plan dla jednej osi: dla kazdego piksela docelowego dwa
/// indeksy zrodlowe (left/right) i waga prawego sasiada w Q8. Waga lewego to
/// `WEIGHT_ONE - w_right`. To klasyczny bilinear, ale z calkowicie branchless
/// inner-loopem (klamp indeksow policzony tutaj, raz).
struct AxisPlan {
    /// Indeks lewego sasiada (px zrodlowy) dla kazdego piksela docelowego.
    left: Vec<u32>,
    /// Indeks prawego sasiada (== left lub left+1, sklampowany do src-1).
    right: Vec<u32>,
    /// Waga prawego sasiada w Q8 (0..=256).
    w_right: Vec<i32>,
}

impl AxisPlan {
    /// Buduje plan bilinear dla mapowania `src_len -> dst_len`. `half-pixel`
    /// center alignment: `src_x = (dst_x + 0.5) * scale - 0.5`. Indeksy
    /// klampowane do `[0, src_len-1]` (brak galezi w inner-loop).
    fn bilinear(src_len: u32, dst_len: u32) -> AxisPlan {
        let mut left = Vec::with_capacity(dst_len as usize);
        let mut right = Vec::with_capacity(dst_len as usize);
        let mut w_right = Vec::with_capacity(dst_len as usize);

        let scale = src_len as f64 / dst_len as f64;
        let max_idx = src_len - 1;

        for d in 0..dst_len {
            // Pozycja zrodlowa srodka piksela docelowego (half-pixel).
            let src_pos = (d as f64 + 0.5) * scale - 0.5;
            let src_pos = if src_pos < 0.0 { 0.0 } else { src_pos };

            let l = src_pos.floor() as i64;
            let frac = src_pos - l as f64;

            // Klamp do prawidlowego zakresu — robione raz, tu, nie w petli px.
            let l_clamped = l.clamp(0, max_idx as i64) as u32;
            let r_clamped = (l + 1).clamp(0, max_idx as i64) as u32;

            // Waga prawego sasiada w Q8 z zaokragleniem do najblizszego.
            let wr = (frac * WEIGHT_ONE as f64).round() as i32;
            let wr = wr.clamp(0, WEIGHT_ONE);

            left.push(l_clamped);
            right.push(r_clamped);
            w_right.push(wr);
        }

        AxisPlan {
            left,
            right,
            w_right,
        }
    }
}

/// Resize obrazu RGB24 (row-major, 3 bajty/piksel) z `src_w x src_h` do
/// `dst_w x dst_h`. Zwraca nowy bufor `dst_w * dst_h * 3`.
///
/// Algorytm: separowalny bilinear w dwoch przebiegach (najpierw poziomy do
/// bufora posredniego `dst_w x src_h`, potem pionowy do wyniku). Wagi w stalym
/// przecinku Q8, prekomputowane raz na os. Wybor sciezki SIMD jest runtime'owy.
pub fn resize_rgb(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> Result<Vec<u8>, ResizeError> {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Err(ResizeError::ZeroDimension);
    }

    let expected = (src_w as usize)
        .checked_mul(src_h as usize)
        .and_then(|v| v.checked_mul(3))
        .ok_or(ResizeError::DimensionOverflow)?;
    if src.len() != expected {
        return Err(ResizeError::BufferSizeMismatch {
            expected,
            got: src.len(),
        });
    }

    // Ochrona przed overflow rozmiaru wyniku.
    let dst_bytes = (dst_w as usize)
        .checked_mul(dst_h as usize)
        .and_then(|v| v.checked_mul(3))
        .ok_or(ResizeError::DimensionOverflow)?;

    // Szybka sciezka: identycznosc wymiarow => czysta kopia.
    if src_w == dst_w && src_h == dst_h {
        return Ok(src.to_vec());
    }

    let x_plan = AxisPlan::bilinear(src_w, dst_w);
    let y_plan = AxisPlan::bilinear(src_h, dst_h);

    // Przebieg poziomy: src (src_w x src_h) -> tmp (dst_w x src_h).
    let mut tmp = vec![0u8; (dst_w as usize) * (src_h as usize) * 3];
    horizontal_pass(src, src_w, src_h, dst_w, &x_plan, &mut tmp);

    // Przebieg pionowy: tmp (dst_w x src_h) -> out (dst_w x dst_h).
    let mut out = vec![0u8; dst_bytes];
    vertical_pass(&tmp, dst_w, dst_h, &y_plan, &mut out);

    Ok(out)
}

/// Przebieg poziomy: dla kazdego wiersza interpoluje kolumny wg `x_plan`.
/// Czyta 2 piksele zrodlowe (RGB) na piksel docelowy, miesza w Q8.
fn horizontal_pass(
    src: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    x_plan: &AxisPlan,
    tmp: &mut [u8],
) {
    let src_row_stride = src_w as usize * 3;
    let dst_row_stride = dst_w as usize * 3;

    for y in 0..src_h as usize {
        let src_row = &src[y * src_row_stride..y * src_row_stride + src_row_stride];
        let dst_row = &mut tmp[y * dst_row_stride..y * dst_row_stride + dst_row_stride];
        horizontal_row_scalar(src_row, dst_w, x_plan, dst_row);
    }
}

/// Skalarny, branchless miks poziomy jednego wiersza. To zarazem referencyjna
/// implementacja do walidacji SIMD. Indeksy/wagi sa prekomputowane => brak
/// `if` w petli, brak bounds-check paniki (slicing po stalej dlugosci 3).
#[inline]
fn horizontal_row_scalar(src_row: &[u8], dst_w: u32, x_plan: &AxisPlan, dst_row: &mut [u8]) {
    for d in 0..dst_w as usize {
        let li = x_plan.left[d] as usize * 3;
        let ri = x_plan.right[d] as usize * 3;
        let wr = x_plan.w_right[d];
        let wl = WEIGHT_ONE - wr;

        // Rozwiniete 3 kanaly. `+ (WEIGHT_ONE/2)` to zaokraglenie do najblizszego.
        let r =
            (src_row[li] as i32 * wl + src_row[ri] as i32 * wr + (WEIGHT_ONE / 2)) >> WEIGHT_SHIFT;
        let g = (src_row[li + 1] as i32 * wl + src_row[ri + 1] as i32 * wr + (WEIGHT_ONE / 2))
            >> WEIGHT_SHIFT;
        let b = (src_row[li + 2] as i32 * wl + src_row[ri + 2] as i32 * wr + (WEIGHT_ONE / 2))
            >> WEIGHT_SHIFT;

        let o = d * 3;
        dst_row[o] = r as u8;
        dst_row[o + 1] = g as u8;
        dst_row[o + 2] = b as u8;
    }
}

/// Przebieg pionowy: dla kazdego wiersza docelowego miesza dwa wiersze
/// zrodlowe (tmp) wg `y_plan`. Tu wlasnie miesza sie CALE wiersze naraz —
/// idealne dla SIMD (ten sam skalar wagi na 32 bajty).
fn vertical_pass(tmp: &[u8], dst_w: u32, dst_h: u32, y_plan: &AxisPlan, out: &mut [u8]) {
    let row_stride = dst_w as usize * 3;

    for d in 0..dst_h as usize {
        let top = y_plan.left[d] as usize;
        let bot = y_plan.right[d] as usize;
        let wb = y_plan.w_right[d];
        let wt = WEIGHT_ONE - wb;

        let top_row = &tmp[top * row_stride..top * row_stride + row_stride];
        let bot_row = &tmp[bot * row_stride..bot * row_stride + row_stride];
        let dst_row = &mut out[d * row_stride..d * row_stride + row_stride];

        blend_rows(top_row, bot_row, wt, wb, dst_row);
    }
}

/// Miesza dwa wiersze bajtow: `dst = (top*wt + bot*wb + round) >> 8`. Dispatch
/// runtime do AVX2 / NEON; fallback skalarny zawsze poprawny.
#[inline]
fn blend_rows(top: &[u8], bot: &[u8], wt: i32, wb: i32, dst: &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            // SAFETY: feature wykryty runtime, slice dlugosci rowne.
            unsafe { blend_rows_avx2(top, bot, wt, wb, dst) };
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            // SAFETY: NEON jest baseline na aarch64, slice dlugosci rowne.
            unsafe { blend_rows_neon(top, bot, wt, wb, dst) };
            return;
        }
    }
    blend_rows_scalar(top, bot, wt, wb, dst);
}

/// Skalarny, branchless miks dwoch wierszy. Referencja do walidacji SIMD.
#[inline]
fn blend_rows_scalar(top: &[u8], bot: &[u8], wt: i32, wb: i32, dst: &mut [u8]) {
    let n = dst.len();
    for i in 0..n {
        let v = (top[i] as i32 * wt + bot[i] as i32 * wb + (WEIGHT_ONE / 2)) >> WEIGHT_SHIFT;
        dst[i] = v as u8;
    }
}

/// AVX2 miks dwoch wierszy. Przetwarza 32 bajty/iteracje przez `_mm256_maddubs_epi16`:
/// pakujemy wagi jako [wt, wb] na para-bajt, dane jako interleave [top, bot] —
/// `maddubs` robi `top*wt + bot*wb` w i16 jednym strzalem. Reszta < 32 bajty
/// idzie skalarnie (branchless).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn blend_rows_avx2(top: &[u8], bot: &[u8], wt: i32, wb: i32, dst: &mut [u8]) {
    use std::arch::x86_64::*;

    let n = dst.len();

    // wagi Q8 (0..=256) nie mieszcza sie w i8, wiec mnozymy na rozpakowanych u16 (mullo_epi16), nie maddubs
    let round = _mm256_set1_epi16((WEIGHT_ONE / 2) as i16);
    let wt16 = _mm256_set1_epi16(wt as i16);
    let wb16 = _mm256_set1_epi16(wb as i16);

    let mut i = 0usize;
    while i + 32 <= n {
        let t = _mm256_loadu_si256(top.as_ptr().add(i) as *const __m256i);
        let b = _mm256_loadu_si256(bot.as_ptr().add(i) as *const __m256i);

        // Rozpakuj u8 -> u16 (dwie polowy 256-bit lane'a).
        let zero = _mm256_setzero_si256();
        let t_lo = _mm256_unpacklo_epi8(t, zero);
        let t_hi = _mm256_unpackhi_epi8(t, zero);
        let b_lo = _mm256_unpacklo_epi8(b, zero);
        let b_hi = _mm256_unpackhi_epi8(b, zero);

        // (top*wt + bot*wb + round) >> 8 w i16.
        let lo = _mm256_srli_epi16(
            _mm256_add_epi16(
                _mm256_add_epi16(
                    _mm256_mullo_epi16(t_lo, wt16),
                    _mm256_mullo_epi16(b_lo, wb16),
                ),
                round,
            ),
            WEIGHT_SHIFT as i32,
        );
        let hi = _mm256_srli_epi16(
            _mm256_add_epi16(
                _mm256_add_epi16(
                    _mm256_mullo_epi16(t_hi, wt16),
                    _mm256_mullo_epi16(b_hi, wb16),
                ),
                round,
            ),
            WEIGHT_SHIFT as i32,
        );

        // Spakuj i16 -> u8 (saturujaco; wartosci sa 0..=255 wiec sat. bez strat).
        // przeplot lane'ow z packus znosi sie z przeplotem unpacklo/unpackhi — NIE usuwac zadnego bez dodania _mm256_permute4x64_epi64; test simd_matches_scalar_* tego pilnuje
        let packed = _mm256_packus_epi16(lo, hi);
        _mm256_storeu_si256(dst.as_mut_ptr().add(i) as *mut __m256i, packed);
        i += 32;
    }

    // Ogon < 32 bajty — branchless skalar.
    while i < n {
        let v = (*top.get_unchecked(i) as i32 * wt
            + *bot.get_unchecked(i) as i32 * wb
            + (WEIGHT_ONE / 2))
            >> WEIGHT_SHIFT;
        *dst.get_unchecked_mut(i) = v as u8;
        i += 1;
    }
}

/// NEON miks dwoch wierszy. Przetwarza 16 bajtow/iteracje: rozszerza u8->u16,
/// mnozy przez skalary wag (`vmull`), dodaje round, przesuwa >>8, pakuje.
#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn blend_rows_neon(top: &[u8], bot: &[u8], wt: i32, wb: i32, dst: &mut [u8]) {
    use std::arch::aarch64::*;

    let n = dst.len();
    let wt16 = wt as u16;
    let wb16 = wb as u16;
    let round = vdupq_n_u16((WEIGHT_ONE / 2) as u16);

    let mut i = 0usize;
    while i + 16 <= n {
        let t = vld1q_u8(top.as_ptr().add(i));
        let b = vld1q_u8(bot.as_ptr().add(i));

        let t_lo = vmovl_u8(vget_low_u8(t));
        let t_hi = vmovl_u8(vget_high_u8(t));
        let b_lo = vmovl_u8(vget_low_u8(b));
        let b_hi = vmovl_u8(vget_high_u8(b));

        let acc_lo = vaddq_u16(vmlaq_n_u16(vmulq_n_u16(t_lo, wt16), b_lo, wb16), round);
        let acc_hi = vaddq_u16(vmlaq_n_u16(vmulq_n_u16(t_hi, wt16), b_hi, wb16), round);
        let lo = vshrq_n_u16::<8>(acc_lo);
        let hi = vshrq_n_u16::<8>(acc_hi);

        let packed = vcombine_u8(vqmovn_u16(lo), vqmovn_u16(hi));
        vst1q_u8(dst.as_mut_ptr().add(i), packed);
        i += 16;
    }

    while i < n {
        let v = (*top.get_unchecked(i) as i32 * wt
            + *bot.get_unchecked(i) as i32 * wb
            + (WEIGHT_ONE / 2))
            >> WEIGHT_SHIFT;
        *dst.get_unchecked_mut(i) = v as u8;
        i += 1;
    }
}

/// Cienki adapter na typ `RgbImage` z crate `image`. Rdzen (`resize_rgb`) nie
/// zalezy od `image` — tutaj jedynie wyciagamy surowe bajty RGB24 i pakujemy
/// wynik z powrotem do `RgbImage`. Dziala tak samo dla downscale i upscale.
pub fn resize_rgb_image(
    img: &image::RgbImage,
    dst_w: u32,
    dst_h: u32,
) -> Result<image::RgbImage, ResizeError> {
    let (sw, sh) = (img.width(), img.height());
    let out = resize_rgb(img.as_raw(), sw, sh, dst_w, dst_h)?;
    // `from_raw` zwraca None tylko gdy dlugosc bufora != dst_w*dst_h*3, co
    // `resize_rgb` gwarantuje — wiec to nigdy nie panikuje, ale mapujemy na blad.
    image::RgbImage::from_raw(dst_w, dst_h, out).ok_or(ResizeError::DimensionOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pseudolosowy generator (xorshift) — bez zaleznosci od rand.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            (x >> 32) as u32
        }
        fn byte(&mut self) -> u8 {
            (self.next() & 0xff) as u8
        }
    }

    fn random_image(w: u32, h: u32, seed: u64) -> Vec<u8> {
        let mut rng = Rng(seed | 1);
        (0..(w as usize * h as usize * 3))
            .map(|_| rng.byte())
            .collect()
    }

    /// Skalarna referencja: oba przebiegi wymuszone skalarnie.
    fn resize_scalar_ref(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
        let xp = AxisPlan::bilinear(sw, dw);
        let yp = AxisPlan::bilinear(sh, dh);
        let mut tmp = vec![0u8; dw as usize * sh as usize * 3];
        let srs = sw as usize * 3;
        let drs = dw as usize * 3;
        for y in 0..sh as usize {
            horizontal_row_scalar(
                &src[y * srs..y * srs + srs],
                dw,
                &xp,
                &mut tmp[y * drs..y * drs + drs],
            );
        }
        let mut out = vec![0u8; dw as usize * dh as usize * 3];
        for d in 0..dh as usize {
            let top = yp.left[d] as usize;
            let bot = yp.right[d] as usize;
            let wb = yp.w_right[d];
            let wt = WEIGHT_ONE - wb;
            let (a, b) = (
                &tmp[top * drs..top * drs + drs],
                &tmp[bot * drs..bot * drs + drs],
            );
            blend_rows_scalar(a, b, wt, wb, &mut out[d * drs..d * drs + drs]);
        }
        out
    }

    #[test]
    fn simd_matches_scalar_various_sizes() {
        let cases = [
            (640u32, 480u32, 560u32, 560u32),
            (5152, 3864, 560, 560),
            (5152, 3864, 1280, 720),
            (333, 211, 100, 99),
            (17, 5, 9, 13),
            (2, 2, 1, 1),
        ];
        for (i, &(sw, sh, dw, dh)) in cases.iter().enumerate() {
            let src = random_image(sw, sh, 0xDEAD_0000 + i as u64);
            let fast = resize_rgb(&src, sw, sh, dw, dh).unwrap();
            let reference = resize_scalar_ref(&src, sw, sh, dw, dh);
            assert_eq!(fast.len(), reference.len(), "len case {i}");
            // SIMD i skalar uzywaja tej samej matematyki Q8 — musza byc identyczne.
            for (k, (a, b)) in fast.iter().zip(reference.iter()).enumerate() {
                assert_eq!(a, b, "px {k} case {i} ({sw}x{sh}->{dw}x{dh})");
            }
        }
    }

    #[test]
    fn simd_matches_scalar_upscale() {
        // Upscale: wagi bilinear dzialaja w obie strony. SIMD i skalar musza byc
        // identyczne tak samo jak przy downscale.
        let cases = [
            (100u32, 100u32, 400u32, 400u32),
            (32, 24, 224, 224),
            (7, 5, 64, 48),
        ];
        for (i, &(sw, sh, dw, dh)) in cases.iter().enumerate() {
            let src = random_image(sw, sh, 0xBEEF_0000 + i as u64);
            let fast = resize_rgb(&src, sw, sh, dw, dh).unwrap();
            let reference = resize_scalar_ref(&src, sw, sh, dw, dh);
            assert_eq!(fast.len(), reference.len(), "len upscale case {i}");
            for (k, (a, b)) in fast.iter().zip(reference.iter()).enumerate() {
                assert_eq!(a, b, "px {k} upscale case {i} ({sw}x{sh}->{dw}x{dh})");
            }
        }
    }

    #[test]
    fn rgb_image_helper_roundtrip() {
        let src = random_image(100, 100, 0x1234);
        let img = image::RgbImage::from_raw(100, 100, src.clone()).unwrap();
        let up = resize_rgb_image(&img, 400, 400).unwrap();
        assert_eq!(up.width(), 400);
        assert_eq!(up.height(), 400);
        let raw = resize_rgb(&src, 100, 100, 400, 400).unwrap();
        assert_eq!(up.as_raw(), &raw);
    }

    #[test]
    fn identity_is_copy() {
        let src = random_image(64, 48, 7);
        let out = resize_rgb(&src, 64, 48, 64, 48).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn downscale_to_1x1() {
        let src = random_image(100, 100, 42);
        let out = resize_rgb(&src, 100, 100, 1, 1).unwrap();
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn odd_dimensions() {
        let src = random_image(101, 57, 99);
        let out = resize_rgb(&src, 101, 57, 33, 41).unwrap();
        assert_eq!(out.len(), 33 * 41 * 3);
    }

    #[test]
    fn rejects_bad_input() {
        let src = vec![0u8; 10];
        assert_eq!(
            resize_rgb(&src, 0, 5, 2, 2),
            Err(ResizeError::ZeroDimension)
        );
        assert!(matches!(
            resize_rgb(&src, 4, 4, 2, 2),
            Err(ResizeError::BufferSizeMismatch { .. })
        ));
    }

    /// Solidny obraz (jednolity kolor) musi pozostac tym samym kolorem po
    /// resize — sanity check matematyki wag (suma wag == 1.0).
    #[test]
    fn solid_color_preserved() {
        let color = [200u8, 100, 50];
        let mut src = vec![0u8; 300 * 200 * 3];
        for px in src.chunks_exact_mut(3) {
            px.copy_from_slice(&color);
        }
        let out = resize_rgb(&src, 300, 200, 77, 55).unwrap();
        for px in out.chunks_exact(3) {
            assert!((px[0] as i32 - 200).abs() <= 1);
            assert!((px[1] as i32 - 100).abs() <= 1);
            assert!((px[2] as i32 - 50).abs() <= 1);
        }
    }

    /// Porownanie jakosci z image::imageops (Triangle). Inny filtr, wiec nie
    /// identyczne — ale sredni blad per kanal musi byc maly (downscale).
    #[test]
    fn close_to_image_crate_triangle() {
        use image::{imageops::FilterType, RgbImage};
        let (sw, sh, dw, dh) = (640u32, 480u32, 160u32, 120u32);
        // Gladki gradient (jak realne zdjecie) — nie czysty szum. Na szumie
        // wide-kernel Triangle i 2-tap bilinear rozjezdzaja sie bardzo; na
        // gladkich danych (low-frequency) musza byc bliskie.
        let mut src = vec![0u8; sw as usize * sh as usize * 3];
        for y in 0..sh as usize {
            for x in 0..sw as usize {
                let o = (y * sw as usize + x) * 3;
                src[o] = ((x * 255) / sw as usize) as u8;
                src[o + 1] = ((y * 255) / sh as usize) as u8;
                src[o + 2] = (((x + y) * 255) / (sw as usize + sh as usize)) as u8;
            }
        }
        let ours = resize_rgb(&src, sw, sh, dw, dh).unwrap();

        let img = RgbImage::from_raw(sw, sh, src.clone()).unwrap();
        let theirs = image::imageops::resize(&img, dw, dh, FilterType::Triangle);
        let theirs = theirs.into_raw();

        let mut sum_abs = 0u64;
        for (a, b) in ours.iter().zip(theirs.iter()) {
            sum_abs += (*a as i32 - *b as i32).unsigned_abs() as u64;
        }
        let mean = sum_abs as f64 / ours.len() as f64;
        // Na gladkim gradiencie oba filtry musza byc bardzo bliskie.
        assert!(mean < 3.0, "sredni blad per kanal zbyt duzy: {mean}");
    }
}
