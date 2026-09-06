// ===== File: linalg.rs — dense complex linear algebra for small matrices =====
//
// Everything here works on row-major `n x n` buffers with `n` of 2 or 4: gate
// matrices, reduced density matrices and their functions. Nothing in this file
// touches a state vector, so its cost never scales with the qubit count.

use num_complex::Complex64;

const JACOBI_SWEEPS: usize = 60;
/// Density-matrix eigenvalues below this are rounding noise, not populations.
const EIGENVALUE_FLOOR: f64 = 1e-14;
const JACOBI_EPS: f64 = 1e-30;

pub fn identity(n: usize) -> Vec<Complex64> {
    let mut m = vec![Complex64::new(0.0, 0.0); n * n];
    for i in 0..n {
        m[i * n + i] = Complex64::new(1.0, 0.0);
    }
    m
}

pub fn matmul(a: &[Complex64], b: &[Complex64], n: usize) -> Vec<Complex64> {
    let mut out = vec![Complex64::new(0.0, 0.0); n * n];
    for i in 0..n {
        for k in 0..n {
            let aik = a[i * n + k];
            if aik == Complex64::new(0.0, 0.0) {
                continue;
            }
            for j in 0..n {
                out[i * n + j] += aik * b[k * n + j];
            }
        }
    }
    out
}

pub fn dagger(a: &[Complex64], n: usize) -> Vec<Complex64> {
    let mut out = vec![Complex64::new(0.0, 0.0); n * n];
    for i in 0..n {
        for j in 0..n {
            out[i * n + j] = a[j * n + i].conj();
        }
    }
    out
}

/// Tr(rho^2) — 1 for a pure state, 1/d for the maximally mixed one.
pub fn purity(rho: &[Complex64], n: usize) -> f64 {
    let mut sum = 0.0;
    for i in 0..n {
        for j in 0..n {
            sum += (rho[i * n + j] * rho[j * n + i]).re;
        }
    }
    sum
}

/// Gauss-Jordan inverse with partial pivoting. Returns `None` for a singular
/// matrix, which the callers treat as "pick a different branch", never as a
/// silent zero.
pub fn inverse(a: &[Complex64], n: usize) -> Option<Vec<Complex64>> {
    let mut m = a.to_vec();
    let mut inv = identity(n);
    for col in 0..n {
        let mut pivot = col;
        let mut best = m[col * n + col].norm();
        for row in (col + 1)..n {
            let candidate = m[row * n + col].norm();
            if candidate > best {
                best = candidate;
                pivot = row;
            }
        }
        if best < 1e-12 {
            return None;
        }
        if pivot != col {
            for j in 0..n {
                m.swap(col * n + j, pivot * n + j);
                inv.swap(col * n + j, pivot * n + j);
            }
        }
        let d = m[col * n + col];
        for j in 0..n {
            m[col * n + j] /= d;
            inv[col * n + j] /= d;
        }
        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = m[row * n + col];
            if factor == Complex64::new(0.0, 0.0) {
                continue;
            }
            for j in 0..n {
                let mv = m[col * n + j];
                let iv = inv[col * n + j];
                m[row * n + j] -= factor * mv;
                inv[row * n + j] -= factor * iv;
            }
        }
    }
    Some(inv)
}

/// Determinant by the same elimination, used only to score how far a candidate
/// branch stays from a singular Cayley transform.
pub fn determinant(a: &[Complex64], n: usize) -> Complex64 {
    let mut m = a.to_vec();
    let mut det = Complex64::new(1.0, 0.0);
    for col in 0..n {
        let mut pivot = col;
        let mut best = m[col * n + col].norm();
        for row in (col + 1)..n {
            let candidate = m[row * n + col].norm();
            if candidate > best {
                best = candidate;
                pivot = row;
            }
        }
        if best < 1e-15 {
            return Complex64::new(0.0, 0.0);
        }
        if pivot != col {
            for j in 0..n {
                m.swap(col * n + j, pivot * n + j);
            }
            det = -det;
        }
        let d = m[col * n + col];
        det *= d;
        for row in (col + 1)..n {
            let factor = m[row * n + col] / d;
            if factor == Complex64::new(0.0, 0.0) {
                continue;
            }
            for j in col..n {
                let mv = m[col * n + j];
                m[row * n + j] -= factor * mv;
            }
        }
    }
    det
}

/// Eigen-decomposition of a Hermitian matrix by cyclic Jacobi rotations.
///
/// Returns eigenvalues in ascending order and the matching eigenvectors as the
/// COLUMNS of the returned matrix, so `a == v * diag(values) * v^dagger`. The
/// off-diagonal element is first rotated to the real axis by a diagonal phase,
/// which turns each step into an ordinary real symmetric Jacobi rotation.
pub fn hermitian_eigh(a_in: &[Complex64], n: usize) -> (Vec<f64>, Vec<Complex64>) {
    let mut a = a_in.to_vec();
    let mut v = identity(n);

    for _ in 0..JACOBI_SWEEPS {
        let mut off = 0.0;
        for p in 0..n {
            for q in (p + 1)..n {
                off += a[p * n + q].norm_sqr();
            }
        }
        if off < JACOBI_EPS {
            break;
        }
        for p in 0..n {
            for q in (p + 1)..n {
                let apq = a[p * n + q];
                if apq.norm_sqr() < JACOBI_EPS {
                    continue;
                }
                // Phase the (p, q) entry onto the positive real axis.
                let phase = Complex64::from_polar(1.0, -apq.arg());
                for k in 0..n {
                    a[k * n + q] *= phase;
                    v[k * n + q] *= phase;
                }
                let conj_phase = phase.conj();
                for k in 0..n {
                    a[q * n + k] *= conj_phase;
                }

                let app = a[p * n + p].re;
                let aqq = a[q * n + q].re;
                let apq_r = a[p * n + q].re;
                let theta = 0.5 * (-2.0 * apq_r).atan2(app - aqq);
                let (s, c) = theta.sin_cos();

                for k in 0..n {
                    let kp = a[k * n + p];
                    let kq = a[k * n + q];
                    a[k * n + p] = c * kp - s * kq;
                    a[k * n + q] = s * kp + c * kq;
                }
                for k in 0..n {
                    let pk = a[p * n + k];
                    let qk = a[q * n + k];
                    a[p * n + k] = c * pk - s * qk;
                    a[q * n + k] = s * pk + c * qk;
                }
                for k in 0..n {
                    let kp = v[k * n + p];
                    let kq = v[k * n + q];
                    v[k * n + p] = c * kp - s * kq;
                    v[k * n + q] = s * kp + c * kq;
                }
            }
        }
    }

    let mut order: Vec<usize> = (0..n).collect();
    let values: Vec<f64> = (0..n).map(|i| a[i * n + i].re).collect();
    order.sort_by(|&i, &j| values[i].total_cmp(&values[j]));

    let mut sorted_values = Vec::with_capacity(n);
    let mut sorted_vectors = vec![Complex64::new(0.0, 0.0); n * n];
    for (new_col, &old_col) in order.iter().enumerate() {
        sorted_values.push(values[old_col]);
        for row in 0..n {
            sorted_vectors[row * n + new_col] = v[row * n + old_col];
        }
    }
    (sorted_values, sorted_vectors)
}

/// Rebuild `v * diag(f(values)) * v^dagger`.
fn recompose(values: &[Complex64], vectors: &[Complex64], n: usize) -> Vec<Complex64> {
    let mut scaled = vec![Complex64::new(0.0, 0.0); n * n];
    for row in 0..n {
        for col in 0..n {
            scaled[row * n + col] = vectors[row * n + col] * values[col];
        }
    }
    matmul(&scaled, &dagger(vectors, n), n)
}

/// Candidate global phases for the Cayley branch of `unitary_power`.
const BRANCH_CANDIDATES: usize = 16;

/// Continuous fractional power of a unitary matrix.
///
/// `u^t` is defined through the eigen-decomposition of `u`; the eigenvectors are
/// obtained from the Hermitian Cayley transform `i (I - u)(I + u)^-1`, which is a
/// function of `u` and therefore shares its eigenvectors while handling
/// degenerate eigenvalues without a special case. An eigenvalue at `-1` makes
/// that transform singular, so `u` is first multiplied by the global phase that
/// keeps `det(I + e^(i a) u)` largest; the phase is divided out again, scaled by
/// `t`, which selects one fixed continuous path from the identity to `u`.
///
/// Invariant: every eigenvalue travels the SHORT arc. The rotating phase is
/// folded into `(-pi, pi]` before it is scaled by `t`, so an eigenvalue the
/// branch search happened to carry past a full turn (`+1` under a non-principal
/// branch) holds still instead of winding through `2 pi` and back between the
/// endpoints.
pub fn unitary_power(u: &[Complex64], n: usize, t: f64) -> Vec<Complex64> {
    let id = identity(n);
    let mut best_alpha = 0.0;
    let mut best_score = -1.0;
    for k in 0..BRANCH_CANDIDATES {
        let alpha = std::f64::consts::TAU * (k as f64) / (BRANCH_CANDIDATES as f64);
        let rotated = scale(u, Complex64::from_polar(1.0, alpha));
        let sum = add(&id, &rotated);
        let score = determinant(&sum, n).norm();
        if score > best_score {
            best_score = score;
            best_alpha = alpha;
        }
    }

    let w = scale(u, Complex64::from_polar(1.0, best_alpha));
    let plus = add(&id, &w);
    let minus = sub(&id, &w);
    let inv = match inverse(&plus, n) {
        Some(inv) => inv,
        // Unreachable for a unitary `u`: at most `n` of the candidate phases can
        // make the transform singular and there are always more candidates.
        None => return u.to_vec(),
    };
    let cayley = scale(&matmul(&minus, &inv, n), Complex64::new(0.0, 1.0));
    let hermitian = hermitize(&cayley, n);

    let (values, vectors) = hermitian_eigh(&hermitian, n);
    let powered: Vec<Complex64> = values
        .iter()
        .map(|h| {
            // `phi` is the eigenphase of the ROTATED matrix, so the eigenphase of
            // `u` itself is `phi - best_alpha`, up to a full turn. Folding it
            // first is what keeps the path short: scaling an angle of `-2 pi`
            // by `t` is a full rotation of the amplitude, scaling the equal
            // angle `0` leaves it alone.
            let phi = 2.0 * h.atan();
            Complex64::from_polar(1.0, wrap_to_pi(phi - best_alpha) * t)
        })
        .collect();
    recompose(&powered, &vectors, n)
}

/// Fold an angle into `(-pi, pi]`.
fn wrap_to_pi(angle: f64) -> f64 {
    let folded = angle.rem_euclid(std::f64::consts::TAU);
    if folded > std::f64::consts::PI {
        folded - std::f64::consts::TAU
    } else {
        folded
    }
}

/// Von Neumann entropy in bits.
pub fn von_neumann_entropy(rho: &[Complex64], n: usize) -> f64 {
    let (values, _) = hermitian_eigh(rho, n);
    values
        .iter()
        .filter(|v| **v > EIGENVALUE_FLOOR)
        .map(|v| -v * v.log2())
        .sum()
}

/// Positive semi-definite square root; negative eigenvalues coming from rounding
/// are clamped to zero rather than producing a complex root.
pub fn psd_sqrt(rho: &[Complex64], n: usize) -> Vec<Complex64> {
    let (values, vectors) = hermitian_eigh(rho, n);
    let roots: Vec<Complex64> = values
        .iter()
        .map(|v| Complex64::new(v.max(0.0).sqrt(), 0.0))
        .collect();
    recompose(&roots, &vectors, n)
}

/// Wootters concurrence of a two-qubit density matrix.
pub fn concurrence(rho: &[Complex64]) -> f64 {
    const N: usize = 4;
    let spin_flip = spin_flip_matrix();
    let conjugate: Vec<Complex64> = rho.iter().map(|z| z.conj()).collect();
    let tilde = matmul(&matmul(&spin_flip, &conjugate, N), &spin_flip, N);

    // R = sqrt(rho) * rho~ * sqrt(rho) is Hermitian and has the same spectrum as
    // the non-Hermitian rho * rho~, which is what Wootters' formula needs.
    let root = psd_sqrt(rho, N);
    let r = matmul(&matmul(&root, &tilde, N), &root, N);
    let (values, _) = hermitian_eigh(&r, N);
    // The spectrum of R lies in [0, 1]; anything at rounding level is zero, and
    // it has to be clamped BEFORE the square root, which would otherwise turn a
    // 1e-17 eigenvalue of a product state into a 3e-9 concurrence.
    let mut lambdas: Vec<f64> = values
        .iter()
        .map(|v| if *v < EIGENVALUE_FLOOR { 0.0 } else { v.sqrt() })
        .collect();
    lambdas.sort_by(|a, b| b.total_cmp(a));
    (lambdas[0] - lambdas[1] - lambdas[2] - lambdas[3]).max(0.0)
}

/// Bloch vector of a single-qubit density matrix.
pub fn bloch_vector(rho: &[Complex64]) -> [f64; 3] {
    [2.0 * rho[1].re, -2.0 * rho[1].im, rho[0].re - rho[3].re]
}

fn spin_flip_matrix() -> Vec<Complex64> {
    // Y (x) Y, with Y = [[0, -i], [i, 0]].
    let zero = Complex64::new(0.0, 0.0);
    let minus_one = Complex64::new(-1.0, 0.0);
    let one = Complex64::new(1.0, 0.0);
    vec![
        zero, zero, zero, minus_one, //
        zero, zero, one, zero, //
        zero, one, zero, zero, //
        minus_one, zero, zero, zero,
    ]
}

fn scale(a: &[Complex64], factor: Complex64) -> Vec<Complex64> {
    a.iter().map(|z| z * factor).collect()
}

fn add(a: &[Complex64], b: &[Complex64]) -> Vec<Complex64> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

fn sub(a: &[Complex64], b: &[Complex64]) -> Vec<Complex64> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

fn hermitize(a: &[Complex64], n: usize) -> Vec<Complex64> {
    let adj = dagger(a, n);
    a.iter()
        .zip(adj)
        .map(|(x, y)| (x + y) * Complex64::new(0.5, 0.0))
        .collect()
}
