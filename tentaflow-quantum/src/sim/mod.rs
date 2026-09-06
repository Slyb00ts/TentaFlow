// ===== File: sim/mod.rs — simulator backends and the scalar types they run on =====

pub mod analysis;
pub mod cpu;
pub mod stabilizer;
pub mod statevector;

/// GPU state-vector backend (plan 6.3). Native only: the browser tier runs the
/// CPU kernels in wasm, and WebGPU is a separate build that does not exist yet.
#[cfg(all(feature = "wgpu", not(target_arch = "wasm32")))]
pub mod wgpu;

use num_complex::{Complex, Complex64};
use serde::{Deserialize, Serialize};

/// Amplitude precision of a backend. `wgpu` will only ever offer `Single`, so
/// the choice is part of the public contract and shown in the UI (plan 18.11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Precision {
    Single,
    Double,
}

/// Which device a simulator runs on (plan 6.3).
///
/// There is no `Cuda` variant because there is no CUDA backend yet; `Auto`
/// therefore short-circuits the plan's `cuda -> wgpu -> cpu` cascade to its
/// last two steps rather than pretending the first one exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Device {
    /// The fastest device that can serve the requested precision.
    Auto,
    Cpu,
    Wgpu,
}

impl Device {
    /// The device a request actually lands on.
    ///
    /// `Auto` skips the GPU when the caller asked for double precision: WGSL
    /// has no f64 and `SHADER_F64` was rejected (plan 18.11), so answering a
    /// `complex128` request on the GPU would silently halve the precision.
    pub fn resolve(self, precision: Precision) -> Device {
        match self {
            Device::Cpu | Device::Wgpu => self,
            Device::Auto if precision == Precision::Single && wgpu_adapter_exists() => Device::Wgpu,
            Device::Auto => Device::Cpu,
        }
    }
}

#[cfg(all(feature = "wgpu", not(target_arch = "wasm32")))]
fn wgpu_adapter_exists() -> bool {
    self::wgpu::adapter_report().is_ok()
}

#[cfg(not(all(feature = "wgpu", not(target_arch = "wasm32"))))]
fn wgpu_adapter_exists() -> bool {
    false
}

/// What a running simulator is: the backend that holds the state, the physical
/// device behind it and the precision of its amplitudes. The target picker in
/// the UI shows all three (plan 18.11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Description {
    pub backend: String,
    /// Name of the physical device; `None` on the CPU.
    pub adapter: Option<String>,
    pub precision: Precision,
    pub num_qubits: usize,
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
/// provide (plan 6.3). `cpu` and `wgpu` implement it; a future `cuda` plugs in
/// here without touching the IR, the scheduler or the analytics above them.
pub trait Backend: Send {
    fn name(&self) -> &'static str;

    /// Name of the physical device the state lives on, for backends that have
    /// one. The CPU backend has none, so a caller can tell "ran on the CPU"
    /// from "ran on a GPU nobody can name".
    fn adapter_name(&self) -> Option<&str>;

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

/// A caller's ability to end a long run early.
///
/// Two dimensions make the work here unbounded, and both come from the request
/// rather than from the machine: the SHOT count (a program with a reset, a
/// classical guard or a gate after a measurement is replayed once per shot, and
/// the tableau replays every shot whatever the program contains) and the GATE
/// count (the parser accepts programs of up to a million operations, and every
/// one of them is a full pass over the state). A million of either is both a
/// legitimate ask and a runaway one, so a server driving these has to be able
/// to stop them; without a hook here it would have to reimplement the loops,
/// and the two copies would drift apart on the next change to the count key or
/// the seeding.
///
/// The hook is therefore asked on both loops — between shots and between gates
/// — so a stop lands within one gate rather than within one circuit.
///
/// [`Cancel::none`] is the default and costs one branch per question.
#[derive(Clone, Copy, Default)]
pub struct Cancel<'a> {
    hook: Option<&'a (dyn Fn() -> bool + Sync)>,
}

impl<'a> Cancel<'a> {
    /// A loop that runs to the end.
    pub fn none() -> Self {
        Cancel { hook: None }
    }

    /// Asks `hook` between shots and between gates; `true` ends the run with
    /// [`crate::Error::Cancelled`].
    pub fn new(hook: &'a (dyn Fn() -> bool + Sync)) -> Self {
        Cancel { hook: Some(hook) }
    }

    pub fn stopped(&self) -> bool {
        self.hook.is_some_and(|hook| hook())
    }
}

impl std::fmt::Debug for Cancel<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cancel")
            .field("hooked", &self.hook.is_some())
            .finish()
    }
}
