// ===== File: gate.rs — the supported gate set, its matrices and algebraic operations =====

use num_complex::Complex64;
use serde::{Deserialize, Serialize};
use std::f64::consts::{FRAC_1_SQRT_2, PI};

use crate::error::{invalid, Result};

/// Unitary of a gate, row-major, in the |q0 q1> basis where the FIRST operand
/// is the most significant bit of the index.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Matrix {
    One([Complex64; 4]),
    Two([Complex64; 16]),
}

impl Matrix {
    pub fn dim(&self) -> usize {
        match self {
            Matrix::One(_) => 2,
            Matrix::Two(_) => 4,
        }
    }

    pub fn as_slice(&self) -> &[Complex64] {
        match self {
            Matrix::One(m) => m,
            Matrix::Two(m) => m,
        }
    }
}

/// Every gate the IR can carry. `stdgates.inc` entries that are compositions of
/// these (`ccx`, `cswap`, `u1`, `u2`, `u3`, `phase`, `cphase`, `CX`) are lowered
/// into this set while parsing, so the simulator only ever sees 1- and
/// 2-qubit unitaries.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Gate {
    Id,
    X,
    Y,
    Z,
    H,
    S,
    Sdg,
    T,
    Tdg,
    Sx,
    SxDg,
    P(f64),
    Rx(f64),
    Ry(f64),
    Rz(f64),
    U(f64, f64, f64),
    Cx,
    Cy,
    Cz,
    Ch,
    Swap,
    Cp(f64),
    Crx(f64),
    Cry(f64),
    Crz(f64),
    Cu(f64, f64, f64, f64),
}

impl Gate {
    pub fn arity(&self) -> usize {
        use Gate::*;
        match self {
            Id | X | Y | Z | H | S | Sdg | T | Tdg | Sx | SxDg | P(_) | Rx(_) | Ry(_) | Rz(_)
            | U(..) => 1,
            Cx | Cy | Cz | Ch | Swap | Cp(_) | Crx(_) | Cry(_) | Crz(_) | Cu(..) => 2,
        }
    }

    /// Name under which the gate is emitted in canonical OpenQASM 3.
    /// `SxDg` has no `stdgates.inc` name and is emitted as `inv @ sx`.
    pub fn qasm_name(&self) -> &'static str {
        use Gate::*;
        match self {
            Id => "id",
            X => "x",
            Y => "y",
            Z => "z",
            H => "h",
            S => "s",
            Sdg => "sdg",
            T => "t",
            Tdg => "tdg",
            Sx => "sx",
            SxDg => "sx",
            P(_) => "p",
            Rx(_) => "rx",
            Ry(_) => "ry",
            Rz(_) => "rz",
            U(..) => "U",
            Cx => "cx",
            Cy => "cy",
            Cz => "cz",
            Ch => "ch",
            Swap => "swap",
            Cp(_) => "cp",
            Crx(_) => "crx",
            Cry(_) => "cry",
            Crz(_) => "crz",
            Cu(..) => "cu",
        }
    }

    pub fn params(&self) -> Vec<f64> {
        use Gate::*;
        match *self {
            P(a) | Rx(a) | Ry(a) | Rz(a) | Cp(a) | Crx(a) | Cry(a) | Crz(a) => vec![a],
            U(a, b, c) => vec![a, b, c],
            Cu(a, b, c, d) => vec![a, b, c, d],
            _ => Vec::new(),
        }
    }

    /// Every angle must be finite: a non-finite one would be emitted as `inf`
    /// and could never be parsed back.
    pub fn validate(&self) -> Result<()> {
        if self.params().iter().all(|p| p.is_finite()) {
            Ok(())
        } else {
            Err(invalid(format!(
                "gate `{}` has a non-finite parameter",
                self.qasm_name()
            )))
        }
    }

    pub fn matrix(&self) -> Matrix {
        use Gate::*;
        let c = |re: f64, im: f64| Complex64::new(re, im);
        let zero = c(0.0, 0.0);
        let one = c(1.0, 0.0);
        match *self {
            Id => Matrix::One([one, zero, zero, one]),
            X => Matrix::One([zero, one, one, zero]),
            Y => Matrix::One([zero, c(0.0, -1.0), c(0.0, 1.0), zero]),
            Z => Matrix::One([one, zero, zero, -one]),
            H => Matrix::One([
                c(FRAC_1_SQRT_2, 0.0),
                c(FRAC_1_SQRT_2, 0.0),
                c(FRAC_1_SQRT_2, 0.0),
                c(-FRAC_1_SQRT_2, 0.0),
            ]),
            S => Matrix::One([one, zero, zero, c(0.0, 1.0)]),
            Sdg => Matrix::One([one, zero, zero, c(0.0, -1.0)]),
            T => Matrix::One([one, zero, zero, phase_factor(PI / 4.0)]),
            Tdg => Matrix::One([one, zero, zero, phase_factor(-PI / 4.0)]),
            Sx => Matrix::One([c(0.5, 0.5), c(0.5, -0.5), c(0.5, -0.5), c(0.5, 0.5)]),
            SxDg => Matrix::One([c(0.5, -0.5), c(0.5, 0.5), c(0.5, 0.5), c(0.5, -0.5)]),
            P(lam) => Matrix::One([one, zero, zero, phase_factor(lam)]),
            Rx(theta) => {
                let (s, co) = ((theta / 2.0).sin(), (theta / 2.0).cos());
                Matrix::One([c(co, 0.0), c(0.0, -s), c(0.0, -s), c(co, 0.0)])
            }
            Ry(theta) => {
                let (s, co) = ((theta / 2.0).sin(), (theta / 2.0).cos());
                Matrix::One([c(co, 0.0), c(-s, 0.0), c(s, 0.0), c(co, 0.0)])
            }
            Rz(lam) => Matrix::One([
                phase_factor(-lam / 2.0),
                zero,
                zero,
                phase_factor(lam / 2.0),
            ]),
            U(theta, phi, lam) => Matrix::One(u_matrix(theta, phi, lam)),
            Cx => controlled([zero, one, one, zero]),
            Cy => controlled([zero, c(0.0, -1.0), c(0.0, 1.0), zero]),
            Cz => controlled([one, zero, zero, -one]),
            Ch => controlled([
                c(FRAC_1_SQRT_2, 0.0),
                c(FRAC_1_SQRT_2, 0.0),
                c(FRAC_1_SQRT_2, 0.0),
                c(-FRAC_1_SQRT_2, 0.0),
            ]),
            Swap => Matrix::Two([
                one, zero, zero, zero, //
                zero, zero, one, zero, //
                zero, one, zero, zero, //
                zero, zero, zero, one,
            ]),
            Cp(lam) => controlled([one, zero, zero, phase_factor(lam)]),
            Crx(theta) => match Gate::Rx(theta).matrix() {
                Matrix::One(m) => controlled(m),
                Matrix::Two(_) => unreachable!("Rx is a 1-qubit gate"),
            },
            Cry(theta) => match Gate::Ry(theta).matrix() {
                Matrix::One(m) => controlled(m),
                Matrix::Two(_) => unreachable!("Ry is a 1-qubit gate"),
            },
            Crz(lam) => match Gate::Rz(lam).matrix() {
                Matrix::One(m) => controlled(m),
                Matrix::Two(_) => unreachable!("Rz is a 1-qubit gate"),
            },
            Cu(theta, phi, lam, gamma) => {
                let g = phase_factor(gamma);
                let u = u_matrix(theta, phi, lam);
                controlled([u[0] * g, u[1] * g, u[2] * g, u[3] * g])
            }
        }
    }

    /// The inverse gate. Every gate in the set has one inside the set, which is
    /// what makes `inv @` expressible without falling back to a raw matrix.
    pub fn adjoint(&self) -> Gate {
        use Gate::*;
        match *self {
            Id => Id,
            X => X,
            Y => Y,
            Z => Z,
            H => H,
            S => Sdg,
            Sdg => S,
            T => Tdg,
            Tdg => T,
            Sx => SxDg,
            SxDg => Sx,
            P(a) => P(-a),
            Rx(a) => Rx(-a),
            Ry(a) => Ry(-a),
            Rz(a) => Rz(-a),
            U(t, p, l) => U(-t, -l, -p),
            Cx => Cx,
            Cy => Cy,
            Cz => Cz,
            Ch => Ch,
            Swap => Swap,
            Cp(a) => Cp(-a),
            Crx(a) => Crx(-a),
            Cry(a) => Cry(-a),
            Crz(a) => Crz(-a),
            Cu(t, p, l, g) => Cu(-t, -l, -p, -g),
        }
    }

    /// Controlled version of a 1-qubit gate, when the result is still inside the
    /// gate set. `None` means the caller has to reject `ctrl @` on this gate.
    /// `Id` is not in the map on purpose: a controlled identity is the identity
    /// and produces no two-qubit gate at all.
    pub fn controlled(&self) -> Option<Gate> {
        use Gate::*;
        match *self {
            X => Some(Cx),
            Y => Some(Cy),
            Z => Some(Cz),
            H => Some(Ch),
            S => Some(Cp(PI / 2.0)),
            Sdg => Some(Cp(-PI / 2.0)),
            T => Some(Cp(PI / 4.0)),
            Tdg => Some(Cp(-PI / 4.0)),
            P(a) => Some(Cp(a)),
            Rx(a) => Some(Crx(a)),
            Ry(a) => Some(Cry(a)),
            Rz(a) => Some(Crz(a)),
            U(t, p, l) => Some(Cu(t, p, l, 0.0)),
            _ => None,
        }
    }

    /// `U^t` when the gate is a one-angle rotation, where the fractional power is
    /// exactly the same gate with a scaled angle. Gates outside this family go
    /// through the eigendecomposition in `linalg::unitary_power`.
    pub fn powered(&self, t: f64) -> Option<Gate> {
        use Gate::*;
        match *self {
            Id => Some(Id),
            P(a) => Some(P(a * t)),
            Rx(a) => Some(Rx(a * t)),
            Ry(a) => Some(Ry(a * t)),
            Rz(a) => Some(Rz(a * t)),
            Cp(a) => Some(Cp(a * t)),
            Crx(a) => Some(Crx(a * t)),
            Cry(a) => Some(Cry(a * t)),
            Crz(a) => Some(Crz(a * t)),
            _ => None,
        }
    }

    /// Conservative Clifford test: `true` guarantees the stabilizer tableau can
    /// run the gate, `false` only means this crate will not try.
    pub fn is_clifford(&self) -> bool {
        use Gate::*;
        match *self {
            Id | X | Y | Z | H | S | Sdg | Sx | SxDg | Cx | Cy | Cz | Swap => true,
            P(a) | Rz(a) | Rx(a) | Ry(a) => is_multiple_of(a, PI / 2.0),
            Cp(a) => is_multiple_of(a, PI),
            _ => false,
        }
    }

    /// Integer power, used for `pow(k) @ g`. Only exact integers are accepted;
    /// `pow(0.5) @ x` would need a gate outside the set.
    pub fn integer_power(&self, exponent: i64) -> Result<Vec<Gate>> {
        if let Some(scaled) = self.powered(exponent as f64) {
            return Ok(vec![scaled]);
        }
        let base = if exponent < 0 { self.adjoint() } else { *self };
        // Compared as u64: `usize` is 32-bit on wasm32 and a huge exponent would
        // wrap into a small repetition count instead of being refused.
        let count = exponent.unsigned_abs();
        if count > MAX_GATE_POWER as u64 {
            return Err(invalid(format!(
                "pow({exponent}) @ {} would expand to more than {MAX_GATE_POWER} gates",
                self.qasm_name()
            )));
        }
        Ok(vec![base; count as usize])
    }
}

/// `pow(k) @ g` on a non-rotation gate is expanded by repetition; a huge
/// exponent would blow up the IR instead of failing, so it is capped. The
/// OpenQASM front end applies the same cap to a multi-gate expansion.
pub(crate) const MAX_GATE_POWER: usize = 1024;

fn is_multiple_of(value: f64, step: f64) -> bool {
    let k = value / step;
    (k - k.round()).abs() < 1e-9
}

fn phase_factor(angle: f64) -> Complex64 {
    Complex64::new(angle.cos(), angle.sin())
}

fn u_matrix(theta: f64, phi: f64, lam: f64) -> [Complex64; 4] {
    let (s, c) = ((theta / 2.0).sin(), (theta / 2.0).cos());
    [
        Complex64::new(c, 0.0),
        -phase_factor(lam) * s,
        phase_factor(phi) * s,
        phase_factor(phi + lam) * c,
    ]
}

fn controlled(target: [Complex64; 4]) -> Matrix {
    let zero = Complex64::new(0.0, 0.0);
    let one = Complex64::new(1.0, 0.0);
    Matrix::Two([
        one, zero, zero, zero, //
        zero, one, zero, zero, //
        zero, zero, target[0], target[1], //
        zero, zero, target[2], target[3],
    ])
}

/// One `stdgates.inc` call expanded into the IR gate set.
///
/// `ops` pairs a gate with the positions it takes in the ORIGINAL qubit operand
/// list of the call, so `ccx a, b, c` can expand into `cx`/`t`/`h` on operands
/// 0, 1 and 2. `global_phase` carries the `gphase` that `stdgates.inc` writes
/// explicitly for `u2`/`u3`.
#[derive(Debug, Clone, PartialEq)]
pub struct GateExpansion {
    pub global_phase: f64,
    pub ops: Vec<(Gate, Vec<usize>)>,
}

impl GateExpansion {
    fn plain(ops: Vec<(Gate, Vec<usize>)>) -> GateExpansion {
        GateExpansion {
            global_phase: 0.0,
            ops,
        }
    }
}

/// Resolve a `stdgates.inc` (or builtin) gate name plus its arguments into the
/// IR gate set.
pub fn resolve_named_gate(name: &str, params: &[f64], qubits: usize) -> Result<GateExpansion> {
    let expect = |n_params: usize, n_qubits: usize| -> Result<()> {
        if params.len() != n_params || qubits != n_qubits {
            return Err(invalid(format!(
                "gate `{name}` takes {n_params} parameter(s) and {n_qubits} qubit(s), got {} and {qubits}",
                params.len()
            )));
        }
        Ok(())
    };
    let single =
        |g: Gate| -> Result<GateExpansion> { Ok(GateExpansion::plain(vec![(g, vec![0])])) };
    let pair =
        |g: Gate| -> Result<GateExpansion> { Ok(GateExpansion::plain(vec![(g, vec![0, 1])])) };

    match name {
        "id" => {
            expect(0, 1)?;
            single(Gate::Id)
        }
        "x" => {
            expect(0, 1)?;
            single(Gate::X)
        }
        "y" => {
            expect(0, 1)?;
            single(Gate::Y)
        }
        "z" => {
            expect(0, 1)?;
            single(Gate::Z)
        }
        "h" => {
            expect(0, 1)?;
            single(Gate::H)
        }
        "s" => {
            expect(0, 1)?;
            single(Gate::S)
        }
        "sdg" => {
            expect(0, 1)?;
            single(Gate::Sdg)
        }
        "t" => {
            expect(0, 1)?;
            single(Gate::T)
        }
        "tdg" => {
            expect(0, 1)?;
            single(Gate::Tdg)
        }
        "sx" => {
            expect(0, 1)?;
            single(Gate::Sx)
        }
        "p" | "phase" | "u1" => {
            expect(1, 1)?;
            single(Gate::P(params[0]))
        }
        "rx" => {
            expect(1, 1)?;
            single(Gate::Rx(params[0]))
        }
        "ry" => {
            expect(1, 1)?;
            single(Gate::Ry(params[0]))
        }
        "rz" => {
            expect(1, 1)?;
            single(Gate::Rz(params[0]))
        }
        "U" => {
            expect(3, 1)?;
            single(Gate::U(params[0], params[1], params[2]))
        }
        "cx" | "CX" => {
            expect(0, 2)?;
            pair(Gate::Cx)
        }
        "cy" => {
            expect(0, 2)?;
            pair(Gate::Cy)
        }
        "cz" => {
            expect(0, 2)?;
            pair(Gate::Cz)
        }
        "ch" => {
            expect(0, 2)?;
            pair(Gate::Ch)
        }
        "swap" => {
            expect(0, 2)?;
            pair(Gate::Swap)
        }
        "cp" | "cphase" => {
            expect(1, 2)?;
            pair(Gate::Cp(params[0]))
        }
        "crx" => {
            expect(1, 2)?;
            pair(Gate::Crx(params[0]))
        }
        "cry" => {
            expect(1, 2)?;
            pair(Gate::Cry(params[0]))
        }
        "crz" => {
            expect(1, 2)?;
            pair(Gate::Crz(params[0]))
        }
        "cu" => {
            expect(4, 2)?;
            pair(Gate::Cu(params[0], params[1], params[2], params[3]))
        }
        // u2/u3 carry a global phase relative to the builtin U; stdgates.inc
        // spells that out with an explicit `gphase`, and so does the IR.
        "u2" => {
            expect(2, 1)?;
            Ok(GateExpansion {
                global_phase: -(params[0] + params[1]) / 2.0,
                ops: vec![(Gate::U(PI / 2.0, params[0], params[1]), vec![0])],
            })
        }
        "u3" => {
            expect(3, 1)?;
            Ok(GateExpansion {
                global_phase: -(params[1] + params[2]) / 2.0,
                ops: vec![(Gate::U(params[0], params[1], params[2]), vec![0])],
            })
        }
        "ccx" => {
            expect(0, 3)?;
            Ok(GateExpansion::plain(toffoli_decomposition()))
        }
        "cswap" => {
            expect(0, 3)?;
            let mut ops = vec![(Gate::Cx, vec![2, 1])];
            ops.extend(toffoli_decomposition());
            ops.push((Gate::Cx, vec![2, 1]));
            Ok(GateExpansion::plain(ops))
        }
        other => Err(invalid(format!("unknown gate `{other}`"))),
    }
}

/// The `stdgates.inc` decomposition of `ccx a, b, c` into `h`, `cx`, `t`, `tdg`.
fn toffoli_decomposition() -> Vec<(Gate, Vec<usize>)> {
    vec![
        (Gate::H, vec![2]),
        (Gate::Cx, vec![1, 2]),
        (Gate::Tdg, vec![2]),
        (Gate::Cx, vec![0, 2]),
        (Gate::T, vec![2]),
        (Gate::Cx, vec![1, 2]),
        (Gate::Tdg, vec![2]),
        (Gate::Cx, vec![0, 2]),
        (Gate::T, vec![1]),
        (Gate::T, vec![2]),
        (Gate::H, vec![2]),
        (Gate::Cx, vec![0, 1]),
        (Gate::T, vec![0]),
        (Gate::Tdg, vec![1]),
        (Gate::Cx, vec![0, 1]),
    ]
}
