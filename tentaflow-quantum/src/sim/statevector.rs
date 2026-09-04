// ===== File: sim/statevector.rs — state-vector scheduler, stepping, sampling and keyframes =====

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

use num_complex::Complex64;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use serde::{Deserialize, Serialize};

use super::analysis;
use super::cpu::CpuBackend;
use super::{Backend, Cancel, GateOp, Precision};
use crate::error::{invalid, Error, Result};
use crate::gate::{Gate, Matrix};
use crate::ir::{Circuit, Condition, OpKind};
use crate::linalg;

/// Largest register the CPU backend will allocate. 30 qubits is 8 GiB of
/// `complex64` amplitudes and 16 GiB in the default `complex128`; anything above
/// that has to be refused before allocation rather than discovered as an OOM
/// mid-run (plan 4.2).
pub const DEFAULT_MAX_QUBITS: usize = 30;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SimOptions {
    pub precision: Precision,
    pub max_qubits: usize,
    /// Seed of the measurement stream; a run with the same seed and the same
    /// circuit produces the same counts, which is what `method.md` promises.
    pub seed: u64,
}

impl Default for SimOptions {
    fn default() -> Self {
        SimOptions {
            precision: Precision::Double,
            max_qubits: DEFAULT_MAX_QUBITS,
            seed: 0,
        }
    }
}

/// One executable instruction. Gates are already dense matrices on concrete
/// qubits, so the backend never looks at the IR.
///
/// The two-qubit matrix is stored inline even though it dwarfs the other
/// variants: a device backend submits `&[GateOp]` straight from here, and
/// boxing it would put a pointer chase in front of every gate.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    Unitary(GateOp),
    GlobalPhase(f64),
    Measure { qubit: usize, clbit: usize },
    Reset { qubit: usize },
    Barrier,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub instruction: Instruction,
    pub conditions: Vec<Condition>,
    /// Index into `Circuit::ops`, or `None` for an instruction produced by
    /// fusing several gates.
    pub source: Option<usize>,
    /// Name shown in a keyframe; `fused` for merged single-qubit gates.
    pub label: &'static str,
}

/// Translate a circuit into the executable program, one step per IR operation.
pub fn compile(circuit: &Circuit) -> Result<Vec<Step>> {
    let mut steps = Vec::with_capacity(circuit.ops().len());
    for (index, op) in circuit.ops().iter().enumerate() {
        let (instruction, label) = match &op.kind {
            OpKind::Gate { gate, qubits } => (
                Instruction::Unitary(gate_op(*gate, qubits)?),
                gate.qasm_name(),
            ),
            OpKind::GlobalPhase(angle) => (Instruction::GlobalPhase(*angle), "gphase"),
            OpKind::Measure { qubit, clbit } => (
                Instruction::Measure {
                    qubit: *qubit,
                    clbit: *clbit,
                },
                "measure",
            ),
            OpKind::Reset { qubit } => (Instruction::Reset { qubit: *qubit }, "reset"),
            OpKind::Barrier { .. } => (Instruction::Barrier, "barrier"),
        };
        steps.push(Step {
            instruction,
            conditions: op.conditions.clone(),
            source: Some(index),
            label,
        });
    }
    Ok(steps)
}

/// Merge runs of unconditional single-qubit gates on the same qubit into one
/// matrix. A 2-qubit gate, a measurement, a reset or a classical guard on that
/// qubit ends the run, so the merged matrix is always applied at the same point
/// in the circuit as the gates it replaces.
pub fn fuse(steps: &[Step]) -> Vec<Step> {
    let mut out: Vec<Step> = Vec::with_capacity(steps.len());
    for step in steps {
        let fusable = step.conditions.is_empty()
            && matches!(step.instruction, Instruction::Unitary(GateOp::One { .. }));
        if fusable {
            if let (
                Some(GateOp::One {
                    qubit: prev_qubit,
                    matrix: prev_matrix,
                }),
                Instruction::Unitary(GateOp::One { qubit, matrix }),
            ) = (last_single_qubit(&out), &step.instruction)
            {
                if prev_qubit == *qubit {
                    let merged = linalg::matmul(matrix, &prev_matrix, 2);
                    let last = out.last_mut().expect("checked by last_single_qubit");
                    last.instruction = Instruction::Unitary(GateOp::One {
                        qubit: *qubit,
                        matrix: [merged[0], merged[1], merged[2], merged[3]],
                    });
                    last.source = None;
                    last.label = "fused";
                    continue;
                }
            }
        }
        out.push(step.clone());
    }
    out
}

fn last_single_qubit(steps: &[Step]) -> Option<GateOp> {
    match steps.last() {
        Some(Step {
            instruction: Instruction::Unitary(op @ GateOp::One { .. }),
            conditions,
            ..
        }) if conditions.is_empty() => Some(*op),
        _ => None,
    }
}

fn gate_op(gate: Gate, qubits: &[usize]) -> Result<GateOp> {
    match gate.matrix() {
        Matrix::One(matrix) => Ok(GateOp::One {
            qubit: qubits[0],
            matrix,
        }),
        Matrix::Two(matrix) => Ok(GateOp::Two {
            qubits: (qubits[0], qubits[1]),
            matrix,
        }),
    }
}

fn make_backend(num_qubits: usize, options: &SimOptions) -> Result<Box<dyn Backend>> {
    if num_qubits == 0 {
        return Err(invalid("circuit declares no qubits"));
    }
    if num_qubits > options.max_qubits {
        return Err(Error::TooManyQubits {
            qubits: num_qubits,
            limit: options.max_qubits,
        });
    }
    Ok(match options.precision {
        Precision::Single => Box::new(CpuBackend::<f32>::new(num_qubits)),
        Precision::Double => Box::new(CpuBackend::<f64>::new(num_qubits)),
    })
}

/// Stateful executor for the circuit editor: it walks the program one IR
/// operation at a time and can describe the state at every stop.
pub struct Simulator {
    circuit: Circuit,
    program: Vec<Step>,
    backend: Box<dyn Backend>,
    /// Scratch register for `step_fraction`, built on the first preview and
    /// kept afterwards: the time slider redraws a frame at a time and must not
    /// allocate a whole state per frame (plan 13.6).
    preview: Option<Box<dyn Backend>>,
    /// Buffer the live state is read into before it is handed to `preview`.
    transfer: Vec<Complex64>,
    clbits: Vec<bool>,
    position: usize,
    rng: StdRng,
}

impl Simulator {
    pub fn new(circuit: &Circuit, options: &SimOptions) -> Result<Simulator> {
        let program = compile(circuit)?;
        let backend = make_backend(circuit.num_qubits(), options)?;
        Ok(Simulator {
            clbits: vec![false; circuit.num_clbits()],
            circuit: circuit.clone(),
            program,
            backend,
            preview: None,
            transfer: Vec::new(),
            position: 0,
            rng: StdRng::seed_from_u64(options.seed),
        })
    }

    pub fn num_qubits(&self) -> usize {
        self.backend.num_qubits()
    }

    pub fn backend_name(&self) -> &'static str {
        self.backend.name()
    }

    pub fn precision(&self) -> Precision {
        self.backend.precision()
    }

    /// Number of executable steps; equal to the number of IR operations.
    pub fn step_count(&self) -> usize {
        self.program.len()
    }

    /// How many steps have been applied so far.
    pub fn position(&self) -> usize {
        self.position
    }

    pub fn clbits(&self) -> &[bool] {
        &self.clbits
    }

    pub fn amplitudes(&self) -> Vec<Complex64> {
        self.backend.amplitudes()
    }

    pub fn probabilities(&self) -> Vec<f64> {
        self.backend.probabilities()
    }

    pub fn rewind(&mut self) {
        self.backend.reset_to_zero();
        self.clbits.iter_mut().for_each(|b| *b = false);
        self.position = 0;
    }

    /// Apply the next operation. Returns `false` once the program is exhausted.
    pub fn step(&mut self) -> bool {
        if self.position >= self.program.len() {
            return false;
        }
        let step = self.program[self.position].clone();
        if self.circuit.conditions_hold(&step.conditions, &self.clbits) {
            self.execute(&step.instruction);
        }
        self.position += 1;
        true
    }

    pub fn run_to_end(&mut self) {
        while self.step() {}
    }

    /// State after applying the fraction `t` of the pending operation, without
    /// consuming it. `t == 1.0` returns exactly what `step` would produce for
    /// unitary instructions, which is the contract the animated time slider in
    /// the run view relies on (plan 13.6).
    pub fn step_fraction(&mut self, t: f64) -> Result<Vec<Complex64>> {
        let Simulator {
            circuit,
            program,
            backend,
            preview,
            transfer,
            clbits,
            position,
            ..
        } = self;
        let step = program
            .get(*position)
            .ok_or_else(|| invalid("the program has no pending operation"))?;
        if !circuit.conditions_hold(&step.conditions, clbits) {
            return Ok(backend.amplitudes());
        }
        let partial = match &step.instruction {
            Instruction::Unitary(op) => fractional_gate(op, t, circuit, step.source),
            Instruction::GlobalPhase(angle) => {
                let mut amps = backend.amplitudes();
                let factor = Complex64::from_polar(1.0, angle * t);
                amps.iter_mut().for_each(|a| *a *= factor);
                return Ok(amps);
            }
            Instruction::Barrier => return Ok(backend.amplitudes()),
            Instruction::Measure { .. } | Instruction::Reset { .. } => {
                return Err(invalid(
                    "a measurement or reset has no fractional form; step through it instead",
                ))
            }
        };
        if preview.is_none() {
            *preview = Some(make_backend(
                backend.num_qubits(),
                &SimOptions {
                    precision: backend.precision(),
                    max_qubits: backend.num_qubits(),
                    seed: 0,
                },
            )?);
        }
        let preview = preview.as_mut().expect("built just above");
        transfer.resize(1usize << backend.num_qubits(), Complex64::new(0.0, 0.0));
        backend.read_amplitudes(transfer);
        preview.set_amplitudes(transfer);
        preview.apply(&[partial]);
        Ok(preview.amplitudes())
    }

    fn execute(&mut self, instruction: &Instruction) {
        match instruction {
            Instruction::Unitary(op) => self.backend.apply(std::slice::from_ref(op)),
            Instruction::GlobalPhase(angle) => self.backend.apply_global_phase(*angle),
            Instruction::Barrier => {}
            Instruction::Measure { qubit, clbit } => {
                let outcome = self.sample_qubit(*qubit);
                self.clbits[*clbit] = outcome;
            }
            Instruction::Reset { qubit } => {
                let outcome = self.sample_qubit(*qubit);
                if outcome {
                    self.backend.apply(&[GateOp::One {
                        qubit: *qubit,
                        matrix: match Gate::X.matrix() {
                            Matrix::One(m) => m,
                            Matrix::Two(_) => unreachable!("X is a 1-qubit gate"),
                        },
                    }]);
                }
            }
        }
    }

    fn sample_qubit(&mut self, qubit: usize) -> bool {
        let p_one = self.backend.probability_of_one(qubit);
        let outcome = self.rng.random::<f64>() < p_one;
        self.backend.collapse(qubit, outcome);
        outcome
    }

    pub fn reduced_density_matrix(&self, qubits: &[usize]) -> Result<Vec<Complex64>> {
        analysis::reduced_density_matrix(&self.backend.amplitudes(), self.num_qubits(), qubits)
    }

    pub fn bloch_vectors(&self) -> Result<Vec<[f64; 3]>> {
        analysis::bloch_vectors(&self.backend.amplitudes(), self.num_qubits())
    }

    pub fn mutual_information(&self, i: usize, j: usize) -> Result<f64> {
        analysis::mutual_information(&self.backend.amplitudes(), self.num_qubits(), i, j)
    }

    pub fn concurrence(&self, i: usize, j: usize) -> Result<f64> {
        analysis::concurrence(&self.backend.amplitudes(), self.num_qubits(), i, j)
    }

    pub fn pauli_expectation(&self, terms: &[(usize, analysis::Pauli)]) -> Result<f64> {
        analysis::pauli_expectation(&self.backend.amplitudes(), self.num_qubits(), terms)
    }

    /// Sample the computational basis of the CURRENT state, one draw per shot.
    ///
    /// This is the live histogram of plan 13.6: shots are drawn from the state
    /// the editor is showing, without collapsing it, so refreshing the histogram
    /// cannot change what the next `step` does. The draw stream carries its own
    /// seed for the same reason — it must not consume the measurement stream.
    ///
    /// Keys name QUBITS (bit 0 rightmost), not the classical register, because
    /// a state that has not been measured has no register image yet; `run`
    /// keys its counts by clbit.
    pub fn sample_counts(&self, shots: u64, seed: u64) -> Result<RunResult> {
        if shots == 0 {
            return Err(invalid("a histogram needs at least one shot"));
        }
        let num_qubits = self.num_qubits();
        let mut counts: BTreeMap<String, u64> = BTreeMap::new();
        for index in self.backend.sample(&sorted_draws(seed, shots)) {
            *counts.entry(bitstring(index, num_qubits)).or_insert(0) += 1;
        }
        Ok(RunResult { counts, shots })
    }

    /// Everything the run view needs to draw the state after the last applied
    /// step: Bloch vectors of every qubit, reduced density matrices of the
    /// selected pairs and the largest amplitudes with their gate partners.
    pub fn keyframe(&self, options: &KeyframeOptions) -> Result<Keyframe> {
        let amps = self.backend.amplitudes();
        let num_qubits = self.num_qubits();
        let applied = self.position;
        let gate = if applied == 0 {
            None
        } else {
            let step = &self.program[applied - 1];
            match &step.instruction {
                Instruction::Unitary(op) => Some(GateInfo {
                    name: step.label.to_string(),
                    qubits: op.qubits(),
                    matrix: op.matrix(),
                }),
                _ => Some(GateInfo {
                    name: step.label.to_string(),
                    qubits: step
                        .source
                        .and_then(|index| self.circuit.ops().get(index))
                        .map(|op| op.qubits().to_vec())
                        .unwrap_or_default(),
                    matrix: Vec::new(),
                }),
            }
        };
        let gate_qubits = gate.as_ref().map(|g| g.qubits.clone()).unwrap_or_default();

        let pairs = self.pair_list(&options.pairs, &gate_qubits, num_qubits)?;
        let mut pair_data = Vec::with_capacity(pairs.len());
        for (i, j) in pairs {
            let rho = analysis::reduced_density_matrix(&amps, num_qubits, &[i, j])?;
            pair_data.push(PairDensity {
                qubits: (i, j),
                mutual_information: analysis::mutual_information(&amps, num_qubits, i, j)?,
                concurrence: linalg::concurrence(&rho),
                rho,
            });
        }

        let bloch = analysis::bloch_vectors(&amps, num_qubits)?;
        Ok(Keyframe {
            step: applied,
            gate,
            purity: analysis::purity_from_bloch(&bloch),
            bloch,
            pairs: pair_data,
            top: top_amplitudes(&amps, &gate_qubits, options.top_k),
            probs_top: top_probabilities(&amps, num_qubits, options.probs_top),
        })
    }

    fn pair_list(
        &self,
        selection: &PairSelection,
        gate_qubits: &[usize],
        num_qubits: usize,
    ) -> Result<Vec<(usize, usize)>> {
        let pairs = match selection {
            PairSelection::None => Vec::new(),
            PairSelection::GateQubits => {
                if gate_qubits.len() == 2 {
                    vec![(gate_qubits[0], gate_qubits[1])]
                } else {
                    Vec::new()
                }
            }
            PairSelection::All => {
                let mut all = Vec::new();
                for i in 0..num_qubits {
                    for j in (i + 1)..num_qubits {
                        all.push((i, j));
                    }
                }
                all
            }
            PairSelection::Explicit(list) => list.clone(),
        };
        for (i, j) in &pairs {
            if *i >= num_qubits || *j >= num_qubits || i == j {
                return Err(invalid(format!("invalid qubit pair ({i}, {j})")));
            }
        }
        Ok(pairs)
    }
}

/// `U^t` of a pending gate. A one-angle rotation scales its angle exactly; every
/// other gate goes through the eigen-decomposition of its matrix.
fn fractional_gate(op: &GateOp, t: f64, circuit: &Circuit, source: Option<usize>) -> GateOp {
    if let Some(index) = source {
        if let Some(crate::ir::Operation {
            kind: OpKind::Gate { gate, qubits },
            ..
        }) = circuit.ops().get(index)
        {
            if let Some(scaled) = gate.powered(t) {
                return match scaled.matrix() {
                    Matrix::One(matrix) => GateOp::One {
                        qubit: qubits[0],
                        matrix,
                    },
                    Matrix::Two(matrix) => GateOp::Two {
                        qubits: (qubits[0], qubits[1]),
                        matrix,
                    },
                };
            }
        }
    }
    match op {
        GateOp::One { qubit, matrix } => {
            let powered = linalg::unitary_power(matrix, 2, t);
            GateOp::One {
                qubit: *qubit,
                matrix: [powered[0], powered[1], powered[2], powered[3]],
            }
        }
        GateOp::Two { qubits, matrix } => {
            let powered = linalg::unitary_power(matrix, 4, t);
            let mut m = [Complex64::new(0.0, 0.0); 16];
            m.copy_from_slice(&powered);
            GateOp::Two {
                qubits: *qubits,
                matrix: m,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateInfo {
    pub name: String,
    pub qubits: Vec<usize>,
    pub matrix: Vec<Complex64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairDensity {
    pub qubits: (usize, usize),
    pub rho: Vec<Complex64>,
    pub mutual_information: f64,
    pub concurrence: f64,
}

/// One large amplitude together with the amplitudes the last gate mixed it
/// with, so the browser can interpolate the bars without the full state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AmplitudeGroup {
    pub index: usize,
    pub amplitude: Complex64,
    pub partners: Vec<(usize, Complex64)>,
}

/// One frame of the run view. It crosses to the browser as JSON, so its fields
/// are named the way JavaScript names fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Keyframe {
    pub step: usize,
    pub gate: Option<GateInfo>,
    pub bloch: Vec<[f64; 3]>,
    pub purity: Vec<f64>,
    pub pairs: Vec<PairDensity>,
    pub top: Vec<AmplitudeGroup>,
    pub probs_top: Vec<(String, f64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PairSelection {
    None,
    GateQubits,
    All,
    Explicit(Vec<(usize, usize)>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyframeOptions {
    pub pairs: PairSelection,
    pub top_k: usize,
    pub probs_top: usize,
}

impl Default for KeyframeOptions {
    fn default() -> Self {
        KeyframeOptions {
            pairs: PairSelection::GateQubits,
            top_k: 256,
            probs_top: 16,
        }
    }
}

/// One candidate of the bounded top-K selection, ordered WORST first: the
/// heap's maximum is the entry to drop, so a full pass never materialises or
/// sorts 2^n entries.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Candidate {
    index: usize,
    weight: f64,
}

impl Eq for Candidate {}

impl Ord for Candidate {
    fn cmp(&self, other: &Candidate) -> Ordering {
        other
            .weight
            .total_cmp(&self.weight)
            .then(self.index.cmp(&other.index))
    }
}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Candidate) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The `k` heaviest entries, heaviest first, ties going to the lower index. One
/// pass over the state and memory proportional to `k`, because plan 13.6 budgets
/// a keyframe at a single pass per gate.
fn top_k_by_weight(entries: impl Iterator<Item = (usize, f64)>, k: usize) -> Vec<Candidate> {
    if k == 0 {
        return Vec::new();
    }
    let mut heap: BinaryHeap<Candidate> = BinaryHeap::with_capacity(k);
    for (index, weight) in entries {
        let candidate = Candidate { index, weight };
        if heap.len() < k {
            heap.push(candidate);
        } else if heap.peek().is_some_and(|worst| candidate < *worst) {
            heap.pop();
            heap.push(candidate);
        }
    }
    let mut out = heap.into_vec();
    out.sort_unstable();
    out
}

fn top_amplitudes(amps: &[Complex64], gate_qubits: &[usize], k: usize) -> Vec<AmplitudeGroup> {
    let mut order: Vec<usize> =
        top_k_by_weight(amps.iter().enumerate().map(|(i, a)| (i, a.norm_sqr())), k)
            .into_iter()
            .map(|candidate| candidate.index)
            .collect();
    order.sort_unstable();
    order
        .into_iter()
        .map(|index| AmplitudeGroup {
            index,
            amplitude: amps[index],
            partners: partner_indices(index, gate_qubits)
                .into_iter()
                .map(|p| (p, amps[p]))
                .collect(),
        })
        .collect()
}

fn partner_indices(index: usize, gate_qubits: &[usize]) -> Vec<usize> {
    let mut out = Vec::new();
    let count = 1usize << gate_qubits.len();
    for pattern in 1..count {
        let mut partner = index;
        for (bit, qubit) in gate_qubits.iter().enumerate() {
            if pattern >> bit & 1 == 1 {
                partner ^= 1usize << qubit;
            }
        }
        out.push(partner);
    }
    out
}

fn top_probabilities(amps: &[Complex64], num_qubits: usize, k: usize) -> Vec<(String, f64)> {
    top_k_by_weight(
        amps.iter()
            .enumerate()
            .map(|(i, a)| (i, a.norm_sqr()))
            .filter(|(_, p)| *p > 0.0),
        k,
    )
    .into_iter()
    .map(|candidate| (bitstring(candidate.index, num_qubits), candidate.weight))
    .collect()
}

/// Count key of a classical register image, bit `0` rightmost. The booleans are
/// read directly rather than packed into a word, so a register of any width -
/// the stabilizer path runs thousands of qubits (plan 4.2) - keeps an exact key
/// on a 32-bit target as well as on a 64-bit one.
pub fn bitstring_from_bits(bits: &[bool]) -> String {
    bits.iter()
        .rev()
        .map(|bit| if *bit { '1' } else { '0' })
        .collect()
}

/// Bit `0` of `index` is rendered rightmost, matching the count keys.
pub fn bitstring(index: usize, width: usize) -> String {
    (0..width)
        .rev()
        .map(|bit| if index >> bit & 1 == 1 { '1' } else { '0' })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResult {
    pub counts: BTreeMap<String, u64>,
    pub shots: u64,
}

/// Final state of a circuit that has no measurement, reset or classical guard.
///
/// `cancel` is asked between gates and ends the run with [`Error::Cancelled`].
pub fn statevector(
    circuit: &Circuit,
    options: &SimOptions,
    cancel: Cancel<'_>,
) -> Result<Vec<Complex64>> {
    require_unitary(circuit)?;
    let mut backend = make_backend(circuit.num_qubits(), options)?;
    let program = fuse(&compile(circuit)?);
    apply_program(backend.as_mut(), &program, cancel, |_, _| {
        unreachable!("rejected by require_unitary")
    })?;
    Ok(backend.amplitudes())
}

/// Applies a whole compiled program to `backend`, handing every measurement to
/// `on_measure`. Every straight-line walk of a program goes through here, so
/// the cancellation question is asked at exactly one place: a step is a full
/// pass over the state, so the branch is free next to the work it guards, and a
/// million-gate program stops at the next gate instead of at the next circuit.
///
/// Conditions are not evaluated here — a program with a classical guard is
/// replayed shot by shot through [`Simulator`], which owns the classical bits
/// and asks the same question in its own stepped loop.
fn apply_program(
    backend: &mut dyn Backend,
    program: &[Step],
    cancel: Cancel<'_>,
    mut on_measure: impl FnMut(usize, usize),
) -> Result<()> {
    for step in program {
        if cancel.stopped() {
            return Err(Error::Cancelled);
        }
        match &step.instruction {
            Instruction::Unitary(op) => backend.apply(std::slice::from_ref(op)),
            Instruction::GlobalPhase(angle) => backend.apply_global_phase(*angle),
            Instruction::Barrier => {}
            Instruction::Measure { qubit, clbit } => on_measure(*qubit, *clbit),
            Instruction::Reset { .. } => {
                unreachable!("rejected by require_unitary or by needs_shot_by_shot")
            }
        }
    }
    Ok(())
}

/// Dense unitary of a circuit, column by column. Used to grade kata solutions
/// that must match a target operation on every input, not just on |0...0>.
pub fn circuit_unitary(circuit: &Circuit, options: &SimOptions) -> Result<Vec<Complex64>> {
    require_unitary(circuit)?;
    let num_qubits = circuit.num_qubits();
    let dim = 1usize << num_qubits;
    let program = fuse(&compile(circuit)?);
    let mut out = vec![Complex64::new(0.0, 0.0); dim * dim];
    let mut backend = make_backend(num_qubits, options)?;
    for column in 0..dim {
        set_basis_state(backend.as_mut(), column);
        // Nothing on the server path builds a dense unitary — it grades a kata
        // answer against a reference in the browser, where there is no run to
        // cancel — so this loop is the one that takes no hook.
        apply_program(backend.as_mut(), &program, Cancel::none(), |_, _| {
            unreachable!("rejected by require_unitary")
        })?;
        for (row, value) in backend.amplitudes().into_iter().enumerate() {
            out[row * dim + column] = value;
        }
    }
    Ok(out)
}

/// Reject a circuit that has no single final state vector, naming the construct
/// that took it away. Callers use it as a predicate before offering a state
/// view: a measured circuit is not a broken circuit, it is a circuit to step
/// through instead (plan 13.6).
pub fn require_unitary(circuit: &Circuit) -> Result<()> {
    for op in circuit.ops() {
        if !op.conditions.is_empty() {
            return Err(invalid(
                "a classically guarded circuit has no single state vector; run it with shots",
            ));
        }
        match op.kind {
            OpKind::Measure { .. } => {
                return Err(invalid(
                    "a circuit with measurements has no single state vector; run it with shots",
                ))
            }
            OpKind::Reset { .. } => {
                return Err(invalid(
                    "a circuit with a reset has no single state vector; run it with shots",
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

/// Sample the circuit. Circuits whose final state does not depend on the
/// measurement outcomes are simulated once and sampled from the marginal
/// distribution; everything else (reset, classical control, work after a
/// measurement) is replayed per shot.
///
/// `cancel` is asked between gates and as the shots are consumed, and ends the
/// run with [`Error::Cancelled`]; [`Cancel::none`] runs to the end.
pub fn run(
    circuit: &Circuit,
    options: &SimOptions,
    shots: u64,
    cancel: Cancel<'_>,
) -> Result<RunResult> {
    if circuit.num_clbits() == 0 {
        return Err(invalid("circuit declares no classical bits to sample"));
    }
    if shots == 0 {
        return Err(invalid("a run needs at least one shot"));
    }
    if circuit.needs_shot_by_shot() {
        run_per_shot(circuit, options, shots, cancel)
    } else {
        run_sampled(circuit, options, shots, cancel)
    }
}

/// How many sampled shots are tallied between two questions to `cancel`. One
/// tally is a handful of bit tests, so asking per shot would cost more than the
/// work it guards; a whole batch is still under a millisecond.
const CANCEL_CHECK_SHOTS: usize = 4096;

fn run_sampled(
    circuit: &Circuit,
    options: &SimOptions,
    shots: u64,
    cancel: Cancel<'_>,
) -> Result<RunResult> {
    let num_qubits = circuit.num_qubits();
    let mut backend = make_backend(num_qubits, options)?;
    let program = fuse(&compile(circuit)?);
    let mut assignments: Vec<(usize, usize)> = Vec::new();
    apply_program(backend.as_mut(), &program, cancel, |qubit, clbit| {
        assignments.push((qubit, clbit))
    })?;

    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut clbits = vec![false; circuit.num_clbits()];
    for (tallied, index) in backend
        .sample(&sorted_draws(options.seed, shots))
        .into_iter()
        .enumerate()
    {
        if tallied % CANCEL_CHECK_SHOTS == 0 && cancel.stopped() {
            return Err(Error::Cancelled);
        }
        clbits.iter_mut().for_each(|b| *b = false);
        for (qubit, clbit) in &assignments {
            clbits[*clbit] = index >> qubit & 1 == 1;
        }
        *counts.entry(bitstring_from_bits(&clbits)).or_insert(0) += 1;
    }
    Ok(RunResult { counts, shots })
}

/// Ascending uniform draws in `[0, 1)`, one per shot.
///
/// Sampling is a backend primitive: the draws go down sorted and come back as
/// basis indices, so the host never materialises the full distribution.
fn sorted_draws(seed: u64, shots: u64) -> Vec<f64> {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut draws: Vec<f64> = (0..shots).map(|_| rng.random::<f64>()).collect();
    draws.sort_by(f64::total_cmp);
    draws
}

fn run_per_shot(
    circuit: &Circuit,
    options: &SimOptions,
    shots: u64,
    cancel: Cancel<'_>,
) -> Result<RunResult> {
    let mut simulator = Simulator::new(circuit, options)?;
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    for _ in 0..shots {
        simulator.rewind();
        // The replay is stepped rather than run to the end so the question is
        // asked per gate: one shot of a long circuit is itself unbounded work,
        // and a shot-granular hook would hold a cancelled run for a whole
        // replay. The check leads, so it also covers the shot boundary.
        loop {
            if cancel.stopped() {
                return Err(Error::Cancelled);
            }
            if !simulator.step() {
                break;
            }
        }
        *counts
            .entry(bitstring_from_bits(simulator.clbits()))
            .or_insert(0) += 1;
    }
    Ok(RunResult { counts, shots })
}

fn set_basis_state(backend: &mut dyn Backend, index: usize) {
    backend.reset_to_zero();
    let x = match Gate::X.matrix() {
        Matrix::One(m) => m,
        Matrix::Two(_) => unreachable!("X is a 1-qubit gate"),
    };
    let ops: Vec<GateOp> = (0..backend.num_qubits())
        .filter(|q| index >> q & 1 == 1)
        .map(|qubit| GateOp::One { qubit, matrix: x })
        .collect();
    backend.apply(&ops);
}
