// ===== File: parse/lower.rs — abstract semantic graph to circuit IR =====

use std::collections::HashMap;
use std::f64::consts::{E, PI, TAU};

use oq3_semantics::asg::{
    self, ArithOp, BinaryOp, CmpOp, Expr, GateModifier, GateOperand, Literal, Stmt, TExpr, UnaryOp,
};
use oq3_semantics::symbols::{SymbolId, SymbolTable, SymbolType};
use oq3_semantics::types::Type;

use super::InputValues;
use crate::error::{invalid, Error, Result, SourcePos};
use crate::gate::{resolve_named_gate, Gate, MAX_GATE_POWER};
use crate::ir::{Circuit, Condition, OpKind, Operation};

/// Guard against a `gate` definition that calls itself; OpenQASM 3 forbids it,
/// but a malformed program must fail with a message rather than a stack overflow.
const MAX_INLINE_DEPTH: usize = 64;

/// Guard against `for i in [0:100000000]` turning into an unbounded IR.
const MAX_OPERATIONS: usize = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Value {
    Int(i64),
    Float(f64),
}

impl Value {
    fn as_f64(self) -> f64 {
        match self {
            Value::Int(v) => v as f64,
            Value::Float(v) => v,
        }
    }

    fn as_i64(self) -> Result<i64> {
        match self {
            Value::Int(v) => Ok(v),
            Value::Float(v) if (v - v.round()).abs() < 1e-9 => Ok(v.round() as i64),
            Value::Float(v) => Err(invalid(format!("expected an integer, got {v}"))),
        }
    }
}

/// A gate call already resolved to concrete qubits, before classical guards are
/// attached. Modifiers rewrite this in place.
#[derive(Debug, Clone, PartialEq)]
struct Expanded {
    global_phase: f64,
    ops: Vec<(Gate, Vec<usize>)>,
}

struct Lowerer<'a> {
    symbols: &'a SymbolTable,
    inputs: &'a InputValues,
    values: HashMap<SymbolId, Value>,
    qubit_registers: HashMap<SymbolId, (usize, usize)>,
    clbit_registers: HashMap<SymbolId, (usize, usize, usize)>,
    qubit_bindings: HashMap<SymbolId, usize>,
    gate_definitions: HashMap<SymbolId, asg::GateDefinition>,
    circuit: Circuit,
    depth: usize,
}

pub fn lower(
    program: &asg::Program,
    symbols: &SymbolTable,
    inputs: &InputValues,
    positions: &[SourcePos],
) -> Result<Circuit> {
    let mut lowerer = Lowerer {
        symbols,
        inputs,
        values: HashMap::new(),
        qubit_registers: HashMap::new(),
        clbit_registers: HashMap::new(),
        qubit_bindings: HashMap::new(),
        gate_definitions: HashMap::new(),
        circuit: Circuit::new(),
        depth: 0,
    };
    // Diagnostics are pinned to the top-level statement being lowered: the
    // semantic graph has no ranges of its own, so that is the finest source
    // granularity available.
    for (index, stmt) in program.stmts().iter().enumerate() {
        lowerer
            .statement(stmt, &[])
            .map_err(|error| locate(error, positions.get(index).copied()))?;
    }
    Ok(lowerer.circuit)
}

/// Give a positionless lowering diagnostic the position of its statement.
fn locate(error: Error, pos: Option<SourcePos>) -> Error {
    match (error, pos) {
        (Error::Invalid(message), Some(pos)) => Error::Semantic { pos, message },
        (error, _) => error,
    }
}

impl Lowerer<'_> {
    fn name_of(&self, id: &SymbolId) -> String {
        self.symbols[id].name().to_string()
    }

    fn symbol(&self, id: &oq3_semantics::symbols::SymbolIdResult) -> Result<SymbolId> {
        id.clone()
            .map_err(|_| invalid("the program refers to an undefined name"))
    }

    fn statements(&mut self, stmts: &[Stmt], conditions: &[Condition]) -> Result<()> {
        for stmt in stmts {
            self.statement(stmt, conditions)?;
        }
        Ok(())
    }

    fn statement(&mut self, stmt: &Stmt, conditions: &[Condition]) -> Result<()> {
        if self.circuit.ops().len() > MAX_OPERATIONS {
            return Err(invalid(format!(
                "circuit exceeds {MAX_OPERATIONS} operations after unrolling"
            )));
        }
        match stmt {
            Stmt::AnnotatedStmt(annotated) => self.statement(annotated.statement(), conditions),
            Stmt::Block(block) => self.statements(block.statements(), conditions),
            Stmt::Include(_) | Stmt::Pragma(_) | Stmt::NullStmt => Ok(()),
            Stmt::DeclareQuantum(decl) => {
                let id = self.symbol(decl.name())?;
                let size = match self.symbols[&id].symbol_type() {
                    Type::Qubit => 1,
                    Type::QubitArray(dims) => dims.dims()[0],
                    other => {
                        return Err(invalid(format!(
                            "`{}` is declared as {other:?}, not a qubit register",
                            self.name_of(&id)
                        )))
                    }
                };
                let name = self.name_of(&id);
                let start = self.circuit.num_qubits();
                self.circuit.add_qubit_register(&name, size)?;
                self.qubit_registers.insert(id, (start, size));
                Ok(())
            }
            Stmt::InputDeclaration(decl) => {
                let id = self.symbol(decl.name())?;
                let name = self.name_of(&id);
                match self.symbols[&id].symbol_type() {
                    Type::Float(..) => {}
                    other => {
                        return Err(invalid(format!(
                            "`input {name}` must be a float, it is declared as {other:?}"
                        )))
                    }
                }
                let value = self
                    .inputs
                    .get(&name)
                    .copied()
                    .ok_or(Error::UnboundInput { name: name.clone() })?;
                self.values.insert(id, Value::Float(value));
                Ok(())
            }
            Stmt::DeclareClassical(decl) => self.declare_classical(decl, conditions),
            Stmt::GateDefinition(definition) => {
                let id = self.symbol(definition.name())?;
                self.gate_definitions.insert(id, definition.clone());
                Ok(())
            }
            Stmt::GateCall(call) => self.gate_call(call, conditions),
            Stmt::GPhaseCall(call) => {
                let angle = self.eval(call.arg())?.as_f64();
                self.push(OpKind::GlobalPhase(angle), conditions)
            }
            Stmt::Barrier(barrier) => {
                // An operand-less `barrier;` never reaches here (the upstream
                // analyser panics on it, which the front end reports), and an
                // empty barrier is rejected by the IR either way.
                let mut qubits = Vec::new();
                for operand in barrier.qubits().into_iter().flatten() {
                    qubits.extend(self.resolve_qubits(operand)?);
                }
                self.push(OpKind::Barrier { qubits }, conditions)
            }
            Stmt::Reset(reset) => {
                for qubit in self.resolve_qubits(reset.gate_operand())? {
                    self.push(OpKind::Reset { qubit }, conditions)?;
                }
                Ok(())
            }
            Stmt::Assignment(assignment) => self.assignment(assignment, conditions),
            Stmt::If(branch) => {
                let condition = self.condition(branch.condition())?;
                let mut then_conditions = conditions.to_vec();
                then_conditions.push(condition.clone());
                self.statements(branch.then_branch().statements(), &then_conditions)?;
                if let Some(else_branch) = branch.else_branch() {
                    let mut else_conditions = conditions.to_vec();
                    else_conditions.push(negate(&condition));
                    self.statements(else_branch.statements(), &else_conditions)?;
                }
                Ok(())
            }
            Stmt::ForStmt(loop_stmt) => self.for_loop(loop_stmt, conditions),
            Stmt::ExprStmt(expr) => Err(invalid(format!(
                "a bare expression statement is outside the supported subset: {:?}",
                expr.expression()
            ))),
            other => Err(invalid(format!(
                "statement {} is outside the supported subset",
                statement_name(other)
            ))),
        }
    }

    fn declare_classical(
        &mut self,
        decl: &asg::DeclareClassical,
        conditions: &[Condition],
    ) -> Result<()> {
        let id = self.symbol(decl.name())?;
        let name = self.name_of(&id);
        let size = match self.symbols[&id].symbol_type() {
            Type::Bit(_) => Some(1),
            Type::BitArray(dims, _) => Some(dims.dims()[0]),
            _ => None,
        };
        match size {
            Some(size) => {
                let start = self.circuit.num_clbits();
                let index = self.circuit.add_clbit_register(&name, size)?;
                self.clbit_registers.insert(id, (index, start, size));
                if let Some(initializer) = decl.initializer() {
                    let qubits = self.measure_operand(initializer)?;
                    if qubits.len() != size {
                        return Err(invalid(format!(
                            "`{name}` holds {size} bit(s) but the measurement yields {}",
                            qubits.len()
                        )));
                    }
                    for (offset, qubit) in qubits.into_iter().enumerate() {
                        self.push(
                            OpKind::Measure {
                                qubit,
                                clbit: start + offset,
                            },
                            conditions,
                        )?;
                    }
                }
                Ok(())
            }
            None => {
                // A classical scalar is accepted only as a compile-time constant
                // that gate parameters and loop bounds can read.
                let initializer = decl.initializer().ok_or_else(|| {
                    invalid(format!(
                        "`{name}` has no initializer; classical variables outside `bit` are only supported as constants"
                    ))
                })?;
                let value = self.eval(initializer)?;
                self.values.insert(id, value);
                Ok(())
            }
        }
    }

    fn assignment(&mut self, assignment: &asg::Assignment, conditions: &[Condition]) -> Result<()> {
        let qubits = self.measure_operand(assignment.rvalue())?;
        let clbits = match assignment.lvalue() {
            asg::LValue::Identifier(id) => {
                let id = self.symbol(id)?;
                let (_, start, size) = *self.clbit_registers.get(&id).ok_or_else(|| {
                    invalid(format!("`{}` is not a bit register", self.name_of(&id)))
                })?;
                (start..start + size).collect::<Vec<_>>()
            }
            asg::LValue::IndexedIdentifier(indexed) => vec![self.indexed_clbit(indexed)?],
        };
        if clbits.len() != qubits.len() {
            return Err(invalid(format!(
                "measurement writes {} bit(s) into {} bit(s)",
                qubits.len(),
                clbits.len()
            )));
        }
        for (qubit, clbit) in qubits.into_iter().zip(clbits) {
            self.push(OpKind::Measure { qubit, clbit }, conditions)?;
        }
        Ok(())
    }

    fn measure_operand(&self, expr: &TExpr) -> Result<Vec<usize>> {
        match expr.expression() {
            Expr::MeasureExpression(measure) => self.resolve_qubits(measure.operand()),
            _ => Err(invalid(
                "only `measure` may be assigned to a bit; classical arithmetic is outside the supported subset",
            )),
        }
    }

    fn for_loop(&mut self, loop_stmt: &asg::ForStmt, conditions: &[Condition]) -> Result<()> {
        let variable = self.symbol(loop_stmt.loop_var())?;
        let values: Vec<i64> =
            match loop_stmt.iterable() {
                asg::ForIterable::SetExpression(set) => set
                    .expressions()
                    .iter()
                    .map(|e| self.eval(e).and_then(Value::as_i64))
                    .collect::<Result<Vec<_>>>()?,
                asg::ForIterable::RangeExpression(range) => {
                    let start = self.eval(range.start())?.as_i64()?;
                    let stop = self.eval(range.stop())?.as_i64()?;
                    let step = match range.step() {
                        Some(step) => self.eval(step)?.as_i64()?,
                        None => 1,
                    };
                    if step == 0 {
                        return Err(invalid("a `for` range needs a non-zero step"));
                    }
                    let mut values = Vec::new();
                    let mut current = start;
                    while (step > 0 && current <= stop) || (step < 0 && current >= stop) {
                        values.push(current);
                        if values.len() > MAX_OPERATIONS {
                            return Err(invalid("`for` range unrolls to too many iterations"));
                        }
                        current += step;
                    }
                    values
                }
                asg::ForIterable::Expr(_) => return Err(invalid(
                    "`for` over a register is outside the supported subset; use a constant range",
                )),
            };

        let previous = self.values.insert(variable.clone(), Value::Int(0));
        for value in values {
            self.values.insert(variable.clone(), Value::Int(value));
            self.statements(loop_stmt.loop_body().statements(), conditions)?;
        }
        match previous {
            Some(value) => self.values.insert(variable, value),
            None => self.values.remove(&variable),
        };
        Ok(())
    }

    fn push(&mut self, kind: OpKind, conditions: &[Condition]) -> Result<()> {
        self.circuit
            .push(Operation::with_conditions(kind, conditions.to_vec()))
    }

    fn gate_call(&mut self, call: &asg::GateCall, conditions: &[Condition]) -> Result<()> {
        let controls = call
            .modifiers()
            .iter()
            .filter(|m| matches!(m, GateModifier::Ctrl(_) | GateModifier::NegCtrl(_)))
            .count();

        let mut operand_lists = Vec::new();
        for operand in call.qubits() {
            operand_lists.push(self.resolve_qubits(operand)?);
        }
        let width = broadcast_width(&operand_lists)?;
        if operand_lists.len() < controls {
            return Err(invalid("a control modifier has no qubit to act on"));
        }

        for slot in 0..width {
            let operands: Vec<usize> = operand_lists
                .iter()
                .map(|list| if list.len() == 1 { list[0] } else { list[slot] })
                .collect();
            let (control_qubits, inner_qubits) = operands.split_at(controls);
            let mut expanded = self.expand_base(call, inner_qubits)?;
            let mut control = control_qubits.iter().rev();
            for modifier in call.modifiers().iter().rev() {
                expanded = match modifier {
                    GateModifier::Inv => invert(expanded),
                    GateModifier::Pow(exponent) => power(expanded, self.eval(exponent)?.as_i64()?)?,
                    GateModifier::Ctrl(count) => {
                        self.check_control_count(count)?;
                        let qubit = *control
                            .next()
                            .ok_or_else(|| invalid("a control modifier has no qubit to act on"))?;
                        control_gate(expanded, qubit, false)?
                    }
                    GateModifier::NegCtrl(count) => {
                        self.check_control_count(count)?;
                        let qubit = *control
                            .next()
                            .ok_or_else(|| invalid("a control modifier has no qubit to act on"))?;
                        control_gate(expanded, qubit, true)?
                    }
                };
            }
            self.emit(expanded, conditions)?;
        }
        Ok(())
    }

    fn check_control_count(&self, count: &Option<TExpr>) -> Result<()> {
        match count {
            None => Ok(()),
            Some(expr) => {
                if self.eval(expr)?.as_i64()? == 1 {
                    Ok(())
                } else {
                    Err(invalid(
                        "only a single control qubit is supported; the simulator has no gates above two qubits",
                    ))
                }
            }
        }
    }

    fn emit(&mut self, expanded: Expanded, conditions: &[Condition]) -> Result<()> {
        if expanded.global_phase != 0.0 {
            self.push(OpKind::GlobalPhase(expanded.global_phase), conditions)?;
        }
        for (gate, qubits) in expanded.ops {
            self.push(OpKind::Gate { gate, qubits }, conditions)?;
        }
        Ok(())
    }

    /// Resolve the gate itself, either from `stdgates.inc` or by inlining a
    /// user `gate` definition.
    fn expand_base(&mut self, call: &asg::GateCall, qubits: &[usize]) -> Result<Expanded> {
        let id = self.symbol(call.name())?;
        let mut params = Vec::new();
        if let Some(exprs) = call.params() {
            for expr in exprs {
                params.push(self.eval(expr)?.as_f64());
            }
        }
        if let Some(definition) = self.gate_definitions.get(&id).cloned() {
            return self.inline_definition(&definition, &params, qubits);
        }
        let name = self.name_of(&id);
        let expansion = resolve_named_gate(&name, &params, qubits.len())?;
        Ok(Expanded {
            global_phase: expansion.global_phase,
            ops: expansion
                .ops
                .into_iter()
                .map(|(gate, positions)| (gate, positions.into_iter().map(|p| qubits[p]).collect()))
                .collect(),
        })
    }

    fn inline_definition(
        &mut self,
        definition: &asg::GateDefinition,
        params: &[f64],
        qubits: &[usize],
    ) -> Result<Expanded> {
        if self.depth >= MAX_INLINE_DEPTH {
            return Err(invalid(
                "gate definitions are nested too deeply; a recursive `gate` is not valid OpenQASM 3",
            ));
        }
        let formal_params = definition.params().unwrap_or(&[]);
        if formal_params.len() != params.len() || definition.qubits().len() != qubits.len() {
            return Err(invalid(format!(
                "gate `{}` takes {} parameter(s) and {} qubit(s)",
                self.name_of(&self.symbol(definition.name())?),
                formal_params.len(),
                definition.qubits().len()
            )));
        }

        let mut saved_values = Vec::new();
        for (formal, value) in formal_params.iter().zip(params) {
            let id = self.symbol(formal)?;
            saved_values.push((id.clone(), self.values.insert(id, Value::Float(*value))));
        }
        let mut saved_qubits = Vec::new();
        for (formal, qubit) in definition.qubits().iter().zip(qubits) {
            let id = self.symbol(formal)?;
            saved_qubits.push((id.clone(), self.qubit_bindings.insert(id, *qubit)));
        }

        self.depth += 1;
        let result = self.inline_body(definition.block().statements());
        self.depth -= 1;

        for (id, previous) in saved_values {
            match previous {
                Some(value) => self.values.insert(id, value),
                None => self.values.remove(&id),
            };
        }
        for (id, previous) in saved_qubits {
            match previous {
                Some(value) => self.qubit_bindings.insert(id, value),
                None => self.qubit_bindings.remove(&id),
            };
        }
        result
    }

    fn inline_body(&mut self, statements: &[Stmt]) -> Result<Expanded> {
        let mut expanded = Expanded {
            global_phase: 0.0,
            ops: Vec::new(),
        };
        for stmt in statements {
            match stmt {
                Stmt::GateCall(call) => {
                    let controls = call
                        .modifiers()
                        .iter()
                        .filter(|m| matches!(m, GateModifier::Ctrl(_) | GateModifier::NegCtrl(_)))
                        .count();
                    let mut operands = Vec::new();
                    for operand in call.qubits() {
                        let resolved = self.resolve_qubits(operand)?;
                        if resolved.len() != 1 {
                            return Err(invalid(
                                "a gate body may only address the qubits named in its signature",
                            ));
                        }
                        operands.push(resolved[0]);
                    }
                    let (control_qubits, inner_qubits) = operands.split_at(controls);
                    let mut inner = self.expand_base(call, inner_qubits)?;
                    let mut control = control_qubits.iter().rev();
                    for modifier in call.modifiers().iter().rev() {
                        inner = match modifier {
                            GateModifier::Inv => invert(inner),
                            GateModifier::Pow(exponent) => {
                                power(inner, self.eval(exponent)?.as_i64()?)?
                            }
                            GateModifier::Ctrl(count) => {
                                self.check_control_count(count)?;
                                let qubit = *control.next().ok_or_else(|| {
                                    invalid("a control modifier has no qubit to act on")
                                })?;
                                control_gate(inner, qubit, false)?
                            }
                            GateModifier::NegCtrl(count) => {
                                self.check_control_count(count)?;
                                let qubit = *control.next().ok_or_else(|| {
                                    invalid("a control modifier has no qubit to act on")
                                })?;
                                control_gate(inner, qubit, true)?
                            }
                        };
                    }
                    expanded.global_phase += inner.global_phase;
                    expanded.ops.extend(inner.ops);
                }
                Stmt::GPhaseCall(call) => {
                    expanded.global_phase += self.eval(call.arg())?.as_f64();
                }
                Stmt::Barrier(_) | Stmt::NullStmt => {}
                other => {
                    return Err(invalid(format!(
                        "a `gate` body may only contain gate calls, found {}",
                        statement_name(other)
                    )))
                }
            }
        }
        Ok(expanded)
    }

    fn resolve_qubits(&self, expr: &TExpr) -> Result<Vec<usize>> {
        let operand = match expr.expression() {
            Expr::GateOperand(operand) => operand,
            Expr::Identifier(_) | Expr::IndexedIdentifier(_) => {
                return self.resolve_qubit_expression(expr.expression())
            }
            other => return Err(invalid(format!("{other:?} is not a qubit operand"))),
        };
        match operand {
            GateOperand::Identifier(id) => self.resolve_qubit_symbol(&self.symbol(id)?),
            GateOperand::IndexedIdentifier(indexed) => {
                Ok(vec![self.resolve_indexed_qubit(indexed)?])
            }
            GateOperand::HardwareQubit(_) => Err(invalid(
                "hardware qubits ($0) are outside the supported subset",
            )),
        }
    }

    fn resolve_qubit_expression(&self, expr: &Expr) -> Result<Vec<usize>> {
        match expr {
            Expr::Identifier(id) => self.resolve_qubit_symbol(&self.symbol(id)?),
            Expr::IndexedIdentifier(indexed) => Ok(vec![self.resolve_indexed_qubit(indexed)?]),
            other => Err(invalid(format!("{other:?} is not a qubit operand"))),
        }
    }

    fn resolve_qubit_symbol(&self, id: &SymbolId) -> Result<Vec<usize>> {
        if let Some(qubit) = self.qubit_bindings.get(id) {
            return Ok(vec![*qubit]);
        }
        match self.qubit_registers.get(id) {
            Some((start, size)) => Ok((*start..start + size).collect()),
            None => Err(invalid(format!("`{}` is not a qubit", self.name_of(id)))),
        }
    }

    fn resolve_indexed_qubit(&self, indexed: &asg::IndexedIdentifier) -> Result<usize> {
        let id = self.symbol(indexed.identifier())?;
        let offset = self.single_index(indexed.indexes())?;
        if let Some(qubit) = self.qubit_bindings.get(&id) {
            return if offset == 0 {
                Ok(*qubit)
            } else {
                Err(invalid("a gate parameter qubit cannot be indexed"))
            };
        }
        let (start, size) = *self
            .qubit_registers
            .get(&id)
            .ok_or_else(|| invalid(format!("`{}` is not a qubit register", self.name_of(&id))))?;
        if offset >= size {
            return Err(invalid(format!(
                "index {offset} is out of range for `{}` of size {size}",
                self.name_of(&id)
            )));
        }
        Ok(start + offset)
    }

    fn indexed_clbit(&self, indexed: &asg::IndexedIdentifier) -> Result<usize> {
        let id = self.symbol(indexed.identifier())?;
        let offset = self.single_index(indexed.indexes())?;
        let (_, start, size) = *self
            .clbit_registers
            .get(&id)
            .ok_or_else(|| invalid(format!("`{}` is not a bit register", self.name_of(&id))))?;
        if offset >= size {
            return Err(invalid(format!(
                "index {offset} is out of range for `{}` of size {size}",
                self.name_of(&id)
            )));
        }
        Ok(start + offset)
    }

    fn single_index(&self, indexes: &[asg::IndexOperator]) -> Result<usize> {
        if indexes.len() != 1 {
            return Err(invalid("only a single index is supported"));
        }
        match &indexes[0] {
            asg::IndexOperator::ExpressionList(list) if list.expressions.len() == 1 => {
                let value = self.eval(&list.expressions[0])?.as_i64()?;
                usize::try_from(value).map_err(|_| invalid("a register index cannot be negative"))
            }
            _ => Err(invalid(
                "slices and index sets are outside the supported subset",
            )),
        }
    }

    fn condition(&self, expr: &TExpr) -> Result<Condition> {
        match expr.expression() {
            Expr::BinaryExpr(binary) => {
                let equal =
                    match binary.op() {
                        BinaryOp::CmpOp(CmpOp::Eq) => true,
                        BinaryOp::CmpOp(CmpOp::Neq) => false,
                        _ => return Err(invalid(
                            "a condition must compare a bit or a bit register with `==` or `!=`",
                        )),
                    };
                let (target, literal) = match (binary.left().expression(), binary.right()) {
                    (Expr::Literal(_), _) => (binary.right().expression(), binary.left()),
                    _ => (binary.left().expression(), binary.right()),
                };
                let value = self.eval(literal)?.as_i64()?;
                if value < 0 {
                    return Err(invalid(
                        "a condition cannot compare against a negative value",
                    ));
                }
                self.condition_on(target, value as u64, equal)
            }
            Expr::UnaryExpr(unary) if *unary.op() == UnaryOp::Not => {
                Ok(negate(&self.condition(unary.operand())?))
            }
            Expr::IndexedIdentifier(indexed) => Ok(Condition::Bit {
                clbit: self.indexed_clbit(indexed)?,
                value: true,
            }),
            other => Err(invalid(format!(
                "{other:?} is not a supported `if` condition"
            ))),
        }
    }

    fn condition_on(&self, target: &Expr, value: u64, equal: bool) -> Result<Condition> {
        match target {
            Expr::IndexedIdentifier(indexed) => {
                if value > 1 {
                    return Err(invalid("a single bit compares only with 0 or 1"));
                }
                Ok(Condition::Bit {
                    clbit: self.indexed_clbit(indexed)?,
                    value: (value == 1) == equal,
                })
            }
            Expr::Identifier(id) => {
                let id = self.symbol(id)?;
                let (register, _, _) = *self.clbit_registers.get(&id).ok_or_else(|| {
                    invalid(format!("`{}` is not a bit register", self.name_of(&id)))
                })?;
                Ok(Condition::Register {
                    register,
                    value,
                    equal,
                })
            }
            other => Err(invalid(format!(
                "{other:?} cannot be used on the left of an `if` comparison"
            ))),
        }
    }

    fn eval(&self, expr: &TExpr) -> Result<Value> {
        match expr.expression() {
            Expr::Literal(Literal::Int(literal)) => {
                let magnitude = i64::try_from(*literal.value())
                    .map_err(|_| invalid("integer literal is too large"))?;
                Ok(Value::Int(if *literal.sign() {
                    magnitude
                } else {
                    -magnitude
                }))
            }
            Expr::Literal(Literal::Float(literal)) => literal
                .value()
                .parse::<f64>()
                .map(Value::Float)
                .map_err(|_| invalid(format!("`{}` is not a float literal", literal.value()))),
            Expr::Literal(Literal::Bool(literal)) => Ok(Value::Int(i64::from(*literal.value()))),
            Expr::Cast(cast) => {
                let inner = self.eval(cast.operand())?;
                Ok(match cast.get_type() {
                    Type::Int(..) | Type::UInt(..) => Value::Int(inner.as_i64()?),
                    _ => Value::Float(inner.as_f64()),
                })
            }
            Expr::Identifier(id) => {
                let id = self.symbol(id)?;
                if let Some(value) = self.values.get(&id) {
                    return Ok(*value);
                }
                match self.name_of(&id).as_str() {
                    "pi" | "\u{03c0}" => Ok(Value::Float(PI)),
                    "tau" | "\u{03c4}" => Ok(Value::Float(TAU)),
                    "euler" | "\u{2107}" => Ok(Value::Float(E)),
                    name => Err(invalid(format!(
                        "`{name}` has no value at parse time; only constants and `input float` parameters can appear here"
                    ))),
                }
            }
            Expr::UnaryExpr(unary) => {
                let inner = self.eval(unary.operand())?;
                match unary.op() {
                    UnaryOp::Minus => Ok(match inner {
                        Value::Int(v) => Value::Int(-v),
                        Value::Float(v) => Value::Float(-v),
                    }),
                    other => Err(invalid(format!("unary `{other:?}` is not supported here"))),
                }
            }
            Expr::BinaryExpr(binary) => {
                let left = self.eval(binary.left())?;
                let right = self.eval(binary.right())?;
                arithmetic(binary.op(), left, right)
            }
            other => Err(invalid(format!(
                "{other:?} cannot be evaluated at parse time"
            ))),
        }
    }
}

fn arithmetic(op: &BinaryOp, left: Value, right: Value) -> Result<Value> {
    let arith = match op {
        BinaryOp::ArithOp(arith) => arith,
        _ => {
            return Err(invalid(
                "comparisons and concatenation are not numeric expressions",
            ))
        }
    };
    if let (Value::Int(a), Value::Int(b)) = (left, right) {
        let value = match arith {
            ArithOp::Add => a.checked_add(b),
            ArithOp::Sub => a.checked_sub(b),
            ArithOp::Mul => a.checked_mul(b),
            ArithOp::Div => a.checked_div(b),
            ArithOp::Mod | ArithOp::Rem => a.checked_rem(b),
            ArithOp::Shl => a.checked_shl(u32::try_from(b).unwrap_or(u32::MAX)),
            ArithOp::Shr => a.checked_shr(u32::try_from(b).unwrap_or(u32::MAX)),
            ArithOp::BitXOr => Some(a ^ b),
            ArithOp::BitAnd => Some(a & b),
        };
        return value
            .map(Value::Int)
            .ok_or_else(|| invalid("integer expression overflows or divides by zero"));
    }
    let (a, b) = (left.as_f64(), right.as_f64());
    let value = match arith {
        ArithOp::Add => a + b,
        ArithOp::Sub => a - b,
        ArithOp::Mul => a * b,
        ArithOp::Div => a / b,
        ArithOp::Mod | ArithOp::Rem => a % b,
        other => return Err(invalid(format!("`{other:?}` is only defined for integers"))),
    };
    Ok(Value::Float(value))
}

fn broadcast_width(lists: &[Vec<usize>]) -> Result<usize> {
    let mut width = 1;
    for list in lists {
        if list.is_empty() {
            return Err(invalid("a gate operand resolves to no qubit"));
        }
        if list.len() == 1 {
            continue;
        }
        if width != 1 && width != list.len() {
            return Err(invalid(
                "broadcast operands must have the same length or be single qubits",
            ));
        }
        width = list.len();
    }
    Ok(width)
}

fn negate(condition: &Condition) -> Condition {
    match condition {
        Condition::Bit { clbit, value } => Condition::Bit {
            clbit: *clbit,
            value: !value,
        },
        Condition::Register {
            register,
            value,
            equal,
        } => Condition::Register {
            register: *register,
            value: *value,
            equal: !equal,
        },
    }
}

fn invert(expanded: Expanded) -> Expanded {
    Expanded {
        global_phase: -expanded.global_phase,
        ops: expanded
            .ops
            .into_iter()
            .rev()
            .map(|(gate, qubits)| (gate.adjoint(), qubits))
            .collect(),
    }
}

fn power(expanded: Expanded, exponent: i64) -> Result<Expanded> {
    // A lone gate is raised by `Gate::integer_power`, which folds the exponent
    // into the angle of a rotation instead of repeating the gate.
    if expanded.global_phase == 0.0 && expanded.ops.len() == 1 {
        let (gate, qubits) = &expanded.ops[0];
        let ops = gate
            .integer_power(exponent)?
            .into_iter()
            .map(|powered| (powered, qubits.clone()))
            .collect();
        return Ok(Expanded {
            global_phase: 0.0,
            ops,
        });
    }
    let base = if exponent < 0 {
        invert(expanded)
    } else {
        expanded
    };
    let count = exponent.unsigned_abs();
    if count > MAX_GATE_POWER as u64 {
        return Err(invalid(format!(
            "`pow` exponent would expand to more than {MAX_GATE_POWER} gates"
        )));
    }
    let mut out = Expanded {
        global_phase: base.global_phase * count as f64,
        ops: Vec::with_capacity(base.ops.len() * count as usize),
    };
    for _ in 0..count {
        out.ops.extend(base.ops.iter().cloned());
    }
    Ok(out)
}

/// `ctrl @` on an expansion. Only a single one-qubit gate can be controlled: a
/// two-qubit gate would need three qubits, which the simulator does not carry.
/// A global phase inside the expansion becomes a phase gate on the control,
/// exactly as `stdgates.inc` defines `p` from `ctrl @ gphase`.
fn control_gate(expanded: Expanded, control: usize, negated: bool) -> Result<Expanded> {
    if expanded.ops.len() > 1 {
        return Err(invalid(
            "`ctrl @` is supported on a single one-qubit gate only",
        ));
    }
    let mut inner = Vec::new();
    if expanded.global_phase != 0.0 {
        inner.push((Gate::P(expanded.global_phase), vec![control]));
    }
    if let Some((gate, qubits)) = expanded.ops.into_iter().next() {
        if qubits.len() != 1 {
            return Err(invalid(
                "`ctrl @` is supported on a single one-qubit gate only",
            ));
        }
        if qubits[0] == control {
            return Err(invalid(
                "`ctrl @` needs a control qubit distinct from the target",
            ));
        }
        // A controlled identity is the identity on both qubits, so it
        // contributes nothing to the expansion.
        if gate != Gate::Id {
            let controlled = gate.controlled().ok_or_else(|| {
                invalid(format!(
                    "`ctrl @ {}` has no equivalent in the supported gate set",
                    gate.qasm_name()
                ))
            })?;
            inner.push((controlled, vec![control, qubits[0]]));
        }
    }
    // `negctrl @` is `ctrl @` conjugated by X on the control. The conjugation is
    // wrapped around the WHOLE expansion, including the empty one: emitting the
    // opening X without its partner would turn `negctrl @ id` into a bare X.
    let ops = if negated && !inner.is_empty() {
        let mut wrapped = Vec::with_capacity(inner.len() + 2);
        wrapped.push((Gate::X, vec![control]));
        wrapped.append(&mut inner);
        wrapped.push((Gate::X, vec![control]));
        wrapped
    } else {
        inner
    };
    Ok(Expanded {
        global_phase: 0.0,
        ops,
    })
}

fn statement_name(stmt: &Stmt) -> &'static str {
    match stmt {
        Stmt::Alias(_) => "alias",
        Stmt::AnnotatedStmt(_) => "annotated statement",
        Stmt::Assignment(_) => "assignment",
        Stmt::Barrier(_) => "barrier",
        Stmt::Block(_) => "block",
        Stmt::Box => "box",
        Stmt::Break => "break",
        Stmt::Cal => "cal",
        Stmt::Continue => "continue",
        Stmt::DeclareClassical(_) => "classical declaration",
        Stmt::DeclareQuantum(_) => "qubit declaration",
        Stmt::DeclareHardwareQubit(_) => "hardware qubit declaration",
        Stmt::DefStmt(_) => "def",
        Stmt::DefCal => "defcal",
        Stmt::Delay(_) => "delay",
        Stmt::End => "end",
        Stmt::ExprStmt(_) => "expression statement",
        Stmt::Extern => "extern",
        Stmt::ForStmt(_) => "for",
        Stmt::GPhaseCall(_) => "gphase",
        Stmt::GateCall(_) => "gate call",
        Stmt::GateDefinition(_) => "gate definition",
        Stmt::InputDeclaration(_) => "input declaration",
        Stmt::OutputDeclaration(_) => "output declaration",
        Stmt::If(_) => "if",
        Stmt::Include(_) => "include",
        Stmt::ModifiedGPhaseCall(_) => "modified gphase",
        Stmt::NullStmt => "empty statement",
        Stmt::OldStyleDeclaration => "OpenQASM 2 declaration",
        Stmt::Pragma(_) => "pragma",
        Stmt::Reset(_) => "reset",
        Stmt::SwitchCaseStmt(_) => "switch",
        Stmt::While(_) => "while",
    }
}
