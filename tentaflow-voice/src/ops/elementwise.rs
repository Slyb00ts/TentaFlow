// =============================================================================
// Plik: ops/elementwise.rs
// Opis: Element-wise operacje (Add/Sub/Mul/Div/Pow/Sqrt) — wszystkie z SIMD.
// =============================================================================

use wide::f32x8;

/// out[i] = a[i] + b[i] — z SIMD
pub fn add(a: &[f32], b: &[f32], out: &mut [f32]) {
    debug_assert_eq!(a.len(), b.len());
    debug_assert_eq!(a.len(), out.len());
    let n = a.len();
    let simd_chunks = n / 8;
    for i in 0..simd_chunks {
        let off = i * 8;
        let va = f32x8::from([a[off], a[off+1], a[off+2], a[off+3], a[off+4], a[off+5], a[off+6], a[off+7]]);
        let vb = f32x8::from([b[off], b[off+1], b[off+2], b[off+3], b[off+4], b[off+5], b[off+6], b[off+7]]);
        let r = (va + vb).to_array();
        out[off..off+8].copy_from_slice(&r);
    }
    for i in (simd_chunks * 8)..n {
        out[i] = a[i] + b[i];
    }
}

/// In-place: a[i] += b[i]
pub fn add_inplace(a: &mut [f32], b: &[f32]) {
    debug_assert_eq!(a.len(), b.len());
    let n = a.len();
    let simd_chunks = n / 8;
    for i in 0..simd_chunks {
        let off = i * 8;
        let va = f32x8::from([a[off], a[off+1], a[off+2], a[off+3], a[off+4], a[off+5], a[off+6], a[off+7]]);
        let vb = f32x8::from([b[off], b[off+1], b[off+2], b[off+3], b[off+4], b[off+5], b[off+6], b[off+7]]);
        let r = (va + vb).to_array();
        a[off..off+8].copy_from_slice(&r);
    }
    for i in (simd_chunks * 8)..n {
        a[i] += b[i];
    }
}

/// out[i] = a[i] * b[i] — z SIMD
pub fn mul(a: &[f32], b: &[f32], out: &mut [f32]) {
    debug_assert_eq!(a.len(), b.len());
    debug_assert_eq!(a.len(), out.len());
    let n = a.len();
    let simd_chunks = n / 8;
    for i in 0..simd_chunks {
        let off = i * 8;
        let va = f32x8::from([a[off], a[off+1], a[off+2], a[off+3], a[off+4], a[off+5], a[off+6], a[off+7]]);
        let vb = f32x8::from([b[off], b[off+1], b[off+2], b[off+3], b[off+4], b[off+5], b[off+6], b[off+7]]);
        let r = (va * vb).to_array();
        out[off..off+8].copy_from_slice(&r);
    }
    for i in (simd_chunks * 8)..n {
        out[i] = a[i] * b[i];
    }
}

/// In-place: a[i] *= b[i]
pub fn mul_inplace(a: &mut [f32], b: &[f32]) {
    debug_assert_eq!(a.len(), b.len());
    let n = a.len();
    let simd_chunks = n / 8;
    for i in 0..simd_chunks {
        let off = i * 8;
        let va = f32x8::from([a[off], a[off+1], a[off+2], a[off+3], a[off+4], a[off+5], a[off+6], a[off+7]]);
        let vb = f32x8::from([b[off], b[off+1], b[off+2], b[off+3], b[off+4], b[off+5], b[off+6], b[off+7]]);
        let r = (va * vb).to_array();
        a[off..off+8].copy_from_slice(&r);
    }
    for i in (simd_chunks * 8)..n {
        a[i] *= b[i];
    }
}

/// In-place: a[i] *= scalar
pub fn mul_scalar_inplace(a: &mut [f32], scalar: f32) {
    let n = a.len();
    let simd_chunks = n / 8;
    let s = f32x8::splat(scalar);
    for i in 0..simd_chunks {
        let off = i * 8;
        let va = f32x8::from([a[off], a[off+1], a[off+2], a[off+3], a[off+4], a[off+5], a[off+6], a[off+7]]);
        let r = (va * s).to_array();
        a[off..off+8].copy_from_slice(&r);
    }
    for i in (simd_chunks * 8)..n {
        a[i] *= scalar;
    }
}

/// out[i] = sqrt(a[i] + eps) — element-wise
pub fn sqrt_with_eps(a: &mut [f32], eps: f32) {
    for v in a.iter_mut() {
        *v = (*v + eps).sqrt();
    }
}

/// L2 norm in-place: a[i] = a[i] / ||a||
pub fn l2_normalize(a: &mut [f32]) {
    let norm: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        let inv = 1.0 / norm;
        mul_scalar_inplace(a, inv);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_basic() {
        let a = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        let b = vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0];
        let mut out = vec![0.0; 10];
        add(&a, &b, &mut out);
        assert_eq!(out, vec![11.0, 22.0, 33.0, 44.0, 55.0, 66.0, 77.0, 88.0, 99.0, 110.0]);
    }

    #[test]
    fn mul_basic() {
        let a = vec![2.0; 17];
        let b = vec![3.0; 17];
        let mut out = vec![0.0; 17];
        mul(&a, &b, &mut out);
        for v in &out {
            assert_eq!(*v, 6.0);
        }
    }

    #[test]
    fn l2_normalize_unit_vec() {
        let mut v = vec![3.0, 4.0]; // norm = 5
        l2_normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
        let new_norm: f32 = v.iter().map(|x| x*x).sum::<f32>().sqrt();
        assert!((new_norm - 1.0).abs() < 1e-6);
    }
}
