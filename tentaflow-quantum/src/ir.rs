// ===== File: ir.rs — circuit intermediate representation and canonical OpenQASM 3 emission =====

use serde::{Deserialize, Serialize};

use crate::error::{invalid, Result};
use crate::gate::Gate;

/// Widest classical register a guard may compare against: `Condition::Register`
/// holds the compared value in a `u64`, and a wider register would have to be
/// compared on a truncated image, silently changing what the program means.
pub const MAX_CONDITION_REGISTER_BITS: usize = 64;

/// A named register. `start` is the index of its first bit in the flat qubit or
/// clbit space that every operation addresses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Register {
    pub name: String,
    pub start: usize,
    pub size: usize,
}

/// A classical guard on one operation.
///
/// Bit order inside a register is little-endian: `c[0]` is the least significant
/// bit of `Condition::Register::value`, matching what Qiskit does, and matching
/// how `RunResult` renders count keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Condition {
    Bit {
        clbit: usize,
        value: bool,
    },
    Register {
        register: usize,
        value: u64,
        equal: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum OpKind {
    Gate { gate: Gate, qubits: Vec<usize> },
    GlobalPhase(f64),
    Measure { qubit: usize, clbit: usize },
    Reset { qubit: usize },
    Barrier { qubits: Vec<usize> },
}

/// One instruction. `conditions` is a conjunction; an empty list is
/// unconditional. Nested `if` blocks in the source become several entries, and
/// an `else` branch becomes the negated condition.
///
/// The guard is re-read before every operation, which matches OpenQASM 3 (where
/// the condition is evaluated once, at block entry) only as long as a block
/// leaves its own guard bits alone. `Circuit::validate_op` enforces exactly
/// that, so the flat list can never mean something the source does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub kind: OpKind,
    pub conditions: Vec<Condition>,
}

impl Operation {
    pub fn new(kind: OpKind) -> Operation {
        Operation {
            kind,
            conditions: Vec::new(),
        }
    }

    pub fn with_conditions(kind: OpKind, conditions: Vec<Condition>) -> Operation {
        Operation { kind, conditions }
    }

    /// Qubits the operation touches, for scheduling and keyframe selection.
    pub fn qubits(&self) -> &[usize] {
        match &self.kind {
            OpKind::Gate { qubits, .. } | OpKind::Barrier { qubits } => qubits,
            OpKind::Measure { qubit, .. } | OpKind::Reset { qubit } => std::slice::from_ref(qubit),
            OpKind::GlobalPhase(_) => &[],
        }
    }
}

/// The IR as JSON is the artefact the browser round-trips through `parse` and
/// `toQasm3`, so its field names follow the JavaScript convention like every
/// other value that crosses that boundary.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Circuit {
    qubit_registers: Vec<Register>,
    clbit_registers: Vec<Register>,
    num_qubits: usize,
    num_clbits: usize,
    ops: Vec<Operation>,
}

impl Circuit {
    pub fn new() -> Circuit {
        Circuit::default()
    }

    pub fn num_qubits(&self) -> usize {
        self.num_qubits
    }

    pub fn num_clbits(&self) -> usize {
        self.num_clbits
    }

    pub fn ops(&self) -> &[Operation] {
        &self.ops
    }

    pub fn qubit_registers(&self) -> &[Register] {
        &self.qubit_registers
    }

    pub fn clbit_registers(&self) -> &[Register] {
        &self.clbit_registers
    }

    pub fn add_qubit_register(&mut self, name: &str, size: usize) -> Result<usize> {
        let register = build_register(&self.qubit_registers, self.num_qubits, name, size)?;
        self.num_qubits += size;
        self.qubit_registers.push(register);
        Ok(self.qubit_registers.len() - 1)
    }

    pub fn add_clbit_register(&mut self, name: &str, size: usize) -> Result<usize> {
        let register = build_register(&self.clbit_registers, self.num_clbits, name, size)?;
        self.num_clbits += size;
        self.clbit_registers.push(register);
        Ok(self.clbit_registers.len() - 1)
    }

    pub fn push(&mut self, op: Operation) -> Result<()> {
        self.validate_op(&op)?;
        self.ops.push(op);
        Ok(())
    }

    pub fn push_gate(&mut self, gate: Gate, qubits: &[usize]) -> Result<()> {
        self.push(Operation::new(OpKind::Gate {
            gate,
            qubits: qubits.to_vec(),
        }))
    }

    pub fn push_measure(&mut self, qubit: usize, clbit: usize) -> Result<()> {
        self.push(Operation::new(OpKind::Measure { qubit, clbit }))
    }

    pub fn push_reset(&mut self, qubit: usize) -> Result<()> {
        self.push(Operation::new(OpKind::Reset { qubit }))
    }

    pub fn push_barrier(&mut self, qubits: &[usize]) -> Result<()> {
        self.push(Operation::new(OpKind::Barrier {
            qubits: qubits.to_vec(),
        }))
    }

    fn validate_op(&self, op: &Operation) -> Result<()> {
        let check_qubit = |q: usize| -> Result<()> {
            if q < self.num_qubits {
                Ok(())
            } else {
                Err(invalid(format!(
                    "qubit index {q} is out of range for a {}-qubit circuit",
                    self.num_qubits
                )))
            }
        };
        match &op.kind {
            OpKind::Gate { gate, qubits } => {
                gate.validate()?;
                if qubits.len() != gate.arity() {
                    return Err(invalid(format!(
                        "gate `{}` takes {} qubit(s), got {}",
                        gate.qasm_name(),
                        gate.arity(),
                        qubits.len()
                    )));
                }
                if qubits.len() == 2 && qubits[0] == qubits[1] {
                    return Err(invalid(format!(
                        "gate `{}` was given the same qubit twice",
                        gate.qasm_name()
                    )));
                }
                for &q in qubits {
                    check_qubit(q)?;
                }
            }
            OpKind::GlobalPhase(angle) => {
                if !angle.is_finite() {
                    return Err(invalid("global phase must be finite"));
                }
            }
            OpKind::Measure { qubit, clbit } => {
                check_qubit(*qubit)?;
                if *clbit >= self.num_clbits {
                    return Err(invalid(format!(
                        "bit index {clbit} is out of range for a {}-bit register space",
                        self.num_clbits
                    )));
                }
                // OpenQASM 3 evaluates an `if` condition once, at block entry,
                // while this IR carries the condition on every operation of the
                // block and re-reads it before each one. The two only agree as
                // long as the block leaves its own guard bits alone, so a
                // guarded measurement into a guard bit is refused instead of
                // executing half a block.
                if op
                    .conditions
                    .iter()
                    .any(|condition| self.condition_reads(condition, *clbit))
                {
                    let name = self
                        .clbit_ref(*clbit)
                        .unwrap_or_else(|_| format!("bit {clbit}"));
                    return Err(invalid(format!(
                        "a guarded measurement may not write `{name}`, which its own condition reads; move it out of the `if` body"
                    )));
                }
            }
            OpKind::Reset { qubit } => check_qubit(*qubit)?,
            OpKind::Barrier { qubits } => {
                // A barrier over no qubit would print as a bare `barrier;`,
                // which the OpenQASM 3 front end cannot read back, so the IR
                // never holds one.
                if qubits.is_empty() {
                    return Err(invalid("a barrier needs at least one qubit"));
                }
                for &q in qubits {
                    check_qubit(q)?;
                }
            }
        }
        for condition in &op.conditions {
            match condition {
                Condition::Bit { clbit, .. } => {
                    if *clbit >= self.num_clbits {
                        return Err(invalid(format!("condition bit {clbit} is out of range")));
                    }
                }
                Condition::Register { register, .. } => {
                    let reg = self.clbit_registers.get(*register).ok_or_else(|| {
                        invalid(format!("condition register {register} does not exist"))
                    })?;
                    if reg.size > MAX_CONDITION_REGISTER_BITS {
                        return Err(invalid(format!(
                            "register `{}` holds {} bits; a condition compares at most {MAX_CONDITION_REGISTER_BITS}",
                            reg.name, reg.size
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// `true` when the stabilizer tableau can run the whole circuit.
    pub fn is_clifford(&self) -> bool {
        self.ops.iter().all(|op| match &op.kind {
            OpKind::Gate { gate, .. } => gate.is_clifford(),
            // A global phase is unobservable, so it never blocks the tableau.
            OpKind::GlobalPhase(_) => true,
            OpKind::Measure { .. } | OpKind::Reset { .. } | OpKind::Barrier { .. } => true,
        })
    }

    /// `true` when sampling has to replay the circuit per shot: a reset, a
    /// classically guarded operation, or a measurement followed by more work on
    /// the measured qubit all make the final state shot-dependent.
    pub fn needs_shot_by_shot(&self) -> bool {
        let mut measured = vec![false; self.num_qubits];
        for op in &self.ops {
            if !op.conditions.is_empty() {
                return true;
            }
            match &op.kind {
                OpKind::Reset { .. } => return true,
                OpKind::Measure { qubit, .. } => measured[*qubit] = true,
                OpKind::Gate { qubits, .. } => {
                    if qubits.iter().any(|q| measured[*q]) {
                        return true;
                    }
                }
                OpKind::Barrier { .. } | OpKind::GlobalPhase(_) => {}
            }
        }
        false
    }

    /// Evaluate a conjunction of classical guards against a bit register image.
    /// Both simulators share this so a guard can never mean two things.
    pub fn conditions_hold(&self, conditions: &[Condition], clbits: &[bool]) -> bool {
        conditions.iter().all(|condition| match condition {
            Condition::Bit { clbit, value } => clbits[*clbit] == *value,
            Condition::Register {
                register,
                value,
                equal,
            } => {
                let reg = &self.clbit_registers[*register];
                // `validate_op` refuses a wider register, so the whole image
                // fits the compared `u64` and nothing is truncated here.
                let mut actual = 0u64;
                for offset in 0..reg.size {
                    if clbits[reg.start + offset] {
                        actual |= 1u64 << offset;
                    }
                }
                (actual == *value) == *equal
            }
        })
    }

    /// `true` when the guard reads `clbit`, either directly or through the
    /// register it compares.
    fn condition_reads(&self, condition: &Condition, clbit: usize) -> bool {
        match condition {
            Condition::Bit { clbit: guarded, .. } => *guarded == clbit,
            Condition::Register { register, .. } => self
                .clbit_registers
                .get(*register)
                .is_some_and(|reg| clbit >= reg.start && clbit < reg.start + reg.size),
        }
    }

    /// Human-readable reference to a qubit, e.g. `q[2]`.
    pub fn qubit_ref(&self, index: usize) -> Result<String> {
        register_ref(&self.qubit_registers, index, "qubit")
    }

    /// Human-readable reference to a classical bit, e.g. `c[2]`.
    pub fn clbit_ref(&self, index: usize) -> Result<String> {
        register_ref(&self.clbit_registers, index, "bit")
    }

    /// Canonical OpenQASM 3 rendering. Deterministic: the same circuit always
    /// produces byte-identical text, and parsing it back yields the same IR.
    pub fn to_qasm3(&self) -> String {
        let mut out = String::from("OPENQASM 3.0;\ninclude \"stdgates.inc\";\n");
        for reg in &self.qubit_registers {
            out.push_str(&format!("qubit[{}] {};\n", reg.size, reg.name));
        }
        for reg in &self.clbit_registers {
            out.push_str(&format!("bit[{}] {};\n", reg.size, reg.name));
        }
        for op in &self.ops {
            self.write_op(&mut out, op);
        }
        out
    }

    fn write_op(&self, out: &mut String, op: &Operation) {
        let depth = op.conditions.len();
        for (level, condition) in op.conditions.iter().enumerate() {
            out.push_str(&indent(level));
            out.push_str(&format!("if ({}) {{\n", self.condition_text(condition)));
        }
        out.push_str(&indent(depth));
        out.push_str(&self.op_text(op));
        out.push('\n');
        for level in (0..depth).rev() {
            out.push_str(&indent(level));
            out.push_str("}\n");
        }
    }

    fn condition_text(&self, condition: &Condition) -> String {
        match condition {
            Condition::Bit { clbit, value } => {
                let name = self
                    .clbit_ref(*clbit)
                    .unwrap_or_else(|_| format!("<bit {clbit}>"));
                format!("{name} == {}", u8::from(*value))
            }
            Condition::Register {
                register,
                value,
                equal,
            } => {
                let name = self
                    .clbit_registers
                    .get(*register)
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| format!("<register {register}>"));
                let op = if *equal { "==" } else { "!=" };
                format!("{name} {op} {value}")
            }
        }
    }

    fn op_text(&self, op: &Operation) -> String {
        let qref = |q: &usize| {
            self.qubit_ref(*q)
                .unwrap_or_else(|_| format!("<qubit {q}>"))
        };
        match &op.kind {
            OpKind::Gate { gate, qubits } => {
                let operands = qubits.iter().map(qref).collect::<Vec<_>>().join(", ");
                let params = gate.params();
                let args = if params.is_empty() {
                    String::new()
                } else {
                    format!(
                        "({})",
                        params
                            .iter()
                            .map(|p| format_float(*p))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                // `sxdg` has no name in stdgates.inc, so the canonical form of
                // its inverse is the modifier the standard library itself uses.
                let prefix = if matches!(gate, Gate::SxDg) {
                    "inv @ "
                } else {
                    ""
                };
                format!("{prefix}{}{args} {operands};", gate.qasm_name())
            }
            OpKind::GlobalPhase(angle) => format!("gphase({});", format_float(*angle)),
            OpKind::Measure { qubit, clbit } => {
                let bit = self
                    .clbit_ref(*clbit)
                    .unwrap_or_else(|_| format!("<bit {clbit}>"));
                format!("{bit} = measure {};", qref(qubit))
            }
            OpKind::Reset { qubit } => format!("reset {};", qref(qubit)),
            OpKind::Barrier { qubits } => format!(
                "barrier {};",
                qubits.iter().map(qref).collect::<Vec<_>>().join(", ")
            ),
        }
    }
}

fn indent(level: usize) -> String {
    "  ".repeat(level)
}

fn register_ref(registers: &[Register], index: usize, kind: &str) -> Result<String> {
    for reg in registers {
        if index >= reg.start && index < reg.start + reg.size {
            return Ok(format!("{}[{}]", reg.name, index - reg.start));
        }
    }
    Err(invalid(format!(
        "{kind} index {index} belongs to no register"
    )))
}

fn build_register(
    existing: &[Register],
    start: usize,
    name: &str,
    size: usize,
) -> Result<Register> {
    if size == 0 {
        return Err(invalid(format!(
            "register `{name}` must hold at least one bit"
        )));
    }
    if !is_identifier(name) {
        return Err(invalid(format!(
            "`{name}` is not a valid OpenQASM 3 identifier"
        )));
    }
    if existing.iter().any(|r| r.name == name) {
        return Err(invalid(format!("register `{name}` is declared twice")));
    }
    Ok(Register {
        name: name.to_string(),
        start,
        size,
    })
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Shortest decimal form that reads back as the same `f64`, always carrying a
/// `.` or an exponent so the OpenQASM 3 lexer sees a float and not an integer.
pub(crate) fn format_float(value: f64) -> String {
    let text = format!("{value:?}");
    if text.contains('.') || text.contains('e') || text.contains('E') {
        text
    } else {
        format!("{text}.0")
    }
}
