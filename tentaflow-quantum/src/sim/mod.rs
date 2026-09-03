// ===== File: sim/mod.rs — simulator backends and the scalar types they run on =====

pub mod analysis;
pub mod cpu;
pub mod stabilizer;
pub mod statevector;

use num_complex::{Complex, Complex64};
use serde::{Deserialize, Serialize};

/// Amplitude precision of a backend. `wgpu` will only ever offer `Single`, so
/// the choice is part of the public contract and shown in the UI (plan 18.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Precision {
    Single,
    Double,
}

/// Real type an amplitude is stored in. `f32` and `f64` are the only two
/// implementations and both are exercised by the test suite.
pub trait Scalar: num_traits::Float + Send + Sync + std::fmt::Debug + 'static {
    const PRECISION: Precision;

    /// Named `from_real`/`as_f64` rather than `from_f64`/`to_f64` because
    /// `num_traits::ToPrimitive` already owns those names for every float.
    fn from_real(value: f64) -> Self;
    fn as_f64(self) -> f64;
}

impl Scalar for f32 {
    const PRECISION: Precision = Precision::Single;

    fn from_real(value: f64) -> Self {
        value as f32
    }

    fn as_f64(self) -> f64 {
        self as f64
    }
}

impl Scalar for f64 {
    const PRECISION: Precision = Precision::Double;

    fn from_real(value: f64) -> Self {
        value
    }

    fn as_f64(self) -> f64 {
        self
    }
}

pub(crate) fn cast<S: Scalar>(z: Complex64) -> Complex<S> {
    Complex::new(S::from_real(z.re), S::from_real(z.im))
}

pub(crate) fn uncast<S: Scalar>(z: Complex<S>) -> Complex64 {
    Complex64::new(z.re.as_f64(), z.im.as_f64())
}

/// A unitary already resolved to concrete qubit indices and a dense matrix.
/// The first operand is the most significant bit of the 2-qubit matrix index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GateOp {
    One {
        qubit: usize,
        matrix: [Complex64; 4],
    },
    Two {
        qubits: (usize, usize),
        matrix: [Complex64; 16],
    },
}

impl GateOp {
    pub fn qubits(&self) -> Vec<usize> {
        match self {
            GateOp::One { qubit, .. } => vec![*qubit],
            GateOp::Two { qubits, .. } => vec![qubits.0, qubits.1],
        }
    }

    pub fn matrix(&self) -> Vec<Complex64> {
        match self {
            GateOp::One { matrix, .. } => matrix.to_vec(),
            GateOp::Two { matrix, .. } => matrix.to_vec(),
        }
    }
}

/// State storage and the primitive operations every simulator device has to
/// provide. `cpu` is the first implementation; `cuda` and `wgpu` (plan 6.3) plug
/// in here without touching the IR, the scheduler or the analytics above them.
pub trait Backend: Send {
    fn name(&self) -> &'static str;
    fn precision(&self) -> Precision;
    fn num_qubits(&self) -> usize;

    /// Put the register back into |0...0>.
    fn reset_to_zero(&mut self);

    /// Load a full state, converted to the backend's precision. The fractional
    /// step preview and the unitary builder both need to start from a state that
    /// no sequence of gates produced.
    fn set_amplitudes(&mut self, amps: &[Complex64]);

    /// Apply a batch of gates in order. Batching exists so a device backend can
    /// submit one command buffer instead of one per gate.
    fn apply(&mut self, ops: &[GateOp]);

    /// Multiply the whole state by exp(i * angle).
    fn apply_global_phase(&mut self, angle: f64);

    /// Probability of measuring |1> on `qubit`.
    fn probability_of_one(&self, qubit: usize) -> f64;

    /// Project onto the given outcome and renormalise. The outcome must have
    /// non-zero probability; projecting onto an impossible one has no
    /// normalisation and is a caller bug.
    fn collapse(&mut self, qubit: usize, outcome: bool);

    /// Probability of every computational basis state.
    fn probabilities(&self) -> Vec<f64>;

    /// Basis-state index for each uniform draw in `sorted_draws`, which must be
    /// ascending values in `[0, 1)`. Sampling is a backend primitive (plan 6.3)
    /// so a device backend can answer from a prefix reduction on the device
    /// instead of shipping 2^n probabilities to the host.
    fn sample(&self, sorted_draws: &[f64]) -> Vec<usize>;

    /// Copy the whole state into `out`, always in double precision. `out` holds
    /// one entry per basis state. The stepper reuses a single buffer across
    /// animation frames, which is why this exists next to `amplitudes`.
    fn read_amplitudes(&self, out: &mut [Complex64]);

    /// Full amplitude read-back into a fresh vector.
    fn amplitudes(&self) -> Vec<Complex64> {
        let mut out = vec![Complex64::new(0.0, 0.0); 1usize << self.num_qubits()];
        self.read_amplitudes(&mut out);
        out
    }
}
