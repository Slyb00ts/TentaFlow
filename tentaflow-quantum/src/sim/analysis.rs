// ===== File: sim/analysis.rs — state analytics: Bloch vectors, reduced density matrices, entanglement =====
//
// Everything here reads a state vector and produces the small quantities the
// dashboard draws (plan 13.6). None of it mutates the state, so the same code
// serves the browser (T0) and the server (T1) and their keyframes compare bit
// for bit.

use num_complex::Complex64;
use serde::{Deserialize, Serialize};

use crate::error::{invalid, Result};
use crate::linalg;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Pauli {
    I,
    X,
    Y,
    Z,
}

/// Reduced density matrix of `qubits` (one or two of them), row-major.
///
/// The basis order follows the gate convention: for two qubits `qubits[0]` is
/// the most significant bit of the 4x4 index.
pub fn reduced_density_matrix(
    amps: &[Complex64],
    num_qubits: usize,
    qubits: &[usize],
) -> Result<Vec<Complex64>> {
    if qubits.is_empty() || qubits.len() > 2 {
        return Err(invalid(
            "a reduced density matrix is available for one or two qubits",
        ));
    }
    if qubits.iter().any(|q| *q >= num_qubits) {
        return Err(invalid("qubit index out of range"));
    }
    if qubits.len() == 2 && qubits[0] == qubits[1] {
        return Err(invalid("a qubit pair must name two different qubits"));
    }
    let dim = 1usize << num_qubits;
    if amps.len() != dim {
        return Err(invalid(
            "state vector length does not match the qubit count",
        ));
    }

    let k = qubits.len();
    let sub_dim = 1usize << k;
    let mut mask = 0usize;
    for q in qubits {
        mask |= 1usize << q;
    }
    // `qubits[0]` is the most significant bit of the sub-index.
    let offsets: Vec<usize> = (0..sub_dim)
        .map(|sub| {
            let mut index = 0usize;
            for (position, q) in qubits.iter().enumerate() {
                let bit = (sub >> (k - 1 - position)) & 1;
                index |= bit << q;
            }
            index
        })
        .collect();

    let mut rho = vec![Complex64::new(0.0, 0.0); sub_dim * sub_dim];
    for base in 0..dim {
        if base & mask != 0 {
            continue;
        }
        for row in 0..sub_dim {
            let a = amps[base | offsets[row]];
            if a == Complex64::new(0.0, 0.0) {
                continue;
            }
            for col in 0..sub_dim {
                rho[row * sub_dim + col] += a * amps[base | offsets[col]].conj();
            }
        }
    }
    Ok(rho)
}

/// Bloch vector of every qubit in a single pass over the state vector, which is
/// what makes a keyframe affordable at 28 qubits (plan 13.6).
pub fn bloch_vectors(amps: &[Complex64], num_qubits: usize) -> Result<Vec<[f64; 3]>> {
    let dim = 1usize << num_qubits;
    if amps.len() != dim {
        return Err(invalid(
            "state vector length does not match the qubit count",
        ));
    }
    let mut coherence = vec![Complex64::new(0.0, 0.0); num_qubits];
    let mut z = vec![0.0f64; num_qubits];
    for (index, a) in amps.iter().enumerate() {
        let weight = a.norm_sqr();
        for q in 0..num_qubits {
            if index >> q & 1 == 0 {
                z[q] += weight;
                coherence[q] += a * amps[index | (1usize << q)].conj();
            } else {
                z[q] -= weight;
            }
        }
    }
    Ok((0..num_qubits)
        .map(|q| [2.0 * coherence[q].re, -2.0 * coherence[q].im, z[q]])
        .collect())
}

/// Tr(rho_q^2) for every qubit; 1 means the qubit is not entangled with the rest.
///
/// Derived from Bloch vectors the caller already has, because the pass that
/// produces them is `O(n * 2^n)` — the dominant cost of a keyframe at 24 qubits
/// — and must not run a second time just to report purity (plan 13.6).
pub fn purity_from_bloch(bloch: &[[f64; 3]]) -> Vec<f64> {
    bloch
        .iter()
        .map(|v| 0.5 * (1.0 + v[0] * v[0] + v[1] * v[1] + v[2] * v[2]))
        .collect()
}

/// Quantum mutual information `S(i) + S(j) - S(ij)` in bits.
pub fn mutual_information(
    amps: &[Complex64],
    num_qubits: usize,
    i: usize,
    j: usize,
) -> Result<f64> {
    let rho_i = reduced_density_matrix(amps, num_qubits, &[i])?;
    let rho_j = reduced_density_matrix(amps, num_qubits, &[j])?;
    let rho_ij = reduced_density_matrix(amps, num_qubits, &[i, j])?;
    Ok(
        linalg::von_neumann_entropy(&rho_i, 2) + linalg::von_neumann_entropy(&rho_j, 2)
            - linalg::von_neumann_entropy(&rho_ij, 4),
    )
}

/// Wootters concurrence of the qubit pair.
pub fn concurrence(amps: &[Complex64], num_qubits: usize, i: usize, j: usize) -> Result<f64> {
    let rho_ij = reduced_density_matrix(amps, num_qubits, &[i, j])?;
    Ok(linalg::concurrence(&rho_ij))
}

/// Expectation value of a tensor product of Pauli operators.
pub fn pauli_expectation(
    amps: &[Complex64],
    num_qubits: usize,
    terms: &[(usize, Pauli)],
) -> Result<f64> {
    let dim = 1usize << num_qubits;
    if amps.len() != dim {
        return Err(invalid(
            "state vector length does not match the qubit count",
        ));
    }
    let mut seen = vec![false; num_qubits];
    let mut flip_mask = 0usize;
    let mut y_mask = 0usize;
    let mut z_mask = 0usize;
    for (qubit, pauli) in terms {
        if *qubit >= num_qubits {
            return Err(invalid("qubit index out of range"));
        }
        if seen[*qubit] {
            return Err(invalid("a Pauli term names the same qubit twice"));
        }
        seen[*qubit] = true;
        match pauli {
            Pauli::I => {}
            Pauli::X => flip_mask |= 1usize << qubit,
            Pauli::Y => {
                flip_mask |= 1usize << qubit;
                y_mask |= 1usize << qubit;
            }
            Pauli::Z => z_mask |= 1usize << qubit,
        }
    }

    let mut total = Complex64::new(0.0, 0.0);
    for (index, a) in amps.iter().enumerate() {
        if *a == Complex64::new(0.0, 0.0) {
            continue;
        }
        // Y|0> = i|1>, Y|1> = -i|0>; Z|1> = -|1>.
        let y_ones = (index & y_mask).count_ones();
        let y_zeros = y_mask.count_ones() - y_ones;
        let sign_z = if (index & z_mask).count_ones().is_multiple_of(2) {
            1.0
        } else {
            -1.0
        };
        let i_power =
            Complex64::new(0.0, 1.0).powu(y_zeros) * Complex64::new(0.0, -1.0).powu(y_ones);
        let coefficient = i_power * sign_z;
        total += amps[index ^ flip_mask].conj() * coefficient * a;
    }
    Ok(total.re)
}
