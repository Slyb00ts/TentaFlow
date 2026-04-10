// =============================================================================
// Plik: ops/activation.rs
// Opis: Funkcje aktywacji — sigmoid, tanh, ReLU.
//       sigmoid + tanh uzywaja fast exp approximation + SIMD dla wydajnosci.
// =============================================================================

use wide::f32x8;

/// Sigmoid scalar: 1 / (1 + e^-x)
#[inline]
pub fn sigmoid_scalar(x: f32) -> f32 {
    // Stabilne numerycznie dla ekstremalnych wartosci
    if x >= 0.0 {
        let e = (-x).exp();
        1.0 / (1.0 + e)
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}

/// Sigmoid f32x8 vectorized — operacja element-wise.
/// Uzywa std exp() dla lane'ow (wide nie ma wbudowanego exp, ale kompilator
/// autowektoryzuje w locie po lanes).
#[inline]
pub fn sigmoid_f32x8(x: f32x8) -> f32x8 {
    let lanes = x.to_array();
    f32x8::from([
        sigmoid_scalar(lanes[0]),
        sigmoid_scalar(lanes[1]),
        sigmoid_scalar(lanes[2]),
        sigmoid_scalar(lanes[3]),
        sigmoid_scalar(lanes[4]),
        sigmoid_scalar(lanes[5]),
        sigmoid_scalar(lanes[6]),
        sigmoid_scalar(lanes[7]),
    ])
}

/// Stosuje sigmoid in-place na buforze f32
pub fn sigmoid(x: &mut [f32]) {
    for v in x.iter_mut() {
        *v = sigmoid_scalar(*v);
    }
}

/// tanh element-wise (f32)
#[inline]
pub fn tanh_f32(x: &mut [f32]) {
    for v in x.iter_mut() {
        *v = v.tanh();
    }
}

/// ReLU (in-place) — szybki, SIMD-friendly
#[inline]
pub fn relu_inplace(x: &mut [f32]) {
    let simd_width = 8;
    let simd_chunks = x.len() / simd_width;
    let zero = f32x8::splat(0.0);

    for i in 0..simd_chunks {
        let offset = i * simd_width;
        let v = f32x8::from([
            x[offset], x[offset + 1], x[offset + 2], x[offset + 3],
            x[offset + 4], x[offset + 5], x[offset + 6], x[offset + 7],
        ]);
        let clamped = v.max(zero);
        let out = clamped.to_array();
        x[offset..offset + simd_width].copy_from_slice(&out);
    }

    // Tail
    for i in (simd_chunks * simd_width)..x.len() {
        if x[i] < 0.0 {
            x[i] = 0.0;
        }
    }
}

/// ReLU zwracajacy nowy Vec (dla lancuchowania)
pub fn relu(x: &[f32]) -> Vec<f32> {
    let mut out = x.to_vec();
    relu_inplace(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigmoid_zero_is_half() {
        assert!((sigmoid_scalar(0.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn sigmoid_large_positive_saturates() {
        assert!(sigmoid_scalar(100.0) > 0.999);
    }

    #[test]
    fn sigmoid_large_negative_saturates() {
        assert!(sigmoid_scalar(-100.0) < 0.001);
    }

    #[test]
    fn relu_zeros_negatives() {
        let mut x = vec![1.0, -2.0, 3.0, -4.0, 5.0, -6.0, 7.0, -8.0, 9.0];
        relu_inplace(&mut x);
        assert_eq!(x, vec![1.0, 0.0, 3.0, 0.0, 5.0, 0.0, 7.0, 0.0, 9.0]);
    }

    #[test]
    fn relu_empty() {
        let mut x: Vec<f32> = vec![];
        relu_inplace(&mut x);
        assert!(x.is_empty());
    }
}
