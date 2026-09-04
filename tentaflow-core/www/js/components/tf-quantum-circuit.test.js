// =============================================================================
// File: components/tf-quantum-circuit.test.js
// Description: The parts of <tf-quantum-circuit> that must not drift: the ASAP
// schedule that turns the IR into a grid (and back), the edits that rewrite the
// IR, the undo stack, the angle grammar, the SVG export, and the DOM layer that
// carries focus and ARIA — the canvas is a picture, so nothing is asserted on it.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}

const {
  buildGrid, gridToCircuit, cellAt, insertOp, removeOps, moveOp, setOpParams,
  setOpWires, UndoStack, parseAngle, formatAngle, circuitToSvg, describeCell,
  makeGate, gateId, gateParams, TfQuantumCircuit,
} = await import('./tf-quantum-circuit.js');

// ---- fixtures --------------------------------------------------------------

function circuit(numQubits, numClbits, ops) {
  return {
    qubitRegisters: [{ name: 'q', start: 0, size: numQubits }],
    clbitRegisters: numClbits ? [{ name: 'c', start: 0, size: numClbits }] : [],
    numQubits,
    numClbits,
    ops,
  };
}

const gate = (id, qubits, params) => ({
  kind: { Gate: { gate: makeGate(id, params || []), qubits } },
  conditions: [],
});
const measure = (qubit, clbit) => ({ kind: { Measure: { qubit, clbit } }, conditions: [] });

// h q[0]; cx q[0], q[1]; c = measure q;
const BELL = circuit(2, 2, [
  gate('H', [0]),
  gate('Cx', [0, 1]),
  measure(0, 0),
  measure(1, 1),
]);

// ---- grid ------------------------------------------------------------------

test('buildGrid schedules a Bell circuit into three moments', () => {
  const grid = buildGrid(BELL);
  assert.equal(grid.numQubits, 2);
  assert.equal(grid.columns, 3);
  assert.deepEqual(grid.cells.map((cell) => cell.column), [0, 1, 2, 2]);
  // The two-qubit gate owns both wires, so its cell answers for either row.
  assert.equal(cellAt(grid, 0, 1).index, 1);
  assert.equal(cellAt(grid, 1, 1).index, 1);
  assert.equal(cellAt(grid, 1, 0), null);
});

test('independent gates share a column, dependent ones do not', () => {
  const grid = buildGrid(circuit(3, 0, [gate('H', [0]), gate('X', [1]), gate('Y', [0])]));
  assert.deepEqual(grid.cells.map((cell) => cell.column), [0, 0, 1]);
});

test('a two-qubit gate reserves the wires its link crosses', () => {
  // The cx spans q0..q2, so the X on q1 cannot share the column — its box would
  // sit on the vertical link.
  const grid = buildGrid(circuit(3, 0, [gate('Cx', [0, 2]), gate('X', [1])]));
  assert.deepEqual(grid.cells.map((cell) => cell.column), [0, 1]);
});

test('a classical guard waits for the measure that writes its bit', () => {
  const conditional = {
    kind: { Gate: { gate: 'X', qubits: [2] } },
    conditions: [{ Bit: { clbit: 0, value: true } }],
  };
  const grid = buildGrid(circuit(3, 1, [measure(0, 0), conditional]));
  assert.deepEqual(grid.cells.map((cell) => cell.column), [0, 1]);
});

test('gridToCircuit round-trips a circuit written in normal form', () => {
  const grid = buildGrid(BELL);
  assert.deepEqual(gridToCircuit(BELL, grid), BELL);
});

// ---- edits -----------------------------------------------------------------

test('insertOp places a gate in the requested column and compacts no further', () => {
  const next = insertOp(BELL, gate('T', [1]), { row: 1, column: 1 });
  const grid = buildGrid(next);
  const inserted = grid.cells.find((cell) => cell.id === 'T');
  assert.ok(inserted, 'the T gate is in the grid');
  // q1 is busy in column 1 (the cx), so the earliest legal moment is column 2.
  assert.equal(inserted.column, 2);
});

test('insertOp on an idle wire settles into the earliest free column', () => {
  const next = insertOp(circuit(2, 0, []), gate('H', [1]), { row: 1, column: 4 });
  assert.equal(buildGrid(next).cells[0].column, 0);
});

test('removeOps drops exactly the named operations', () => {
  const next = removeOps(BELL, [1]);
  assert.equal(next.ops.length, 3);
  assert.ok(!next.ops.some((op) => op.kind.Gate && gateId(op.kind.Gate.gate) === 'Cx'));
});

test('moveOp carries a two-qubit gate down one wire, refusing to leave the register', () => {
  const wide = circuit(3, 0, [gate('Cx', [0, 1])]);
  const moved = moveOp(wide, 0, { row: 1, column: 0 });
  assert.deepEqual(moved.ops[0].kind.Gate.qubits, [1, 2]);
  // Row 2 would put the target on a wire that does not exist: the op stays put.
  assert.deepEqual(moveOp(wide, 0, { row: 2, column: 0 }).ops[0].kind.Gate.qubits, [0, 1]);
});

test('setOpParams rewrites the angle of a parametric gate in place', () => {
  const rotated = circuit(1, 0, [gate('Rz', [0], [0])]);
  const next = setOpParams(rotated, 0, [Math.PI / 4]);
  assert.deepEqual(gateParams(next.ops[0].kind.Gate.gate), [Math.PI / 4]);
  assert.deepEqual(gateParams(rotated.ops[0].kind.Gate.gate), [0], 'the input is untouched');
});

// ---- undo ------------------------------------------------------------------

// Re-pairing is the one edit dragging cannot express: a drag translates every
// operand by the same delta, so a control and a distant target need this.
test('setOpWires re-pairs a two-qubit gate onto non-adjacent wires', () => {
  const source = circuit(3, 0, [gate('Cx', [0, 1])]);
  const next = setOpWires(source, 0, { qubits: [0, 2] });
  assert.deepEqual(next.ops[0].kind.Gate.qubits, [0, 2]);
  assert.deepEqual(source.ops[0].kind.Gate.qubits, [0, 1], 'the input is not mutated');
  const grid = buildGrid(next);
  assert.deepEqual(grid.cells[0].rows, [0, 1, 2], 'the link now crosses the middle wire');
});

test('setOpWires refuses operands the IR cannot hold', () => {
  const source = circuit(3, 0, [gate('Cx', [0, 1])]);
  assert.equal(setOpWires(source, 0, { qubits: [1, 1] }), source, 'a gate on one wire twice');
  assert.equal(setOpWires(source, 0, { qubits: [0, 3] }), source, 'a wire outside the register');
  assert.equal(setOpWires(source, 0, { qubits: [0] }), source, 'the wrong arity');
  assert.equal(setOpWires(source, 9, { qubits: [0, 2] }), source, 'no such operation');
});

test('setOpWires re-points a measurement at another classical bit', () => {
  const source = circuit(2, 2, [measure(0, 0)]);
  assert.equal(setOpWires(source, 0, { clbit: 1 }).ops[0].kind.Measure.clbit, 1);
  assert.equal(setOpWires(source, 0, { clbit: 1 }).ops[0].kind.Measure.qubit, 0);
  assert.equal(setOpWires(source, 0, { clbit: 2 }), source, 'outside the register');
});

test('the undo stack walks back and forward and drops the abandoned future', () => {
  const stack = new UndoStack({ ops: [] });
  stack.push({ ops: ['a'] });
  stack.push({ ops: ['a', 'b'] });
  assert.deepEqual(stack.undo(), { ops: ['a'] });
  assert.equal(stack.canRedo, true);
  assert.deepEqual(stack.redo(), { ops: ['a', 'b'] });
  stack.undo();
  stack.push({ ops: ['a', 'c'] });
  assert.equal(stack.canRedo, false, 'a new edit discards the redo branch');
  assert.deepEqual(stack.current, { ops: ['a', 'c'] });
});

test('the undo stack hands out copies, never its own entries', () => {
  const stack = new UndoStack({ ops: [] });
  const taken = stack.current;
  taken.ops.push('mutated');
  assert.deepEqual(stack.current, { ops: [] });
});

// ---- angles ----------------------------------------------------------------

test('parseAngle understands numbers and pi expressions', () => {
  assert.equal(parseAngle('0'), 0);
  assert.ok(Math.abs(parseAngle('pi/4') - Math.PI / 4) < 1e-12);
  assert.ok(Math.abs(parseAngle('2*pi/3') - (2 * Math.PI) / 3) < 1e-12);
  assert.ok(Math.abs(parseAngle('-pi') + Math.PI) < 1e-12);
  assert.ok(Math.abs(parseAngle('(1+2)*0.5') - 1.5) < 1e-12);
  assert.ok(Math.abs(parseAngle('π/2') - Math.PI / 2) < 1e-12);
});

test('parseAngle rejects anything it cannot turn into a finite number', () => {
  for (const bad of ['', 'theta', 'pi/', '1 + ', '2 pi', 'pi)', '1/0']) {
    assert.equal(parseAngle(bad), null, `${bad} must be rejected`);
  }
});

test('formatAngle names the multiples of pi an editor writes', () => {
  assert.equal(formatAngle(0), '0');
  assert.equal(formatAngle(Math.PI), 'π');
  assert.equal(formatAngle(Math.PI / 4), 'π/4');
  assert.equal(formatAngle(-Math.PI / 2), '-π/2');
  assert.equal(formatAngle(3 * Math.PI / 4), '3π/4');
  assert.equal(formatAngle(0.123456), '0.123');
});

// ---- descriptions and export ----------------------------------------------

test('describeCell names the control and the target of a cx', () => {
  const cell = buildGrid(BELL).cells[1];
  assert.match(describeCell(cell), /q0, q1/);
  assert.match(describeCell(cell), /control/);
  assert.match(describeCell(cell), /target/);
});

test('circuitToSvg emits a standalone document with wires and gate labels', () => {
  const svg = circuitToSvg(BELL);
  assert.match(svg, /^<svg xmlns="http:\/\/www\.w3\.org\/2000\/svg"/);
  assert.match(svg, /<\/svg>$/);
  assert.match(svg, />H</);
  assert.equal((svg.match(/\|q\d⟩/g) || []).length, 2, 'one label per qubit wire');
  assert.match(svg, /c\[2\]/, 'the classical register is drawn');
});

test('gate ink follows the fill, so a light box never carries white text', () => {
  const rotation = circuit(1, 0, [gate('Rz', [0], [Math.PI / 4]), gate('H', [0])]);
  const svg = circuitToSvg(rotation);
  assert.match(svg, /fill="#151935"[^>]*>RZ/, 'dark ink on the amber rotation box');
  assert.match(svg, /fill="#ffffff">H</, 'white ink on the indigo hadamard box');
});

test('every operation in the SVG is its own group with the title first', () => {
  const svg = circuitToSvg(BELL);
  const groups = svg.match(/<g><title>[^<]*<\/title>/g) || [];
  assert.equal(groups.length, 4, 'one group per operation');
  assert.match(groups[0], /<g><title>H q0/);
  assert.match(groups[1], /<g><title>X q0, q1 — q0 control, q1 target/);
});

test('circuitToSvg escapes the text it puts in a title', () => {
  const svg = circuitToSvg(BELL, { labels: { circuit: '<script>' } });
  assert.ok(!svg.includes('<script>'));
  assert.match(svg, /&lt;script&gt;/);
});

// ---- the element -----------------------------------------------------------

function mount(source) {
  const el = new TfQuantumCircuit();
  document.body.appendChild(el);
  el.circuit = source;
  return el;
}

test('the DOM layer is a real grid with one labelled cell per moment', () => {
  const el = mount(BELL);
  const grid = el.querySelector('[role="grid"]');
  assert.ok(grid);
  const rows = el.querySelectorAll('[role="row"]');
  assert.equal(rows.length, 3, 'two qubit rows plus the classical wire');
  const first = el.querySelector('[data-row="0"][data-column="0"]');
  assert.match(first.getAttribute('aria-label'), /^H q0/);
  const idle = el.querySelector('[data-row="1"][data-column="0"]');
  assert.match(idle.getAttribute('aria-label'), /Empty cell/);
  el.remove();
});

test('arrow keys move the caret and the roving tabindex follows it', () => {
  const el = mount(BELL);
  const grid = el.querySelector('[role="grid"]');
  assert.equal(el.querySelector('[data-row="0"][data-column="0"]').tabIndex, 0);
  grid.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'ArrowRight', bubbles: true }));
  assert.equal(el.querySelector('[data-row="0"][data-column="1"]').tabIndex, 0);
  assert.equal(el.querySelector('[data-row="0"][data-column="0"]').tabIndex, -1);
  grid.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'ArrowDown', bubbles: true }));
  assert.equal(el.querySelector('[data-row="1"][data-column="1"]').tabIndex, 0);
  el.remove();
});

test('a pointer press moves the roving tabindex onto the pressed cell', () => {
  const el = mount(BELL);
  el.querySelector('[data-row="1"][data-column="2"]')
    .dispatchEvent(new window.Event('pointerdown', { bubbles: true }));
  assert.equal(el.querySelector('[data-row="1"][data-column="2"]').tabIndex, 0);
  assert.equal(el.querySelector('[data-row="0"][data-column="0"]').tabIndex, -1);
  el.remove();
});

test('Delete removes the gate under the caret and emits the new circuit', () => {
  const el = mount(BELL);
  const grid = el.querySelector('[role="grid"]');
  let emitted = null;
  el.addEventListener('change', (event) => { emitted = event.detail.circuit; });
  grid.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Delete', bubbles: true }));
  assert.ok(emitted, 'change fired');
  assert.equal(emitted.ops.length, 3);
  assert.equal(el.circuit.ops.length, 3);
  el.remove();
});

test('deleteOps and duplicateOp are edits, not circuit reassignments', () => {
  const el = mount(BELL);
  let emitted = null;
  el.addEventListener('change', (event) => { emitted = event.detail.circuit; });

  el.duplicateOp(0);
  assert.equal(emitted.ops.length, 5, 'the copy is in the program');
  assert.deepEqual(el.selection, [1], 'and is selected, so the panel keeps describing something');
  assert.equal(buildGrid(el.circuit).cells[1].column, 1, 'ASAP puts it in the next free column');

  el.deleteOps([1]);
  assert.equal(el.circuit.ops.length, 4);
  el.undo();
  assert.equal(el.circuit.ops.length, 5, 'both edits sit on the undo stack');

  // Nothing the IR does not hold, and nothing at all on a read-only grid.
  el.deleteOps([99]);
  assert.equal(el.circuit.ops.length, 5);
  el.setAttribute('readonly', '');
  el.deleteOps([0]);
  el.duplicateOp(0);
  assert.equal(el.circuit.ops.length, 5);
  el.remove();
});

test('undo restores the deleted gate, redo takes it away again', () => {
  const el = mount(BELL);
  const grid = el.querySelector('[role="grid"]');
  grid.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Delete', bubbles: true }));
  assert.equal(el.circuit.ops.length, 3);
  el.undo();
  assert.equal(el.circuit.ops.length, 4);
  el.redo();
  assert.equal(el.circuit.ops.length, 3);
  el.remove();
});

test('assigning a circuit clears the undo history of the previous one', () => {
  const el = mount(BELL);
  const grid = el.querySelector('[role="grid"]');
  grid.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Delete', bubbles: true }));
  assert.equal(el.canUndo, true);
  el.circuit = BELL;
  assert.equal(el.canUndo, false);
  el.remove();
});

test('a readonly circuit hides the palette and ignores destructive keys', () => {
  const el = new TfQuantumCircuit();
  el.setAttribute('readonly', '');
  document.body.appendChild(el);
  el.circuit = BELL;
  assert.equal(el.querySelector('.tf-qc__palette').hidden, true);
  el.querySelector('[role="grid"]')
    .dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Delete', bubbles: true }));
  assert.equal(el.circuit.ops.length, 4);
  el.remove();
});

test('the state a host pushes in becomes a per-wire readout, not a simulation', () => {
  const el = mount(BELL);
  el.state = { bloch: [0, 0, 1, 1, 0, 0] };
  const readouts = el.querySelectorAll('.tf-qc__readout--live');
  assert.equal(readouts.length, 2);
  assert.match(readouts[0].getAttribute('aria-label'), /q0: P\(1\) = 0\.0%/);
  assert.match(readouts[1].getAttribute('aria-label'), /q1: P\(1\) = 50\.0%/);
  el.remove();
});

test('a pointer press selects the operation under it and announces the column', () => {
  const el = mount(BELL);
  let column = null;
  el.addEventListener('column-click', (event) => { column = event.detail.column; });
  el.querySelector('[data-row="0"][data-column="1"]')
    .dispatchEvent(new window.Event('pointerdown', { bubbles: true }));
  assert.equal(column, 1);
  assert.deepEqual(el.selection, [1]);
  assert.ok(el.querySelector('[data-row="0"][data-column="1"]').classList.contains('tf-qc__cell--selected'));
  el.remove();
});

test('the popover re-pairs a control and a target, and one undo takes it back', () => {
  const el = mount(circuit(3, 0, [gate('Cx', [0, 1])]));
  let seen = null;
  el.addEventListener('change', (event) => { seen = event.detail.circuit; });
  el.querySelector('[data-row="0"][data-column="0"]')
    .dispatchEvent(new window.Event('dblclick', { bubbles: true }));
  const selects = el.querySelectorAll('.tf-qc__popover tf-select');
  assert.equal(selects.length, 2, 'one picker per operand');
  assert.equal(selects[0].getAttribute('label'), 'control');
  assert.equal(selects[1].getAttribute('label'), 'target');
  selects[1].value = '2';
  el.querySelector('.tf-qc__popover .tf-btn-primary')
    .dispatchEvent(new window.Event('click', { bubbles: true }));
  assert.deepEqual(seen.ops[0].kind.Gate.qubits, [0, 2]);
  el.undo();
  assert.deepEqual(el.circuit.ops[0].kind.Gate.qubits, [0, 1]);
  el.remove();
});

test('picking a wire the other operand holds swaps them instead of doubling up', () => {
  const el = mount(circuit(2, 0, [gate('Cx', [0, 1])]));
  el.querySelector('[data-row="0"][data-column="0"]')
    .dispatchEvent(new window.Event('dblclick', { bubbles: true }));
  const selects = el.querySelectorAll('.tf-qc__popover tf-select');
  selects[0].value = '1';
  selects[0].dispatchEvent(new window.Event('change', { bubbles: true }));
  assert.equal(selects[1].value, '0', 'the target took the wire the control left');
  el.remove();
});

test('an angle and a wire typed in one popover are one commit', () => {
  const el = mount(circuit(3, 0, [gate('Crz', [0, 1], [0])]));
  el.querySelector('[data-row="0"][data-column="0"]')
    .dispatchEvent(new window.Event('dblclick', { bubbles: true }));
  const popover = el.querySelector('.tf-qc__popover');
  popover.querySelector('tf-input').value = 'pi/2';
  popover.querySelectorAll('tf-select')[1].value = '2';
  popover.querySelector('.tf-btn-primary').dispatchEvent(new window.Event('click', { bubbles: true }));
  const [op] = el.circuit.ops;
  assert.deepEqual(op.kind.Gate.qubits, [0, 2]);
  assert.ok(Math.abs(gateParams(op.kind.Gate.gate)[0] - Math.PI / 2) < 1e-12);
  el.undo();
  assert.deepEqual(el.circuit.ops[0].kind.Gate.qubits, [0, 1], 'one undo undoes both');
  el.remove();
});

test('the classical row carries a cell per column so the grid is not ragged', () => {
  const el = mount(BELL);
  const rows = el.querySelectorAll('[role="row"]');
  const classical = rows[rows.length - 1];
  assert.ok(classical.classList.contains('tf-qc__row--classical'));
  const qubitCells = rows[0].querySelectorAll('[role="gridcell"]').length;
  assert.equal(classical.querySelectorAll('[role="gridcell"]').length, qubitCells,
    'every row holds the aria-colcount the grid declares');
  assert.equal(classical.querySelectorAll('[role="rowheader"]').length, 1);
  const written = Array.from(classical.querySelectorAll('.tf-qc__cell--classical'))
    .filter((cell) => /measure/.test(cell.getAttribute('aria-label')));
  assert.equal(written.length, 1, 'the moment the register is written is announced');
  el.remove();
});

test('a keyframe from the simulator drives the wire readouts as well as a flat array', () => {
  const el = mount(BELL);
  el.state = { bloch: [[0, 0, 1], [1, 0, 0]] };
  const readouts = el.querySelectorAll('.tf-qc__readout--live');
  assert.equal(readouts.length, 2);
  assert.match(readouts[0].getAttribute('aria-label'), /q0: P\(1\) = 0\.0%/);
  assert.match(readouts[1].getAttribute('aria-label'), /q1: P\(1\) = 50\.0%/);
  el.remove();
});

test('a second re-pair swaps the pair on screen, not the one the popover opened with', () => {
  const el = mount(circuit(3, 0, [gate('Cx', [0, 1])]));
  el.querySelector('[data-row="0"][data-column="0"]')
    .dispatchEvent(new window.Event('dblclick', { bubbles: true }));
  const [control, target] = el.querySelectorAll('.tf-qc__popover tf-select');
  target.value = '2';
  target.dispatchEvent(new window.Event('change', { bubbles: true }));
  control.value = '2';
  control.dispatchEvent(new window.Event('change', { bubbles: true }));
  assert.equal(target.value, '0', 'the target takes the wire the control just left');
  el.querySelector('.tf-qc__popover .tf-btn-primary')
    .dispatchEvent(new window.Event('click', { bubbles: true }));
  assert.deepEqual(el.circuit.ops[0].kind.Gate.qubits, [2, 0]);
  el.remove();
});

test('moveOp anchors on the wire it was grabbed by, not the gate top', () => {
  const wide = circuit(4, 0, [gate('Cx', [0, 1])]);
  // Grabbed by the target and dropped where it already sits: nothing moves.
  assert.deepEqual(
    moveOp(wide, 0, { row: 1, column: 0, anchorRow: 1 }).ops[0].kind.Gate.qubits,
    [0, 1],
  );
  assert.deepEqual(
    moveOp(wide, 0, { row: 2, column: 0, anchorRow: 1 }).ops[0].kind.Gate.qubits,
    [1, 2],
  );
});

test('dropping a two-qubit gate on the cell it was grabbed from is a no-op', () => {
  const el = mount(circuit(4, 0, [gate('Cx', [0, 1])]));
  const target = el.querySelector('[data-row="1"][data-column="0"]');
  target.dispatchEvent(new window.Event('dragstart', { bubbles: true }));
  target.dispatchEvent(new window.Event('drop', { bubbles: true }));
  assert.deepEqual(el.circuit.ops[0].kind.Gate.qubits, [0, 1]);
  el.remove();
});

test('an undo closes a popover that still points at the operation it edited', () => {
  const el = mount(circuit(3, 0, [gate('Cx', [0, 1])]));
  el.querySelector('[data-row="0"][data-column="0"]')
    .dispatchEvent(new window.Event('dblclick', { bubbles: true }));
  assert.ok(el.querySelector('.tf-qc__popover'));
  el.querySelector('[role="grid"]')
    .dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Delete', bubbles: true }));
  el.undo();
  assert.equal(el.querySelector('.tf-qc__popover'), null);
  el.remove();
});

test('a pointer press outside the popover dismisses it', () => {
  const el = mount(circuit(3, 0, [gate('Cx', [0, 1])]));
  el.querySelector('[data-row="0"][data-column="0"]')
    .dispatchEvent(new window.Event('dblclick', { bubbles: true }));
  const popover = el.querySelector('.tf-qc__popover');
  popover.dispatchEvent(new window.Event('pointerdown', { bubbles: true }));
  assert.ok(el.querySelector('.tf-qc__popover'), 'a press inside keeps it open');
  el.querySelector('[data-row="2"][data-column="3"]')
    .dispatchEvent(new window.Event('pointerdown', { bubbles: true }));
  assert.equal(el.querySelector('.tf-qc__popover'), null);
  el.remove();
});
