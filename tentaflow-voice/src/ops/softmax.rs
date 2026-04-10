// =============================================================================
// Plik: ops/softmax.rs
// Opis: Softmax wzdluz osi czasowej dla tensora [C, T] (per channel).
//       Numerically stable: subtract max before exp.
//       SIMD + rayon po kanalach.
// =============================================================================

use rayon::prelude::*;
use wide::f32x8;

/// Softmax po axis=last (time) per kanal.
/// Dla [C, T]: out[c, t] = exp(in[c, t] - max[c]) / sum_t exp(in[c, t'] - max[c])
///
/// Rayon po kanalach — exp jest ciezkie (transcendental) wiec parallelizm sie
/// oplaca nawet dla krotkich wierszy.
pub fn softmax_axis_last(data: &mut [f32], num_channels: usize, length: usize) {
    debug_assert_eq!(data.len(), num_channels * length);
    data.par_chunks_mut(length).take(num_channels).for_each(|row| {
        softmax_row(row, length);
    });
}

#[inline]
fn softmax_row(row: &mut [f32], length: usize) {
    // Krok 1: Max value (SIMD reduce) — 4-way unrolled
    let n32 = length - (length % 32);
    let n8 = length - (length % 8);
    let neg_inf = f32x8::splat(f32::NEG_INFINITY);
    let mut m0 = neg_inf;
    let mut m1 = neg_inf;
    let mut m2 = neg_inf;
    let mut m3 = neg_inf;
    let mut i = 0;
    while i < n32 {
        let a0: [f32; 8] = row[i..i + 8].try_into().unwrap();
        let a1: [f32; 8] = row[i + 8..i + 16].try_into().unwrap();
        let a2: [f32; 8] = row[i + 16..i + 24].try_into().unwrap();
        let a3: [f32; 8] = row[i + 24..i + 32].try_into().unwrap();
        m0 = m0.max(f32x8::from(a0));
        m1 = m1.max(f32x8::from(a1));
        m2 = m2.max(f32x8::from(a2));
        m3 = m3.max(f32x8::from(a3));
        i += 32;
    }
    while i < n8 {
        let a: [f32; 8] = row[i..i + 8].try_into().unwrap();
        m0 = m0.max(f32x8::from(a));
        i += 8;
    }
    let m = m0.max(m1).max(m2).max(m3);
    let lanes = m.to_array();
    let mut max_val = lanes[0];
    for &v in &lanes[1..] {
        if v > max_val {
            max_val = v;
        }
    }
    while i < length {
        if row[i] > max_val {
            max_val = row[i];
        }
        i += 1;
    }

    // Krok 2: exp(x - max) w miejscu + suma
    // (wide nie ma exp, wiec petla scalar — ale kompilator z target-cpu=native
    // potrafi to wektoryzowac przez libmvec gdy dostepne)
    let mut sum = 0.0_f32;
    for v in row.iter_mut() {
        *v = (*v - max_val).exp();
        sum += *v;
    }

    // Krok 3: Normalizacja (SIMD, reciprocal)
    if sum > 0.0 {
        let inv = 1.0 / sum;
        let inv_v = f32x8::splat(inv);
        let mut i = 0;
        while i < n32 {
            let a0: [f32; 8] = row[i..i + 8].try_into().unwrap();
            let a1: [f32; 8] = row[i + 8..i + 16].try_into().unwrap();
            let a2: [f32; 8] = row[i + 16..i + 24].try_into().unwrap();
            let a3: [f32; 8] = row[i + 24..i + 32].try_into().unwrap();
            row[i..i + 8].copy_from_slice(&(f32x8::from(a0) * inv_v).to_array());
            row[i + 8..i + 16].copy_from_slice(&(f32x8::from(a1) * inv_v).to_array());
            row[i + 16..i + 24].copy_from_slice(&(f32x8::from(a2) * inv_v).to_array());
            row[i + 24..i + 32].copy_from_slice(&(f32x8::from(a3) * inv_v).to_array());
            i += 32;
        }
        while i < n8 {
            let a: [f32; 8] = row[i..i + 8].try_into().unwrap();
            row[i..i + 8].copy_from_slice(&(f32x8::from(a) * inv_v).to_array());
            i += 8;
        }
        while i < length {
            row[i] *= inv;
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn softmax_uniform_input() {
        let mut data = vec![1.0, 1.0, 1.0, 1.0];
        softmax_axis_last(&mut data, 1, 4);
        for v in &data {
            assert!((*v - 0.25).abs() < 1e-6);
        }
    }

    #[test]
    fn softmax_sum_to_one() {
        let mut data = vec![1.0, 2.0, 3.0, 4.0];
        softmax_axis_last(&mut data, 1, 4);
        let s: f32 = data.iter().sum();
        assert!((s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn softmax_per_channel_independent() {
        let mut data = vec![
            1.0, 1.0, 1.0,
            5.0, 5.0, 5.0,
        ];
        softmax_axis_last(&mut data, 2, 3);
        for v in &data {
            assert!((*v - 1.0/3.0).abs() < 1e-6);
        }
    }
}
