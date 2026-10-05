// ===== File: sim/stabilizer.rs — Aaronson-Gottesman tableau for Clifford circuits =====
//
// A Clifford circuit needs O(n^2) bits instead of 2^n amplitudes, which is what
// lets the circuit editor offer "this circuit is Clifford, thousands of qubits
// are fine" (plan 4.2).

use std::collections::BTreeMap;

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use super::statevector::{bitstring_from_bits, RunResult, SimOptions};
use super::Cancel;
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
    /// 64-bit words per tableau row.
    words: usize,
    /// `x`, `z` and `r` hold 2n + 1 rows: n destabilizers, n stabilizers and one
    /// scratch row used by deterministic measurements. A row of `x`/`z` is
    /// packed 64 qubits to a word, with the padding bits of the last word kept
    /// at zero — `rowsum` is the hot path of every measurement and works a
    /// word at a time, which is what keeps a thousand-qubit tableau usable.
    x: Vec<u64>,
    z: Vec<u64>,
    r: Vec<bool>,
    rng: StdRng,
}

impl StabilizerSim {
    pub fn new(num_qubits: usize, seed: u64) -> StabilizerSim {
        let rows = 2 * num_qubits + 1;
        let words = num_qubits.div_ceil(64).max(1);
        let mut sim = StabilizerSim {
            num_qubits,
            words,
            x: vec![0; rows * words],
            z: vec![0; rows * words],
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
        self.x.iter_mut().for_each(|w| *w = 0);
        self.z.iter_mut().for_each(|w| *w = 0);
        self.r.iter_mut().for_each(|b| *b = false);
        let n = self.num_qubits;
        for i in 0..n {
            self.set_bit(false, i, i, true);
            self.set_bit(true, n + i, i, true);
        }
    }

    fn bit(&self, z: bool, row: usize, qubit: usize) -> bool {
        let words = if z { &self.z } else { &self.x };
        words[row * self.words + qubit / 64] >> (qubit % 64) & 1 == 1
    }

    fn set_bit(&mut self, z: bool, row: usize, qubit: usize, value: bool) {
        let words = if z { &mut self.z } else { &mut self.x };
        let word = &mut words[row * self.words + qubit / 64];
        let mask = 1u64 << (qubit % 64);
        if value {
            *word |= mask;
        } else {
            *word &= !mask;
        }
    }

    fn h(&mut self, a: usize) {
        for i in 0..(2 * self.num_qubits) {
            let xi = self.bit(false, i, a);
            let zi = self.bit(true, i, a);
            self.r[i] ^= xi & zi;
            self.set_bit(false, i, a, zi);
            self.set_bit(true, i, a, xi);
        }
    }

    fn s(&mut self, a: usize) {
        for i in 0..(2 * self.num_qubits) {
            let xi = self.bit(false, i, a);
            let zi = self.bit(true, i, a);
            self.r[i] ^= xi & zi;
            self.set_bit(true, i, a, zi ^ xi);
        }
    }

    fn cx(&mut self, a: usize, b: usize) {
        for i in 0..(2 * self.num_qubits) {
            let xa = self.bit(false, i, a);
            let xb = self.bit(false, i, b);
            let za = self.bit(true, i, a);
            let zb = self.bit(true, i, b);
            self.r[i] ^= xa & zb & (xb ^ za ^ true);
            self.set_bit(false, i, b, xb ^ xa);
            self.set_bit(true, i, a, za ^ zb);
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
    /// original Aaronson-Gottesman paper. The per-qubit phase function g is
    /// evaluated for 64 qubits at once: the qubits where it is +1 and where it
    /// is -1 are counted with popcounts.
    fn rowsum(&mut self, h: usize, i: usize) {
        let words = self.words;
        let (hs, is) = (h * words, i * words);
        let mut plus: i64 = 0;
        let mut minus: i64 = 0;
        for w in 0..words {
            let (x1, z1) = (self.x[is + w], self.z[is + w]);
            let (x2, z2) = (self.x[hs + w], self.z[hs + w]);
            let y = x1 & z1;
            let px = x1 & !z1;
            let pz = !x1 & z1;
            plus += i64::from((y & !x2 & z2).count_ones())
                + i64::from((px & x2 & z2).count_ones())
                + i64::from((pz & x2 & !z2).count_ones());
            minus += i64::from((y & x2 & !z2).count_ones())
                + i64::from((px & !x2 & z2).count_ones())
                + i64::from((pz & x2 & z2).count_ones());
        }
        let total =
            (2 * i64::from(self.r[h]) + 2 * i64::from(self.r[i]) + plus - minus).rem_euclid(4);
        self.r[h] = total == 2;
        for w in 0..words {
            self.x[hs + w] ^= self.x[is + w];
            self.z[hs + w] ^= self.z[is + w];
        }
    }

    fn copy_row(&mut self, to: usize, from: usize) {
        let words = self.words;
        self.x
            .copy_within(from * words..(from + 1) * words, to * words);
        self.z
            .copy_within(from * words..(from + 1) * words, to * words);
    }

    fn clear_row(&mut self, row: usize) {
        let words = self.words;
        self.x[row * words..(row + 1) * words].fill(0);
        self.z[row * words..(row + 1) * words].fill(0);
    }

    pub fn measure(&mut self, a: usize) -> bool {
        let n = self.num_qubits;
        let pivot = (n..(2 * n)).find(|i| self.bit(false, *i, a));
        match pivot {
            Some(p) => {
                for i in 0..(2 * n) {
                    if i != p && self.bit(false, i, a) {
                        self.rowsum(i, p);
                    }
                }
                self.copy_row(p - n, p);
                self.clear_row(p);
                self.r[p - n] = self.r[p];
                self.set_bit(true, p, a, true);
                let outcome = self.rng.random::<bool>();
                self.r[p] = outcome;
                outcome
            }
            None => {
                let scratch = 2 * n;
                self.clear_row(scratch);
                self.r[scratch] = false;
                for i in 0..n {
                    if self.bit(false, i, a) {
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
///
/// `cancel` is asked between gates and between shots, and ends the run with
/// [`Error::Cancelled`]; [`Cancel::none`] runs to the end.
pub fn run(
    circuit: &Circuit,
    options: &SimOptions,
    shots: u64,
    cancel: Cancel<'_>,
) -> Result<RunResult> {
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
        // Asked on both loops: a tableau op is O(n^2) and a program may carry
        // a million of them, while a program with no gates still pays an
        // O(n^2) reset per shot.
        if cancel.stopped() {
            return Err(Error::Cancelled);
        }
        sim.reset_to_zero();
        clbits.iter_mut().for_each(|b| *b = false);
        for op in circuit.ops() {
            if cancel.stopped() {
                return Err(Error::Cancelled);
            }
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
