// ===== File: grade.rs — comparing states, unitaries and count distributions =====
//
// These are the primitives behind kata grading and behind the
// simulation-versus-QPU comparison in the run view (plan 6.1, 13.6).

use std::collections::{BTreeMap, BTreeSet};

use num_complex::Complex64;

use crate::error::{invalid, Result};

/// |<a|b>|^2 for two normalised state vectors.
pub fn state_fidelity(a: &[Complex64], b: &[Complex64]) -> Result<f64> {
    if a.len() != b.len() {
        return Err(invalid("state vectors have different dimensions"));
    }
    let overlap: Complex64 = a.iter().zip(b).map(|(x, y)| x.conj() * y).sum();
    Ok(overlap.norm_sqr())
}

/// Equality of two state vectors up to a global phase.
///
/// The phase is fixed on the largest component of `a`, then every component is
/// compared; that is stricter than a fidelity threshold and it reports the
/// difference the student actually made.
pub fn states_equal(a: &[Complex64], b: &[Complex64], tolerance: f64) -> Result<bool> {
    if a.len() != b.len() {
        return Err(invalid("state vectors have different dimensions"));
    }
    Ok(equal_up_to_phase(a, b, tolerance))
}

/// Equality of two dense unitaries up to a global phase.
pub fn unitaries_equal(a: &[Complex64], b: &[Complex64], tolerance: f64) -> Result<bool> {
    if a.len() != b.len() {
        return Err(invalid("unitaries have different dimensions"));
    }
    let dim = (a.len() as f64).sqrt().round() as usize;
    if dim * dim != a.len() {
        return Err(invalid("a unitary must be a square matrix"));
    }
    Ok(equal_up_to_phase(a, b, tolerance))
}

fn equal_up_to_phase(a: &[Complex64], b: &[Complex64], tolerance: f64) -> bool {
    let pivot = a
        .iter()
        .enumerate()
        .max_by(|x, y| x.1.norm_sqr().total_cmp(&y.1.norm_sqr()))
        .map(|(i, _)| i);
    let pivot = match pivot {
        Some(i) if a[i].norm() > tolerance => i,
        // `a` is the zero vector: `b` has to be one too.
        _ => return b.iter().all(|z| z.norm() <= tolerance),
    };
    if b[pivot].norm() <= tolerance {
        return false;
    }
    let phase = b[pivot] / a[pivot];
    let normalised = phase / phase.norm();
    if (phase.norm() - 1.0).abs() > tolerance {
        return false;
    }
    a.iter()
        .zip(b)
        .all(|(x, y)| (x * normalised - y).norm() <= tolerance)
}

/// Total variation distance between two count histograms, in [0, 1].
pub fn total_variation_distance(
    left: &BTreeMap<String, u64>,
    right: &BTreeMap<String, u64>,
) -> Result<f64> {
    let (left_total, right_total) = (total(left), total(right));
    if left_total == 0 || right_total == 0 {
        return Err(invalid("a count histogram must hold at least one shot"));
    }
    let mut distance = 0.0;
    for key in union_keys(left, right) {
        let p = *left.get(&key).unwrap_or(&0) as f64 / left_total as f64;
        let q = *right.get(&key).unwrap_or(&0) as f64 / right_total as f64;
        distance += (p - q).abs();
    }
    Ok(0.5 * distance)
}

/// Hellinger fidelity of two count histograms, the number shown next to the
/// overlaid histograms in the run view.
pub fn hellinger_fidelity(
    left: &BTreeMap<String, u64>,
    right: &BTreeMap<String, u64>,
) -> Result<f64> {
    let (left_total, right_total) = (total(left), total(right));
    if left_total == 0 || right_total == 0 {
        return Err(invalid("a count histogram must hold at least one shot"));
    }
    let mut overlap = 0.0;
    for key in union_keys(left, right) {
        let p = *left.get(&key).unwrap_or(&0) as f64 / left_total as f64;
        let q = *right.get(&key).unwrap_or(&0) as f64 / right_total as f64;
        overlap += (p * q).sqrt();
    }
    Ok(overlap * overlap)
}

fn total(counts: &BTreeMap<String, u64>) -> u64 {
    counts.values().sum()
}

fn union_keys(left: &BTreeMap<String, u64>, right: &BTreeMap<String, u64>) -> BTreeSet<String> {
    left.keys().chain(right.keys()).cloned().collect()
}
