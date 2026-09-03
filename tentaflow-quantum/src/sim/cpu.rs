// ===== File: sim/cpu.rs — CPU state-vector backend (rayon natively, single-thread in wasm) =====

use num_complex::{Complex, Complex64};

use super::{cast, uncast, Backend, GateOp, Precision, Scalar};

/// Below this many amplitudes per half-block the inner loop is left serial:
/// splitting it costs more than the work it hands out.
#[cfg(not(target_arch = "wasm32"))]
const INNER_PARALLEL_THRESHOLD: usize = 1 << 12;

pub struct CpuBackend<S: Scalar> {
    num_qubits: usize,
    amps: Vec<Complex<S>>,
}

impl<S: Scalar> CpuBackend<S> {
    pub fn new(num_qubits: usize) -> CpuBackend<S> {
        let mut backend = CpuBackend {
            num_qubits,
            amps: vec![Complex::new(S::zero(), S::zero()); 1usize << num_qubits],
        };
        backend.reset_to_zero();
        backend
    }

    fn apply_one(&mut self, qubit: usize, matrix: &[Complex64; 4]) {
        let m = [
            cast::<S>(matrix[0]),
            cast::<S>(matrix[1]),
            cast::<S>(matrix[2]),
            cast::<S>(matrix[3]),
        ];
        for_each_pair(&mut self.amps, 1usize << qubit, move |a, b| {
            let (x, y) = (*a, *b);
            *a = m[0] * x + m[1] * y;
            *b = m[2] * x + m[3] * y;
        });
    }

    fn apply_two(&mut self, qubits: (usize, usize), matrix: &[Complex64; 16]) {
        let (first, second) = qubits;
        // The kernel always addresses (high bit, low bit); when the first
        // operand is the low bit the matrix basis order 01 <-> 10 is swapped.
        let ordered = if first > second {
            *matrix
        } else {
            swap_basis_middle(matrix)
        };
        let mut m = [Complex::new(S::zero(), S::zero()); 16];
        for (dst, src) in m.iter_mut().zip(ordered.iter()) {
            *dst = cast(*src);
        }
        let high = first.max(second);
        let low = first.min(second);
        for_each_quad(&mut self.amps, high, low, move |a00, a01, a10, a11| {
            let (x0, x1, x2, x3) = (*a00, *a01, *a10, *a11);
            *a00 = m[0] * x0 + m[1] * x1 + m[2] * x2 + m[3] * x3;
            *a01 = m[4] * x0 + m[5] * x1 + m[6] * x2 + m[7] * x3;
            *a10 = m[8] * x0 + m[9] * x1 + m[10] * x2 + m[11] * x3;
            *a11 = m[12] * x0 + m[13] * x1 + m[14] * x2 + m[15] * x3;
        });
    }
}

impl<S: Scalar> Backend for CpuBackend<S> {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn precision(&self) -> Precision {
        S::PRECISION
    }

    fn num_qubits(&self) -> usize {
        self.num_qubits
    }

    fn reset_to_zero(&mut self) {
        self.amps.fill(Complex::new(S::zero(), S::zero()));
        self.amps[0] = Complex::new(S::one(), S::zero());
    }

    fn set_amplitudes(&mut self, amps: &[Complex64]) {
        debug_assert_eq!(amps.len(), self.amps.len());
        for (dst, src) in self.amps.iter_mut().zip(amps) {
            *dst = cast(*src);
        }
    }

    fn apply(&mut self, ops: &[GateOp]) {
        for op in ops {
            match op {
                GateOp::One { qubit, matrix } => self.apply_one(*qubit, matrix),
                GateOp::Two { qubits, matrix } => self.apply_two(*qubits, matrix),
            }
        }
    }

    fn apply_global_phase(&mut self, angle: f64) {
        let factor = cast::<S>(Complex64::from_polar(1.0, angle));
        for a in self.amps.iter_mut() {
            *a = *a * factor;
        }
    }

    fn probability_of_one(&self, qubit: usize) -> f64 {
        let mask = 1usize << qubit;
        self.amps
            .iter()
            .enumerate()
            .filter(|(i, _)| i & mask != 0)
            .map(|(_, a)| a.norm_sqr().as_f64())
            .sum()
    }

    fn collapse(&mut self, qubit: usize, outcome: bool) {
        let mask = 1usize << qubit;
        let mut norm = 0.0f64;
        for (i, a) in self.amps.iter_mut().enumerate() {
            if ((i & mask) != 0) == outcome {
                norm += a.norm_sqr().as_f64();
            } else {
                *a = Complex::new(S::zero(), S::zero());
            }
        }
        debug_assert!(
            norm > 0.0,
            "collapse onto an outcome with zero probability has no normalisation"
        );
        let scale = S::from_real(1.0 / norm.sqrt());
        for a in self.amps.iter_mut() {
            *a = Complex::new(a.re * scale, a.im * scale);
        }
    }

    fn probabilities(&self) -> Vec<f64> {
        self.amps.iter().map(|a| a.norm_sqr().as_f64()).collect()
    }

    fn sample(&self, sorted_draws: &[f64]) -> Vec<usize> {
        let mut out = Vec::with_capacity(sorted_draws.len());
        let mut cumulative = 0.0f64;
        let mut last = 0usize;
        for (index, amp) in self.amps.iter().enumerate() {
            if out.len() == sorted_draws.len() {
                break;
            }
            let p = amp.norm_sqr().as_f64();
            if p == 0.0 {
                continue;
            }
            cumulative += p;
            last = index;
            while out.len() < sorted_draws.len() && sorted_draws[out.len()] < cumulative {
                out.push(index);
            }
        }
        // Rounding can leave the last draws past the accumulated total; they
        // belong to the last outcome with non-zero probability, which is where
        // the missing mass came from.
        while out.len() < sorted_draws.len() {
            out.push(last);
        }
        out
    }

    fn read_amplitudes(&self, out: &mut [Complex64]) {
        debug_assert_eq!(out.len(), self.amps.len());
        for (dst, src) in out.iter_mut().zip(self.amps.iter()) {
            *dst = uncast(*src);
        }
    }
}

/// Swap the 01 and 10 basis elements of a 4x4 matrix, in rows and columns.
fn swap_basis_middle(matrix: &[Complex64; 16]) -> [Complex64; 16] {
    const ORDER: [usize; 4] = [0, 2, 1, 3];
    let mut out = [Complex64::new(0.0, 0.0); 16];
    for (row, &r) in ORDER.iter().enumerate() {
        for (col, &c) in ORDER.iter().enumerate() {
            out[row * 4 + col] = matrix[r * 4 + c];
        }
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
fn for_each_pair<S, F>(amps: &mut [Complex<S>], half: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut Complex<S>, &mut Complex<S>) + Send + Sync,
{
    use rayon::prelude::*;
    amps.par_chunks_mut(half << 1).for_each(|chunk| {
        let (lo, hi) = chunk.split_at_mut(half);
        if half >= INNER_PARALLEL_THRESHOLD {
            lo.par_iter_mut()
                .zip(hi.par_iter_mut())
                .for_each(|(a, b)| f(a, b));
        } else {
            lo.iter_mut().zip(hi.iter_mut()).for_each(|(a, b)| f(a, b));
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn for_each_pair<S, F>(amps: &mut [Complex<S>], half: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut Complex<S>, &mut Complex<S>) + Send + Sync,
{
    for chunk in amps.chunks_mut(half << 1) {
        let (lo, hi) = chunk.split_at_mut(half);
        lo.iter_mut().zip(hi.iter_mut()).for_each(|(a, b)| f(a, b));
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn for_each_quad<S, F>(amps: &mut [Complex<S>], high: usize, low: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut Complex<S>, &mut Complex<S>, &mut Complex<S>, &mut Complex<S>) + Send + Sync,
{
    use rayon::prelude::*;
    let high_half = 1usize << high;
    let low_half = 1usize << low;
    amps.par_chunks_mut(high_half << 1).for_each(|chunk| {
        let (zero_block, one_block) = chunk.split_at_mut(high_half);
        zero_block
            .chunks_mut(low_half << 1)
            .zip(one_block.chunks_mut(low_half << 1))
            .for_each(|(d0, d1)| {
                let (a00, a01) = d0.split_at_mut(low_half);
                let (a10, a11) = d1.split_at_mut(low_half);
                for i in 0..low_half {
                    f(&mut a00[i], &mut a01[i], &mut a10[i], &mut a11[i]);
                }
            });
    });
}

#[cfg(target_arch = "wasm32")]
fn for_each_quad<S, F>(amps: &mut [Complex<S>], high: usize, low: usize, f: F)
where
    S: Scalar,
    F: Fn(&mut Complex<S>, &mut Complex<S>, &mut Complex<S>, &mut Complex<S>) + Send + Sync,
{
    let high_half = 1usize << high;
    let low_half = 1usize << low;
    for chunk in amps.chunks_mut(high_half << 1) {
        let (zero_block, one_block) = chunk.split_at_mut(high_half);
        for (d0, d1) in zero_block
            .chunks_mut(low_half << 1)
            .zip(one_block.chunks_mut(low_half << 1))
        {
            let (a00, a01) = d0.split_at_mut(low_half);
            let (a10, a11) = d1.split_at_mut(low_half);
            for i in 0..low_half {
                f(&mut a00[i], &mut a01[i], &mut a10[i], &mut a11[i]);
            }
        }
    }
}
