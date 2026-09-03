// ===== File: sim/stabilizer.rs — Aaronson-Gottesman tableau for Clifford circuits =====
//
// A Clifford circuit needs O(n^2) bits instead of 2^n amplitudes, which is what
// lets the circuit editor offer "this circuit is Clifford, thousands of qubits
// are fine" (plan 4.2).

use std::collections::BTreeMap;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use super::statevector::{bitstring_from_bits, RunResult, SimOptions};
use crate::error::{invalid, Error, Result};
use crate::gate::Gate;
use crate::ir::{Circuit, OpKind};

/// Elementary Clifford operations the tableau implements directly. Everything
/// else in the gate set is expressed as a sequence of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Primitive {
    H(usize),
    S(usize),
    Cx(usize, usize),
}

pub struct StabilizerSim {
    num_qubits: usize,
    /// `x`, `z` and `r` hold 2n + 1 rows: n destabilizers, n stabilizers and one
    /// scratch row used by deterministic measurements.
    x: Vec<bool>,
    z: Vec<bool>,
    r: Vec<bool>,
    rng: StdRng,
}

impl StabilizerSim {
    pub fn new(num_qubits: usize, seed: u64) -> StabilizerSim {
        let rows = 2 * num_qubits + 1;
        let mut sim = StabilizerSim {
            num_qubits,
            x: vec![false; rows * num_qubits],
            z: vec![false; rows * num_qubits],
            r: vec![false; rows],
            rng: StdRng::seed_from_u64(seed),
        };
        sim.reset_to_zero();
        sim
    }

    pub fn num_qubits(&self) -> usize {
        self.num_qubits
    }

    pub fn reset_to_zero(&mut self) {
        self.x.iter_mut().for_each(|b| *b = false);
        self.z.iter_mut().for_each(|b| *b = false);
        self.r.iter_mut().for_each(|b| *b = false);
        let n = self.num_qubits;
        for i in 0..n {
            self.x[i * n + i] = true;
            self.z[(n + i) * n + i] = true;
        }
    }

    fn h(&mut self, a: usize) {
        let n = self.num_qubits;
        for i in 0..(2 * n) {
            let xi = self.x[i * n + a];
            let zi = self.z[i * n + a];
            self.r[i] ^= xi & zi;
            self.x[i * n + a] = zi;
            self.z[i * n + a] = xi;
        }
    }

    fn s(&mut self, a: usize) {
        let n = self.num_qubits;
        for i in 0..(2 * n) {
            let xi = self.x[i * n + a];
            let zi = self.z[i * n + a];
            self.r[i] ^= xi & zi;
            self.z[i * n + a] = zi ^ xi;
        }
    }

    fn cx(&mut self, a: usize, b: usize) {
        let n = self.num_qubits;
        for i in 0..(2 * n) {
            let xa = self.x[i * n + a];
            let xb = self.x[i * n + b];
            let za = self.z[i * n + a];
            let zb = self.z[i * n + b];
            self.r[i] ^= xa & zb & (xb ^ za ^ true);
            self.x[i * n + b] = xb ^ xa;
            self.z[i * n + a] = za ^ zb;
        }
    }

    fn apply_primitive(&mut self, primitive: Primitive) {
        match primitive {
            Primitive::H(a) => self.h(a),
            Primitive::S(a) => self.s(a),
            Primitive::Cx(a, b) => self.cx(a, b),
        }
    }

    /// Accumulate row `i` onto row `h`, tracking the phase in Z4 as in the
    /// original Aaronson-Gottesman paper.
    fn rowsum(&mut self, h: usize, i: usize) {
        let n = self.num_qubits;
        let mut total: i32 = 2 * i32::from(self.r[h]) + 2 * i32::from(self.r[i]);
        for j in 0..n {
            total += g(
                self.x[i * n + j],
                self.z[i * n + j],
                self.x[h * n + j],
                self.z[h * n + j],
            );
        }
        total = total.rem_euclid(4);
        self.r[h] = total == 2;
        for j in 0..n {
            self.x[h * n + j] ^= self.x[i * n + j];
            self.z[h * n + j] ^= self.z[i * n + j];
        }
    }

    pub fn measure(&mut self, a: usize) -> bool {
        let n = self.num_qubits;
        let pivot = (n..(2 * n)).find(|i| self.x[i * n + a]);
        match pivot {
            Some(p) => {
                for i in 0..(2 * n) {
                    if i != p && self.x[i * n + a] {
                        self.rowsum(i, p);
                    }
                }
                for j in 0..n {
                    self.x[(p - n) * n + j] = self.x[p * n + j];
                    self.z[(p - n) * n + j] = self.z[p * n + j];
                    self.x[p * n + j] = false;
                    self.z[p * n + j] = false;
                }
                self.r[p - n] = self.r[p];
                self.z[p * n + a] = true;
                let outcome = self.rng.random::<bool>();
                self.r[p] = outcome;
                outcome
            }
            None => {
                let scratch = 2 * n;
                for j in 0..n {
                    self.x[scratch * n + j] = false;
                    self.z[scratch * n + j] = false;
                }
                self.r[scratch] = false;
                for i in 0..n {
                    if self.x[i * n + a] {
                        self.rowsum(scratch, i + n);
                    }
                }
                self.r[scratch]
            }
        }
    }

    pub fn reset(&mut self, a: usize) {
        if self.measure(a) {
            for primitive in x_primitives(a) {
                self.apply_primitive(primitive);
            }
        }
    }

    /// Run one gate of the IR gate set. Returns `NotClifford` for anything the
    /// tableau cannot represent, which is the same predicate `Gate::is_clifford`
    /// reports.
    pub fn apply_gate(&mut self, gate: Gate, qubits: &[usize]) -> Result<()> {
        for primitive in clifford_primitives(gate, qubits)? {
            self.apply_primitive(primitive);
        }
        Ok(())
    }
}

fn g(x1: bool, z1: bool, x2: bool, z2: bool) -> i32 {
    let (x2, z2) = (i32::from(x2), i32::from(z2));
    match (x1, z1) {
        (false, false) => 0,
        (true, true) => z2 - x2,
        (true, false) => z2 * (2 * x2 - 1),
        (false, true) => x2 * (1 - 2 * z2),
    }
}

fn x_primitives(a: usize) -> Vec<Primitive> {
    // X = H Z H and Z = S S.
    vec![
        Primitive::H(a),
        Primitive::S(a),
        Primitive::S(a),
        Primitive::H(a),
    ]
}

/// Number of `S` gates equivalent to a rotation angle, or `None` when the angle
/// is not a multiple of pi/2.
fn s_power(angle: f64, step: f64) -> Option<i64> {
    let k = angle / step;
    if (k - k.round()).abs() > 1e-9 {
        return None;
    }
    Some(k.round() as i64)
}

fn repeat_s(a: usize, count: i64, modulus: i64) -> Vec<Primitive> {
    let times = count.rem_euclid(modulus);
    (0..times).map(|_| Primitive::S(a)).collect()
}

/// Decompose a Clifford gate into `h`, `s` and `cx`.
///
/// Global phases are dropped: conjugation by `U` and by `c * U` with `|c| = 1`
/// is the same channel, so the tableau is unaffected.
fn clifford_primitives(gate: Gate, qubits: &[usize]) -> Result<Vec<Primitive>> {
    use Gate::*;
    let not_clifford = |gate: Gate| {
        Err(Error::NotClifford {
            reason: format!("gate `{}` is not Clifford", gate.qasm_name()),
        })
    };
    if qubits.len() != gate.arity() {
        return Err(invalid(format!(
            "gate `{}` takes {} qubit(s)",
            gate.qasm_name(),
            gate.arity()
        )));
    }
    let a = qubits[0];
    Ok(match gate {
        Id => Vec::new(),
        X => x_primitives(a),
        // Z then X differs from Y only by a global phase.
        Y => {
            let mut ops = vec![Primitive::S(a), Primitive::S(a)];
            ops.extend(x_primitives(a));
            ops
        }
        Z => vec![Primitive::S(a), Primitive::S(a)],
        H => vec![Primitive::H(a)],
        S => vec![Primitive::S(a)],
        Sdg => vec![Primitive::S(a), Primitive::S(a), Primitive::S(a)],
        Sx => vec![Primitive::H(a), Primitive::S(a), Primitive::H(a)],
        SxDg => vec![
            Primitive::H(a),
            Primitive::S(a),
            Primitive::S(a),
            Primitive::S(a),
            Primitive::H(a),
        ],
        P(angle) | Rz(angle) => match s_power(angle, std::f64::consts::FRAC_PI_2) {
            Some(k) => repeat_s(a, k, 4),
            None => return not_clifford(gate),
        },
        Rx(angle) => match s_power(angle, std::f64::consts::FRAC_PI_2) {
            Some(k) => {
                let mut ops = vec![Primitive::H(a)];
                ops.extend(repeat_s(a, k, 4));
                ops.push(Primitive::H(a));
                ops
            }
            None => return not_clifford(gate),
        },
        // ry = S rx S^dagger, so the state sees S^dagger first.
        Ry(angle) => match s_power(angle, std::f64::consts::FRAC_PI_2) {
            Some(k) => {
                let mut ops = vec![
                    Primitive::S(a),
                    Primitive::S(a),
                    Primitive::S(a),
                    Primitive::H(a),
                ];
                ops.extend(repeat_s(a, k, 4));
                ops.push(Primitive::H(a));
                ops.push(Primitive::S(a));
                ops
            }
            None => return not_clifford(gate),
        },
        Cx => vec![Primitive::Cx(a, qubits[1])],
        Cz => vec![
            Primitive::H(qubits[1]),
            Primitive::Cx(a, qubits[1]),
            Primitive::H(qubits[1]),
        ],
        // cy = (I (x) S) cx (I (x) S^dagger)
        Cy => vec![
            Primitive::S(qubits[1]),
            Primitive::S(qubits[1]),
            Primitive::S(qubits[1]),
            Primitive::Cx(a, qubits[1]),
            Primitive::S(qubits[1]),
        ],
        Swap => vec![
            Primitive::Cx(a, qubits[1]),
            Primitive::Cx(qubits[1], a),
            Primitive::Cx(a, qubits[1]),
        ],
        Cp(angle) => match s_power(angle, std::f64::consts::PI) {
            Some(k) if k.rem_euclid(2) == 0 => Vec::new(),
            Some(_) => vec![
                Primitive::H(qubits[1]),
                Primitive::Cx(a, qubits[1]),
                Primitive::H(qubits[1]),
            ],
            None => return not_clifford(gate),
        },
        T | Tdg | U(..) | Ch | Crx(_) | Cry(_) | Crz(_) | Cu(..) => return not_clifford(gate),
    })
}

/// Sample a Clifford circuit with the tableau. Every shot is a fresh replay, so
/// mid-circuit measurement and classical control behave exactly as in the state
/// vector simulator.
pub fn run(circuit: &Circuit, options: &SimOptions, shots: u64) -> Result<RunResult> {
    if !circuit.is_clifford() {
        return Err(Error::NotClifford {
            reason: "circuit contains a non-Clifford gate".to_string(),
        });
    }
    if circuit.num_clbits() == 0 {
        return Err(invalid("circuit declares no classical bits to sample"));
    }
    if shots == 0 {
        return Err(invalid("a run needs at least one shot"));
    }
    let width = circuit.num_clbits();
    let mut sim = StabilizerSim::new(circuit.num_qubits(), options.seed);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut clbits = vec![false; width];
    for _ in 0..shots {
        sim.reset_to_zero();
        clbits.iter_mut().for_each(|b| *b = false);
        for op in circuit.ops() {
            if !circuit.conditions_hold(&op.conditions, &clbits) {
                continue;
            }
            match &op.kind {
                OpKind::Gate { gate, qubits } => sim.apply_gate(*gate, qubits)?,
                OpKind::GlobalPhase(_) | OpKind::Barrier { .. } => {}
                OpKind::Measure { qubit, clbit } => clbits[*clbit] = sim.measure(*qubit),
                OpKind::Reset { qubit } => sim.reset(*qubit),
            }
        }
        *counts.entry(bitstring_from_bits(&clbits)).or_insert(0) += 1;
    }
    Ok(RunResult { counts, shots })
}
