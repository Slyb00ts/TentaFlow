// ===== File: export.rs — Qiskit-Python rendering of a circuit =====
//
// Canonical OpenQASM 3 lives in `Circuit::to_qasm3`; this module covers the
// second textual target of plan 6.1. QIR and vendor dialects are produced by the
// Python service, not here.

use crate::gate::Gate;
use crate::ir::{Circuit, Condition, OpKind};

/// Qiskit program that rebuilds the circuit.
///
/// Classical guards are emitted with `if_test`, including the `else` block a
/// negated register comparison needs, so a round trip through Qiskit keeps the
/// control flow the IR carries.
pub fn qiskit_python(circuit: &Circuit) -> String {
    let mut out = String::new();
    out.push_str("from qiskit import QuantumCircuit, QuantumRegister, ClassicalRegister\n\n");
    for reg in circuit.qubit_registers() {
        out.push_str(&format!(
            "{} = QuantumRegister({}, \"{}\")\n",
            reg.name, reg.size, reg.name
        ));
    }
    for reg in circuit.clbit_registers() {
        out.push_str(&format!(
            "{} = ClassicalRegister({}, \"{}\")\n",
            reg.name, reg.size, reg.name
        ));
    }
    let registers: Vec<String> = circuit
        .qubit_registers()
        .iter()
        .chain(circuit.clbit_registers())
        .map(|r| r.name.clone())
        .collect();
    out.push_str(&format!(
        "circuit = QuantumCircuit({})\n",
        registers.join(", ")
    ));

    for op in circuit.ops() {
        let mut depth = 0usize;
        for (level, condition) in op.conditions.iter().enumerate() {
            match condition {
                Condition::Bit { clbit, value } => {
                    let bit = circuit
                        .clbit_ref(*clbit)
                        .unwrap_or_else(|_| format!("c[{clbit}]"));
                    out.push_str(&format!(
                        "{}with circuit.if_test(({bit}, {})):\n",
                        indent(depth),
                        u8::from(*value)
                    ));
                    depth += 1;
                }
                Condition::Register {
                    register,
                    value,
                    equal,
                } => {
                    let name = circuit
                        .clbit_registers()
                        .get(*register)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| format!("c{register}"));
                    if *equal {
                        out.push_str(&format!(
                            "{}with circuit.if_test(({name}, {value})):\n",
                            indent(depth)
                        ));
                        depth += 1;
                    } else {
                        // `if_test` only compares for equality, so the negated
                        // guard becomes the else branch of an empty if.
                        let handle = format!("_else{level}");
                        out.push_str(&format!(
                            "{}with circuit.if_test(({name}, {value})) as {handle}:\n",
                            indent(depth)
                        ));
                        out.push_str(&format!("{}pass\n", indent(depth + 1)));
                        out.push_str(&format!("{}with {handle}:\n", indent(depth)));
                        depth += 1;
                    }
                }
            }
        }
        out.push_str(&indent(depth));
        out.push_str(&statement(circuit, &op.kind));
        out.push('\n');
    }
    out
}

fn indent(depth: usize) -> String {
    "    ".repeat(depth)
}

fn statement(circuit: &Circuit, kind: &OpKind) -> String {
    let qref = |q: &usize| circuit.qubit_ref(*q).unwrap_or_else(|_| format!("q[{q}]"));
    match kind {
        OpKind::Gate { gate, qubits } => {
            let mut args: Vec<String> = gate
                .params()
                .iter()
                .map(|p| crate::ir::format_float(*p))
                .collect();
            args.extend(qubits.iter().map(qref));
            format!("circuit.{}({})", qiskit_method(gate), args.join(", "))
        }
        OpKind::GlobalPhase(angle) => format!(
            "circuit.global_phase += {}",
            crate::ir::format_float(*angle)
        ),
        OpKind::Measure { qubit, clbit } => format!(
            "circuit.measure({}, {})",
            qref(qubit),
            circuit
                .clbit_ref(*clbit)
                .unwrap_or_else(|_| format!("c[{clbit}]"))
        ),
        OpKind::Reset { qubit } => format!("circuit.reset({})", qref(qubit)),
        OpKind::Barrier { qubits } => format!(
            "circuit.barrier({})",
            qubits.iter().map(qref).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Qiskit `QuantumCircuit` method for a gate. The names line up with
/// `stdgates.inc` except for the builtin `U`, which Qiskit spells `u`, and for
/// `sxdg`, which Qiskit has but OpenQASM 3 does not.
fn qiskit_method(gate: &Gate) -> &'static str {
    match gate {
        Gate::U(..) => "u",
        Gate::SxDg => "sxdg",
        other => other.qasm_name(),
    }
}
