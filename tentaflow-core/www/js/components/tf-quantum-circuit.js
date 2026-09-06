// =============================================================================
// File: components/tf-quantum-circuit.js
// Description: <tf-quantum-circuit> — the circuit editor of TentaQuant
//              (plan §13.2, §13.4; mockups Q06/Q07/Q15). Qubit rows × moment
//              columns painted on a <canvas>, with a DOM grid layered on top so
//              that focus, ARIA and the keyboard are real elements and not a
//              picture: canvas is presentation, the DOM is the truth (§13.4).
//
//              The component does NOT simulate. `circuit` is the IR exactly as
//              the wasm facade's parse() returns it, and `state` (Bloch vectors)
//              is pushed in by the host.
//
//              The grid is an ASAP schedule of the IR — the layout is derived,
//              never stored (plan §6.1 keeps layout_json outside the IR), so a
//              gate dropped on an idle wire settles into the earliest column in
//              which its qubits are free.
//
//  Properties: circuit  — IR JSON {numQubits, numClbits, qubitRegisters,
//                         clbitRegisters, ops}; assigning it resets undo.
//              state    — a Bloch payload: the flat `blochVectors()` array, a
//                         keyframe's nested `bloch: [[x,y,z], ...]`, or either
//                         wrapped in `{bloch}`; drawn as a per-wire readout at
//                         the right edge.
//              step     — how many columns have been applied; earlier columns
//                         render dimmed, the step column highlighted.
//              playhead — fractional column position for the evolution
//                         animation (§13.6); null hides the head.
//              selection — array of op indices; also settable by the user.
//              labels   — i18n dict, English fallbacks only (see DEFAULT_LABELS).
//  Attributes: readonly, palette="none" (hide the built-in palette),
//              min-columns, aria-label.
//  Methods   : undo(), redo(), deleteOps(indices), duplicateOp(index), toSvg(),
//              focusCell(row, column).
//  Editing   : drag from the palette or between cells places and translates an
//              operation; Enter or double-click opens the cell popover, which
//              edits angles, re-pairs the control and target of a 2-qubit gate
//              and re-points a measurement at another classical bit.
//  Events    : "change"        detail {circuit} — after every edit,
//              "select"        detail {indices},
//              "column-click"  detail {column} — the step slider's partner.
//
// Example: const qc = document.querySelector('tf-quantum-circuit');
//          qc.circuit = (await parse(source)).circuit;
//          qc.addEventListener('change', (e) => save(e.detail.circuit));
// =============================================================================

import { cssToken } from './shared-styles.js';
import { blochVectorList } from './tf-bloch-sphere.js';
import './tf-input.js';
import './tf-select.js';

// Geometry, in CSS pixels. The DOM grid and the canvas share these numbers, so
// a cell and the gate painted under it can never drift apart.
export const LABEL_W = 56;
export const COL_W = 56;
export const ROW_H = 46;
const GATE_SIZE = 34;
const PAD = 14;
const CTRL_R = 6;
const TARGET_R = 13;

const DEFAULT_LABELS = {
  circuit: 'Quantum circuit',
  palette: 'Gate palette',
  empty: 'Empty cell',
  add_gate: 'Add gate',
  remove: 'Remove',
  apply: 'Apply',
  control: 'control',
  target: 'target',
  measure: 'measure',
  reset: 'reset',
  barrier: 'barrier',
  global_phase: 'global phase',
  classical: 'classical register',
  qubit: 'qubit',
  clbit: 'classical bit',
  operand: 'operand',
  column: 'column',
  conditional: 'conditional',
  readout: 'readout',
  mixed: 'mixed',
  group_single: '1-qubit',
  group_rotation: 'rotations',
  group_two: '2-qubit',
  group_meta: 'measurement',
  bad_angle: 'Expected a number or an expression with pi',
};

// ---------------------------------------------------------------------------
// Gate catalogue — the stdgates.inc subset the IR carries (tentaflow-quantum
// lowers ccx/cswap/u1/u2/u3/phase/cphase into exactly this set).
// `tone` selects a --tf-q-gate-* colour; `control` marks operand 0 as the
// control dot; `target` picks the glyph drawn on operand 1.
// ---------------------------------------------------------------------------

export const GATE_INFO = {
  Id: { label: 'I', tone: 'plain', arity: 1, params: [], group: 'single' },
  X: { label: 'X', tone: 'x', arity: 1, params: [], group: 'single' },
  Y: { label: 'Y', tone: 'z', arity: 1, params: [], group: 'single' },
  Z: { label: 'Z', tone: 'z', arity: 1, params: [], group: 'single' },
  H: { label: 'H', tone: 'h', arity: 1, params: [], group: 'single' },
  S: { label: 'S', tone: 's', arity: 1, params: [], group: 'single' },
  Sdg: { label: 'S†', tone: 's', arity: 1, params: [], group: 'single' },
  T: { label: 'T', tone: 's', arity: 1, params: [], group: 'single' },
  Tdg: { label: 'T†', tone: 's', arity: 1, params: [], group: 'single' },
  Sx: { label: '√X', tone: 'x', arity: 1, params: [], group: 'single' },
  SxDg: { label: '√X†', tone: 'x', arity: 1, params: [], group: 'single' },
  P: { label: 'P', tone: 'rot', arity: 1, params: ['λ'], group: 'rotation' },
  Rx: { label: 'RX', tone: 'rot', arity: 1, params: ['θ'], group: 'rotation' },
  Ry: { label: 'RY', tone: 'rot', arity: 1, params: ['θ'], group: 'rotation' },
  Rz: { label: 'RZ', tone: 'rot', arity: 1, params: ['θ'], group: 'rotation' },
  U: { label: 'U', tone: 'u', arity: 1, params: ['θ', 'φ', 'λ'], group: 'rotation' },
  Cx: { label: 'X', tone: 'x', arity: 2, params: [], group: 'two', control: true, target: 'plus' },
  Cy: { label: 'Y', tone: 'z', arity: 2, params: [], group: 'two', control: true, target: 'box' },
  Cz: { label: 'Z', tone: 'z', arity: 2, params: [], group: 'two', control: true, target: 'dot' },
  Ch: { label: 'H', tone: 'h', arity: 2, params: [], group: 'two', control: true, target: 'box' },
  Swap: { label: 'SWAP', tone: 'plain', arity: 2, params: [], group: 'two', target: 'swap' },
  Cp: { label: 'P', tone: 'rot', arity: 2, params: ['λ'], group: 'two', control: true, target: 'box' },
  Crx: { label: 'RX', tone: 'rot', arity: 2, params: ['θ'], group: 'two', control: true, target: 'box' },
  Cry: { label: 'RY', tone: 'rot', arity: 2, params: ['θ'], group: 'two', control: true, target: 'box' },
  Crz: { label: 'RZ', tone: 'rot', arity: 2, params: ['θ'], group: 'two', control: true, target: 'box' },
  Cu: { label: 'U', tone: 'u', arity: 2, params: ['θ', 'φ', 'λ', 'γ'], group: 'two', control: true, target: 'box' },
};

// Non-gate operations the palette offers alongside the unitaries.
export const META_OPS = ['Measure', 'Reset', 'Barrier'];

/// Which ink reads on a gate box. The amber rotations and the grey utility
/// boxes are light enough that white text disappears on them, so the ink is
/// chosen by the FILL and not by the theme — the same rule holds for the dark
/// dashboard and for the light publication export.
function gateTextColor(tone, colors) {
  return tone === 'rot' || tone === 'plain' ? colors.inkOnLight : colors.inkOnDark;
}

const TONE_TOKEN = {
  h: '--tf-q-gate-h',
  x: '--tf-q-gate-x',
  z: '--tf-q-gate-z',
  s: '--tf-q-gate-s',
  rot: '--tf-q-gate-rot',
  u: '--tf-q-gate-u',
  plain: '--tf-q-gate-plain',
};

// ---------------------------------------------------------------------------
// Gate ⇄ JSON. serde writes a unit variant as "H", a one-parameter variant as
// {"Rx": 0.78} and a multi-parameter one as {"U": [θ, φ, λ]}.
// ---------------------------------------------------------------------------

export function gateId(gate) {
  if (typeof gate === 'string') return gate;
  if (gate && typeof gate === 'object') {
    const keys = Object.keys(gate);
    return keys.length === 1 ? keys[0] : null;
  }
  return null;
}

export function gateParams(gate) {
  if (!gate || typeof gate !== 'object') return [];
  const value = Object.values(gate)[0];
  if (Array.isArray(value)) return value.map(Number);
  return [Number(value)];
}

export function makeGate(id, params = []) {
  const info = GATE_INFO[id];
  if (!info) return null;
  if (info.params.length === 0) return id;
  const values = info.params.map((_, i) => (Number.isFinite(Number(params[i])) ? Number(params[i]) : 0));
  return { [id]: values.length === 1 ? values[0] : values };
}

// ---------------------------------------------------------------------------
// Angles. The IR carries f64, so a symbolic parameter cannot be stored — the
// editor accepts numbers and pi expressions and rejects everything else rather
// than silently writing a zero.
// ---------------------------------------------------------------------------

const ANGLE_TOKEN = /\s*(\d+\.?\d*(?:[eE][+-]?\d+)?|pi|π|[-+*/()])/y;

export function parseAngle(text) {
  const src = String(text ?? '').trim();
  if (!src) return null;
  const tokens = [];
  let at = 0;
  while (at < src.length) {
    ANGLE_TOKEN.lastIndex = at;
    const match = ANGLE_TOKEN.exec(src);
    if (!match) return null;
    at = ANGLE_TOKEN.lastIndex;
    tokens.push(match[1]);
  }
  const output = [];
  const ops = [];
  const precedence = { '+': 1, '-': 1, '*': 2, '/': 2, u: 3 };
  let expectValue = true;
  for (const token of tokens) {
    if (token === 'pi' || token === 'π') {
      if (!expectValue) return null;
      output.push(Math.PI);
      expectValue = false;
    } else if (/^\d/.test(token)) {
      if (!expectValue) return null;
      output.push(Number(token));
      expectValue = false;
    } else if (token === '(') {
      if (!expectValue) return null;
      ops.push(token);
    } else if (token === ')') {
      if (expectValue) return null;
      while (ops.length && ops[ops.length - 1] !== '(') output.push(ops.pop());
      if (!ops.length) return null;
      ops.pop();
      expectValue = false;
    } else {
      const operator = expectValue ? (token === '-' ? 'u' : token === '+' ? null : false) : token;
      if (operator === false) return null;
      if (operator === null) continue;
      while (ops.length) {
        const top = ops[ops.length - 1];
        if (top === '(' || precedence[top] < precedence[operator]) break;
        if (operator === 'u' && precedence[top] === precedence[operator]) break;
        output.push(ops.pop());
      }
      ops.push(operator);
      expectValue = true;
    }
  }
  if (expectValue) return null;
  while (ops.length) {
    const top = ops.pop();
    if (top === '(') return null;
    output.push(top);
  }
  const stack = [];
  for (const item of output) {
    if (typeof item === 'number') {
      stack.push(item);
      continue;
    }
    if (item === 'u') {
      if (!stack.length) return null;
      stack.push(-stack.pop());
      continue;
    }
    if (stack.length < 2) return null;
    const b = stack.pop();
    const a = stack.pop();
    if (item === '+') stack.push(a + b);
    else if (item === '-') stack.push(a - b);
    else if (item === '*') stack.push(a * b);
    else stack.push(a / b);
  }
  if (stack.length !== 1 || !Number.isFinite(stack[0])) return null;
  return stack[0];
}

/// Renders an angle the way the palette and the SVG export label it: as a
/// multiple of pi when it is one, otherwise as a short decimal.
export function formatAngle(value) {
  const number = Number(value);
  if (!Number.isFinite(number)) return '0';
  if (Math.abs(number) < 1e-9) return '0';
  const ratio = number / Math.PI;
  for (let denominator = 1; denominator <= 12; denominator += 1) {
    const numerator = ratio * denominator;
    if (Math.abs(numerator - Math.round(numerator)) < 1e-9) {
      const top = Math.round(numerator);
      if (Math.abs(top) > 24) break;
      const sign = top < 0 ? '-' : '';
      const magnitude = Math.abs(top);
      const head = magnitude === 1 ? 'π' : `${magnitude}π`;
      return denominator === 1 ? `${sign}${head}` : `${sign}${head}/${denominator}`;
    }
  }
  return number.toFixed(3).replace(/0+$/, '').replace(/\.$/, '');
}

// ---------------------------------------------------------------------------
// IR → grid. The schedule is ASAP over three kinds of resource: qubit rows,
// the rows a multi-qubit link crosses (so a link never runs through a gate box)
// and classical bits (so a measure and the guard that reads it keep their
// order when the grid is read back).
// ---------------------------------------------------------------------------

function conditionClbits(condition, circuit) {
  if (!condition || typeof condition !== 'object') return [];
  if (condition.Bit) return [condition.Bit.clbit];
  if (condition.Register) {
    const register = (circuit.clbitRegisters || [])[condition.Register.register];
    if (!register) return [];
    return Array.from({ length: register.size }, (_, i) => register.start + i);
  }
  return [];
}

function cellOf(op, index, circuit) {
  const kind = op && op.kind;
  const clbits = (op.conditions || []).flatMap((c) => conditionClbits(c, circuit));
  if (kind && kind.Gate) {
    const qubits = kind.Gate.qubits.slice();
    return {
      index, op, type: 'gate', gate: kind.Gate.gate, id: gateId(kind.Gate.gate),
      params: gateParams(kind.Gate.gate), qubits, clbits, span: true,
    };
  }
  if (kind && kind.Measure) {
    return {
      index, op, type: 'measure', qubits: [kind.Measure.qubit],
      clbits: clbits.concat([kind.Measure.clbit]), span: false,
    };
  }
  if (kind && kind.Reset) {
    return { index, op, type: 'reset', qubits: [kind.Reset.qubit], clbits, span: false };
  }
  if (kind && kind.Barrier) {
    return { index, op, type: 'barrier', qubits: kind.Barrier.qubits.slice(), clbits, span: true };
  }
  if (kind && Object.prototype.hasOwnProperty.call(kind, 'GlobalPhase')) {
    return {
      index, op, type: 'gphase', qubits: [], clbits, span: true,
      params: [Number(kind.GlobalPhase)],
    };
  }
  return null;
}

/// Schedules the IR into moment columns. Pure — the element only draws it.
export function buildGrid(circuit) {
  const source = circuit && typeof circuit === 'object' ? circuit : {};
  const numQubits = Number(source.numQubits) || 0;
  const numClbits = Number(source.numClbits) || 0;
  const qubitFree = new Array(numQubits).fill(0);
  const clbitFree = new Array(numClbits).fill(0);
  const cells = [];
  let columns = 0;
  (Array.isArray(source.ops) ? source.ops : []).forEach((op, index) => {
    const cell = cellOf(op, index, source);
    if (!cell) return;
    const rows = cell.qubits.length
      ? (cell.span
        ? rangeOf(Math.min(...cell.qubits), Math.max(...cell.qubits))
        : cell.qubits.slice())
      : rangeOf(0, Math.max(0, numQubits - 1));
    let column = 0;
    for (const row of rows) column = Math.max(column, qubitFree[row] || 0);
    for (const clbit of cell.clbits) column = Math.max(column, clbitFree[clbit] || 0);
    for (const row of rows) qubitFree[row] = column + 1;
    for (const clbit of cell.clbits) clbitFree[clbit] = column + 1;
    cell.column = column;
    cell.rows = rows;
    cell.minRow = rows.length ? rows[0] : 0;
    cell.maxRow = rows.length ? rows[rows.length - 1] : 0;
    columns = Math.max(columns, column + 1);
    cells.push(cell);
  });
  return { numQubits, numClbits, columns, cells };
}

function rangeOf(from, to) {
  const out = [];
  for (let i = from; i <= to; i += 1) out.push(i);
  return out;
}

/// The op that owns a cell of the grid, or null when the cell is idle.
export function cellAt(grid, row, column) {
  return grid.cells.find((cell) => cell.column === column && cell.rows.includes(row)) || null;
}

/// Rebuilds the IR from a scheduled grid: column-major, top row first. Because
/// the schedule already respects every qubit and clbit dependency, this order
/// is the same program — for a circuit written in normal form, byte for byte.
export function gridToCircuit(circuit, grid) {
  const ordered = grid.cells.slice().sort((a, b) => (
    a.column - b.column || a.minRow - b.minRow || a.index - b.index
  ));
  return { ...circuit, ops: ordered.map((cell) => cell.op) };
}

// ---------------------------------------------------------------------------
// Edits. Every one is a pure IR → IR function so the element can push the
// result on the undo stack and re-render from a single code path.
// ---------------------------------------------------------------------------

/// Inserts one operation so that it schedules at or after (column, row).
export function insertOp(circuit, op, at) {
  const grid = buildGrid(circuit);
  const ordered = grid.cells.slice().sort((a, b) => (
    a.column - b.column || a.minRow - b.minRow || a.index - b.index
  ));
  const before = [];
  const after = [];
  for (const cell of ordered) {
    const precedes = cell.column < at.column
      || (cell.column === at.column && cell.minRow < at.row);
    (precedes ? before : after).push(cell.op);
  }
  return { ...circuit, ops: [...before, op, ...after] };
}

export function removeOps(circuit, indices) {
  const drop = new Set(indices);
  return { ...circuit, ops: (circuit.ops || []).filter((_, i) => !drop.has(i)) };
}

/// Moves the operation at `index` so the wire it was grabbed by — `at.anchorRow`,
/// the gate's top wire when the caller names none — lands on `at.row`; every
/// other operand travels by the same delta. Dragging a controlled gate by its
/// target must not shift the pair, so the drag path always names the grabbed row.
export function moveOp(circuit, index, at) {
  const grid = buildGrid(circuit);
  const cell = grid.cells.find((c) => c.index === index);
  if (!cell) return circuit;
  const anchor = Number.isInteger(at.anchorRow) ? at.anchorRow : cell.minRow;
  const shift = at.row - anchor;
  const moved = shiftOpRows(cell.op, shift, Number(circuit.numQubits) || 0);
  if (!moved) return circuit;
  const rest = { ...circuit, ops: (circuit.ops || []).filter((_, i) => i !== index) };
  // Column ordering compares against the moved gate's own top wire, which is
  // only `at.row` when the anchor was that top wire to begin with.
  return insertOp(rest, moved, { column: at.column, row: cell.minRow + shift });
}

function shiftOpRows(op, shift, numQubits) {
  if (!shift) return op;
  const moveRow = (q) => q + shift;
  const inRange = (list) => list.every((q) => q >= 0 && q < numQubits);
  const kind = op.kind;
  if (kind.Gate) {
    const qubits = kind.Gate.qubits.map(moveRow);
    if (!inRange(qubits) || new Set(qubits).size !== qubits.length) return null;
    return { ...op, kind: { Gate: { ...kind.Gate, qubits } } };
  }
  if (kind.Measure) {
    const qubit = moveRow(kind.Measure.qubit);
    if (!inRange([qubit])) return null;
    return { ...op, kind: { Measure: { ...kind.Measure, qubit } } };
  }
  if (kind.Reset) {
    const qubit = moveRow(kind.Reset.qubit);
    if (!inRange([qubit])) return null;
    return { ...op, kind: { Reset: { qubit } } };
  }
  return op;
}

/// Rewrites which wires an operation acts on. Dragging only TRANSLATES a gate
/// — every operand moves by the same delta — so re-pairing a control with a
/// non-adjacent target, or pointing a measurement at another classical bit,
/// has to be an edit of its own. Rejects anything the IR cannot hold (a repeated
/// qubit, a wire outside the register), leaving the circuit untouched.
export function setOpWires(circuit, index, wires) {
  const ops = (circuit.ops || []).slice();
  const op = ops[index];
  if (!op || !op.kind) return circuit;
  const numQubits = Number(circuit.numQubits) || 0;
  const numClbits = Number(circuit.numClbits) || 0;
  const requested = Array.isArray(wires && wires.qubits) ? wires.qubits.map(Number) : null;
  const usable = (list, arity) => list.length === arity
    && new Set(list).size === arity
    && list.every((q) => Number.isInteger(q) && q >= 0 && q < numQubits);
  const kind = op.kind;
  if (kind.Gate) {
    const qubits = requested || kind.Gate.qubits.slice();
    if (!usable(qubits, kind.Gate.qubits.length)) return circuit;
    ops[index] = { ...op, kind: { Gate: { ...kind.Gate, qubits } } };
  } else if (kind.Measure) {
    const qubit = requested ? requested[0] : kind.Measure.qubit;
    const raw = Number(wires && wires.clbit);
    const clbit = Number.isInteger(raw) ? raw : kind.Measure.clbit;
    if (!usable([qubit], 1) || clbit < 0 || clbit >= numClbits) return circuit;
    ops[index] = { ...op, kind: { Measure: { qubit, clbit } } };
  } else if (kind.Reset) {
    const qubit = requested ? requested[0] : kind.Reset.qubit;
    if (!usable([qubit], 1)) return circuit;
    ops[index] = { ...op, kind: { Reset: { qubit } } };
  } else {
    return circuit;
  }
  return { ...circuit, ops };
}

/// Rewrites the parameters of a parametric gate in place.
export function setOpParams(circuit, index, params) {
  const ops = (circuit.ops || []).slice();
  const op = ops[index];
  if (!op || !op.kind || !op.kind.Gate) return circuit;
  const id = gateId(op.kind.Gate.gate);
  const gate = makeGate(id, params);
  if (!gate) return circuit;
  ops[index] = { ...op, kind: { Gate: { ...op.kind.Gate, gate } } };
  return { ...circuit, ops };
}

// ---------------------------------------------------------------------------
// Undo / redo. A snapshot stack rather than a command log: an IR of a few
// hundred operations is cheap to clone, and a snapshot can never drift out of
// sync with the document the way an inverse command can.
// ---------------------------------------------------------------------------

export class UndoStack {
  constructor(initial, limit = 100) {
    this._limit = limit;
    this._entries = [clone(initial)];
    this._at = 0;
  }

  get current() { return clone(this._entries[this._at]); }

  get canUndo() { return this._at > 0; }

  get canRedo() { return this._at < this._entries.length - 1; }

  push(value) {
    this._entries = this._entries.slice(0, this._at + 1);
    this._entries.push(clone(value));
    if (this._entries.length > this._limit) this._entries.shift();
    this._at = this._entries.length - 1;
  }

  reset(value) {
    this._entries = [clone(value)];
    this._at = 0;
  }

  undo() {
    if (!this.canUndo) return null;
    this._at -= 1;
    return this.current;
  }

  redo() {
    if (!this.canRedo) return null;
    this._at += 1;
    return this.current;
  }
}

function clone(value) {
  return value == null ? value : JSON.parse(JSON.stringify(value));
}

// ---------------------------------------------------------------------------
// Descriptions — one source for the ARIA label, the popover title and the SVG
// <title>, so the screen reader and the export never disagree.
// ---------------------------------------------------------------------------

export function describeCell(cell, labels = DEFAULT_LABELS) {
  const qubits = cell.qubits.map((q) => `q${q}`).join(', ');
  if (cell.type === 'gate') {
    const info = GATE_INFO[cell.id];
    const name = info ? info.label : cell.id;
    const params = cell.params.length ? `(${cell.params.map(formatAngle).join(', ')})` : '';
    const roles = info && info.arity === 2 && info.control
      ? ` — q${cell.qubits[0]} ${labels.control}, q${cell.qubits[1]} ${labels.target}`
      : '';
    const guard = (cell.op.conditions || []).length ? `, ${labels.conditional}` : '';
    return `${name}${params} ${qubits}${roles}${guard}`;
  }
  if (cell.type === 'measure') {
    return `${labels.measure} q${cell.qubits[0]} → c${cell.op.kind.Measure.clbit}`;
  }
  if (cell.type === 'reset') return `${labels.reset} ${qubits}`;
  if (cell.type === 'barrier') return `${labels.barrier} ${qubits}`;
  return `${labels.global_phase} ${formatAngle(cell.params[0])}`;
}

// ---------------------------------------------------------------------------
// SVG export. Pure: the element resolves the live tokens and hands them in, so
// the same function serves the publication theme of §13.6 with another palette.
// ---------------------------------------------------------------------------

const SVG_FALLBACK_COLORS = {
  bg: '#ffffff', wire: '#4b5168', text: '#151935', label: '#4b5168',
  inkOnDark: '#ffffff', inkOnLight: '#151935',
  h: '#6366f1', x: '#ef4444', z: '#0ea5e9', s: '#14b8a6',
  rot: '#f59e0b', u: '#a78bfa', plain: '#8b90ab',
};

export function circuitToSvg(circuit, options = {}) {
  const colors = { ...SVG_FALLBACK_COLORS, ...(options.colors || {}) };
  const labels = { ...DEFAULT_LABELS, ...(options.labels || {}) };
  const grid = buildGrid(circuit);
  const rows = grid.numQubits + (grid.numClbits ? 1 : 0);
  const columns = Math.max(grid.columns, 1);
  const width = PAD * 2 + LABEL_W + columns * COL_W;
  const height = PAD * 2 + rows * ROW_H;
  const rowY = (row) => PAD + row * ROW_H + ROW_H / 2;
  const colX = (column) => PAD + LABEL_W + column * COL_W + COL_W / 2;
  const frame = [];
  const groups = [];
  frame.push(`<rect width="${width}" height="${height}" fill="${colors.bg}"/>`);
  for (let row = 0; row < grid.numQubits; row += 1) {
    frame.push(`<text x="${PAD + LABEL_W - 10}" y="${rowY(row) + 4}" text-anchor="end" `
      + `font-family="monospace" font-size="12" fill="${colors.label}">|q${row}⟩</text>`);
    frame.push(`<line x1="${PAD + LABEL_W}" y1="${rowY(row)}" x2="${width - PAD}" y2="${rowY(row)}" `
      + `stroke="${colors.wire}" stroke-width="1.5"/>`);
  }
  if (grid.numClbits) {
    const y = rowY(grid.numQubits);
    frame.push(`<text x="${PAD + LABEL_W - 10}" y="${y + 4}" text-anchor="end" `
      + `font-family="monospace" font-size="12" fill="${colors.label}">c[${grid.numClbits}]</text>`);
    frame.push(`<line x1="${PAD + LABEL_W}" y1="${y - 2}" x2="${width - PAD}" y2="${y - 2}" `
      + `stroke="${colors.wire}" stroke-width="1"/>`);
    frame.push(`<line x1="${PAD + LABEL_W}" y1="${y + 2}" x2="${width - PAD}" y2="${y + 2}" `
      + `stroke="${colors.wire}" stroke-width="1"/>`);
  }
  for (const cell of grid.cells) {
    const x = colX(cell.column);
    // Each operation is its own <g> whose FIRST child is the <title>: that is
    // what makes the tooltip belong to the gate rather than to the document.
    const parts = [];
    groups.push({ cell, parts });
    if (cell.type === 'barrier') {
      parts.push(`<line x1="${x}" y1="${rowY(cell.minRow) - ROW_H / 2 + 4}" x2="${x}" `
        + `y2="${rowY(cell.maxRow) + ROW_H / 2 - 4}" stroke="${colors.plain}" `
        + 'stroke-width="2" stroke-dasharray="4 4"/>');
      continue;
    }
    if (cell.type === 'gphase') {
      parts.push(svgBox(x, PAD + ROW_H / 2, `gφ(${formatAngle(cell.params[0])})`, colors.plain, gateTextColor('plain', colors)));
      continue;
    }
    if (cell.type === 'measure') {
      const clbitY = rowY(grid.numQubits);
      parts.push(`<line x1="${x}" y1="${rowY(cell.qubits[0])}" x2="${x}" y2="${clbitY}" `
        + `stroke="${colors.wire}" stroke-width="1.5"/>`);
      parts.push(svgBox(x, rowY(cell.qubits[0]), 'M', colors.plain, gateTextColor('plain', colors)));
      continue;
    }
    if (cell.type === 'reset') {
      parts.push(svgBox(x, rowY(cell.qubits[0]), '|0⟩', colors.plain, gateTextColor('plain', colors)));
      continue;
    }
    const info = GATE_INFO[cell.id] || { label: cell.id, tone: 'plain', arity: 1 };
    const tint = colors[info.tone] || colors.plain;
    if (info.arity === 2) {
      parts.push(`<line x1="${x}" y1="${rowY(cell.qubits[0])}" x2="${x}" y2="${rowY(cell.qubits[1])}" `
        + `stroke="${colors.text}" stroke-width="2"/>`);
      if (info.control) {
        parts.push(`<circle cx="${x}" cy="${rowY(cell.qubits[0])}" r="${CTRL_R}" fill="${colors.text}"/>`);
        if (info.target === 'plus') {
          const y = rowY(cell.qubits[1]);
          parts.push(`<circle cx="${x}" cy="${y}" r="${TARGET_R}" fill="none" stroke="${colors.text}" stroke-width="2"/>`);
          parts.push(`<line x1="${x - TARGET_R}" y1="${y}" x2="${x + TARGET_R}" y2="${y}" stroke="${colors.text}" stroke-width="2"/>`);
          parts.push(`<line x1="${x}" y1="${y - TARGET_R}" x2="${x}" y2="${y + TARGET_R}" stroke="${colors.text}" stroke-width="2"/>`);
        } else if (info.target === 'dot') {
          parts.push(`<circle cx="${x}" cy="${rowY(cell.qubits[1])}" r="${CTRL_R}" fill="${colors.text}"/>`);
        } else {
          parts.push(svgBox(x, rowY(cell.qubits[1]), svgGateLabel(info, cell), tint, gateTextColor(info.tone, colors)));
        }
      } else {
        for (const qubit of cell.qubits) parts.push(svgCross(x, rowY(qubit), colors.text));
      }
      continue;
    }
    parts.push(svgBox(x, rowY(cell.qubits[0]), svgGateLabel(info, cell), tint, gateTextColor(info.tone, colors)));
  }
  const drawn = groups
    .map(({ cell, parts }) => `<g><title>${escapeXml(describeCell(cell, labels))}</title>`
      + `${parts.join('')}</g>`)
    .join('');
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" `
    + `viewBox="0 0 ${width} ${height}" role="img" aria-label="${escapeXml(labels.circuit)}">`
    + frame.join('') + drawn + '</svg>';
}

function svgGateLabel(info, cell) {
  if (!cell.params || !cell.params.length) return info.label;
  return `${info.label}(${cell.params.map(formatAngle).join(',')})`;
}

function svgBox(cx, cy, text, fill, ink) {
  const wide = text.length > 3;
  const w = wide ? Math.min(COL_W - 6, 12 + text.length * 6) : GATE_SIZE;
  const size = wide ? 9 : 13;
  return `<rect x="${cx - w / 2}" y="${cy - GATE_SIZE / 2}" width="${w}" height="${GATE_SIZE}" rx="7" `
    + `fill="${fill}"/><text x="${cx}" y="${cy + size / 3}" text-anchor="middle" `
    + `font-family="monospace" font-weight="700" font-size="${size}" fill="${ink}">`
    + `${escapeXml(text)}</text>`;
}

function svgCross(cx, cy, color) {
  const r = 8;
  return `<line x1="${cx - r}" y1="${cy - r}" x2="${cx + r}" y2="${cy + r}" stroke="${color}" stroke-width="2"/>`
    + `<line x1="${cx - r}" y1="${cy + r}" x2="${cx + r}" y2="${cy - r}" stroke="${color}" stroke-width="2"/>`;
}

function escapeXml(text) {
  return String(text)
    .replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;');
}

// ---------------------------------------------------------------------------
// The element
// ---------------------------------------------------------------------------

const EMPTY_CIRCUIT = {
  qubitRegisters: [], clbitRegisters: [], numQubits: 0, numClbits: 0, ops: [],
};

class TfQuantumCircuit extends HTMLElement {
  static get observedAttributes() {
    return ['readonly', 'palette', 'min-columns', 'aria-label'];
  }

  constructor() {
    super();
    this._circuit = clone(EMPTY_CIRCUIT);
    this._grid = buildGrid(this._circuit);
    this._undo = new UndoStack(this._circuit);
    this._labels = { ...DEFAULT_LABELS };
    this._state = null;
    this._step = null;
    this._playhead = null;
    this._selection = [];
    this._caret = { row: 0, column: 0 };
    this._anchor = null;
    this._drag = null;
    this._popover = null;
    this._built = false;
    this._onKeyDown = this._onKeyDown.bind(this);
    this._onResize = this._onResize.bind(this);
    this._onBoardPointerDown = this._onBoardPointerDown.bind(this);
  }

  connectedCallback() {
    if (!this._built) this._build();
    window.addEventListener('resize', this._onResize);
    this._render();
  }

  disconnectedCallback() {
    window.removeEventListener('resize', this._onResize);
    this._closePopover();
  }

  attributeChangedCallback() {
    if (this._built) this._render();
  }

  // -- properties ------------------------------------------------------------

  get circuit() { return clone(this._circuit); }

  set circuit(value) {
    this._circuit = normalizeCircuit(value);
    this._undo.reset(this._circuit);
    this._selection = [];
    this._afterCircuitChange();
  }

  get state() { return this._state; }

  /// The evolution animation pushes a new state every frame, so this refreshes
  /// the wire readouts and repaints — it never rebuilds the grid.
  set state(value) {
    const bloch = blochVectorList(value);
    this._state = bloch.length ? { bloch } : null;
    if (!this._built) return;
    for (const stale of Array.from(this._gridEl.querySelectorAll('.tf-qc__readout[data-row]'))) {
      stale.replaceWith(this._readoutCell(Number(stale.dataset.row)));
    }
    this._paint();
  }

  get step() { return this._step; }

  set step(value) {
    const number = Number(value);
    this._step = Number.isFinite(number) ? Math.max(0, Math.round(number)) : null;
    this._paint();
  }

  get playhead() { return this._playhead; }

  set playhead(value) {
    const number = Number(value);
    this._playhead = Number.isFinite(number) ? number : null;
    this._paint();
  }

  get selection() { return this._selection.slice(); }

  set selection(value) {
    this._selection = Array.isArray(value) ? value.map(Number).filter(Number.isInteger) : [];
    this._applySelection();
  }

  _applySelection() {
    if (!this._built) return;
    for (const cell of Array.from(this._gridEl.querySelectorAll('.tf-qc__cell'))) {
      const owned = cell.dataset.index !== undefined
        && this._selection.includes(Number(cell.dataset.index));
      cell.classList.toggle('tf-qc__cell--selected', owned);
      if (owned) cell.setAttribute('aria-selected', 'true');
      else cell.removeAttribute('aria-selected');
    }
    this._paint();
  }

  get labels() { return { ...this._labels }; }

  set labels(value) {
    this._labels = { ...DEFAULT_LABELS, ...(value || {}) };
    this._render();
  }

  get readonly() { return this.hasAttribute('readonly'); }

  set readonly(value) {
    if (value) this.setAttribute('readonly', '');
    else this.removeAttribute('readonly');
  }

  get canUndo() { return this._undo.canUndo; }

  get canRedo() { return this._undo.canRedo; }

  undo() {
    const next = this._undo.undo();
    if (!next) return;
    this._circuit = next;
    this._afterCircuitChange();
    this._emitChange();
  }

  redo() {
    const next = this._undo.redo();
    if (!next) return;
    this._circuit = next;
    this._afterCircuitChange();
    this._emitChange();
  }

  /// Deletes the named operations — the Delete key's edit, exposed because a
  /// host with its own gate-properties panel (Q07) must not reach for the
  /// `circuit` setter: that resets the undo history the user just built.
  deleteOps(indices) {
    if (this.readonly) return;
    const ops = this._circuit.ops || [];
    const drop = (Array.isArray(indices) ? indices : [indices])
      .map(Number)
      .filter((index) => Number.isInteger(index) && index >= 0 && index < ops.length);
    if (!drop.length) return;
    this._commit(removeOps(this._circuit, drop), []);
  }

  /// Places a copy of one operation after it and selects the copy. The ASAP
  /// schedule decides where it lands: the first column its qubits are free in.
  duplicateOp(index) {
    if (this.readonly) return;
    const cell = this._grid.cells.find((entry) => entry.index === Number(index));
    if (!cell) return;
    const copy = clone(cell.op);
    const next = insertOp(this._circuit, copy, { column: cell.column + 1, row: cell.minRow });
    this._commit(next, [next.ops.indexOf(copy)]);
  }

  toSvg() {
    return circuitToSvg(this._circuit, { colors: this._svgColors(), labels: this._labels });
  }

  focusCell(row, column) {
    this._caret = { row, column };
    this._applyCaret();
    const cell = this._cellElement(row, column);
    if (cell) cell.focus();
    this._paint();
  }

  /// One cell of the grid is tabbable at a time (a roving tabindex), so Tab
  /// enters the editor where the caret was left rather than at cell one.
  _applyCaret() {
    if (!this._built) return;
    for (const cell of Array.from(this._gridEl.querySelectorAll('.tf-qc__cell'))) {
      cell.tabIndex = Number(cell.dataset.row) === this._caret.row
        && Number(cell.dataset.column) === this._caret.column ? 0 : -1;
    }
  }

  // -- construction ----------------------------------------------------------

  _build() {
    this._built = true;
    this.classList.add('tf-qc');
    this.innerHTML = '';

    this._paletteEl = document.createElement('div');
    this._paletteEl.className = 'tf-qc__palette';
    this.appendChild(this._paletteEl);

    this._board = document.createElement('div');
    this._board.className = 'tf-qc__board';
    // Capture, so a press anywhere on the board dismisses the popover before
    // the cell handlers move the caret out from under it.
    this._board.addEventListener('pointerdown', this._onBoardPointerDown, true);
    this.appendChild(this._board);

    this._canvas = document.createElement('canvas');
    this._canvas.className = 'tf-qc__canvas';
    this._canvas.setAttribute('aria-hidden', 'true');
    this._board.appendChild(this._canvas);

    this._gridEl = document.createElement('div');
    this._gridEl.className = 'tf-qc__grid';
    this._gridEl.setAttribute('role', 'grid');
    this._gridEl.addEventListener('keydown', this._onKeyDown);
    this._board.appendChild(this._gridEl);
  }

  _afterCircuitChange() {
    // A popover holds the operation index it was opened for. Surviving an undo
    // or a fresh `circuit` would let Apply rewrite whatever now sits at that index.
    this._closePopover();
    this._grid = buildGrid(this._circuit);
    this._caret = {
      row: Math.min(this._caret.row, Math.max(0, this._grid.numQubits - 1)),
      column: Math.min(this._caret.column, this._columnCount() - 1),
    };
    this._render();
  }

  _columnCount() {
    const minimum = Number(this.getAttribute('min-columns')) || 8;
    // One idle column past the end is where an appended gate lands.
    return Math.max(this._grid.columns + 1, minimum);
  }

  // -- rendering -------------------------------------------------------------

  _render() {
    if (!this._built) return;
    this._renderPalette();
    this._renderGrid();
    this._paint();
  }

  _renderPalette() {
    const hidden = this.getAttribute('palette') === 'none' || this.readonly;
    this._paletteEl.hidden = hidden;
    if (hidden) {
      this._paletteEl.innerHTML = '';
      return;
    }
    this._paletteEl.innerHTML = '';
    this._paletteEl.setAttribute('aria-label', this._labels.palette);
    this._paletteEl.setAttribute('role', 'toolbar');
    const groups = [
      ['single', this._labels.group_single],
      ['rotation', this._labels.group_rotation],
      ['two', this._labels.group_two],
    ];
    for (const [group, title] of groups) {
      this._paletteEl.appendChild(paletteLabel(title));
      for (const [id, info] of Object.entries(GATE_INFO)) {
        if (info.group !== group) continue;
        this._paletteEl.appendChild(this._paletteButton(id, info.label, info.tone));
      }
    }
    this._paletteEl.appendChild(paletteLabel(this._labels.group_meta));
    this._paletteEl.appendChild(this._paletteButton('Measure', 'M', 'plain'));
    this._paletteEl.appendChild(this._paletteButton('Reset', '|0⟩', 'plain'));
    this._paletteEl.appendChild(this._paletteButton('Barrier', '┆', 'plain'));
  }

  _paletteButton(id, label, tone) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = `tf-qc__pal tf-qc__pal--${tone}`;
    button.dataset.gate = id;
    button.draggable = !this.readonly;
    button.textContent = label;
    button.title = id;
    button.setAttribute('aria-label', id);
    button.addEventListener('dragstart', (event) => {
      // Firefox refuses to start a drag whose dataTransfer carries nothing; the
      // drop itself reads `_drag`, because a payload is not a grid position.
      if (event.dataTransfer) event.dataTransfer.setData('text/plain', `gate:${id}`);
      this._drag = { kind: 'new', gate: id };
    });
    button.addEventListener('dragend', () => { this._drag = null; });
    button.addEventListener('click', () => {
      this._placeFromPalette(id, this._caret.column, this._caret.row);
    });
    return button;
  }

  _renderGrid() {
    const grid = this._grid;
    const columns = this._columnCount();
    const rows = grid.numQubits + (grid.numClbits ? 1 : 0);
    this._gridEl.style.setProperty('--tf-qc-cols', String(columns));
    this._gridEl.style.setProperty('--tf-qc-label', `${LABEL_W}px`);
    this._gridEl.style.setProperty('--tf-qc-col', `${COL_W}px`);
    this._gridEl.style.setProperty('--tf-qc-row', `${ROW_H}px`);
    this._gridEl.style.setProperty('--tf-qc-pad', `${PAD}px`);
    this._gridEl.setAttribute('aria-label', this.getAttribute('aria-label') || this._labels.circuit);
    this._gridEl.setAttribute('aria-rowcount', String(rows));
    this._gridEl.setAttribute('aria-colcount', String(columns + 2));
    this._gridEl.innerHTML = '';

    for (let row = 0; row < grid.numQubits; row += 1) {
      this._gridEl.appendChild(this._renderRow(row, columns));
    }
    if (grid.numClbits) this._gridEl.appendChild(this._classicalRow(columns));
  }

  /// The classical register is a readable wire, not an editable one — nothing
  /// is dropped on it. It still emits a cell per column, because a row that
  /// declares role="row" inside an aria-colcount grid and then carries only its
  /// header reads to assistive tech as a row whose cells are missing.
  _classicalRow(columns) {
    const grid = this._grid;
    const rowEl = document.createElement('div');
    rowEl.className = 'tf-qc__row tf-qc__row--classical';
    rowEl.setAttribute('role', 'row');
    const label = document.createElement('div');
    label.className = 'tf-qc__rowlabel';
    label.setAttribute('role', 'rowheader');
    label.textContent = `c[${grid.numClbits}]`;
    label.title = this._labels.classical;
    rowEl.appendChild(label);

    const written = new Set();
    for (const cell of grid.cells) {
      if (cell.type === 'measure') written.add(cell.column);
    }
    for (let column = 0; column < columns; column += 1) {
      const cell = document.createElement('div');
      cell.className = 'tf-qc__cell tf-qc__cell--classical';
      cell.setAttribute('role', 'gridcell');
      cell.dataset.column = String(column);
      cell.tabIndex = -1;
      cell.setAttribute('aria-label', written.has(column)
        ? `${this._labels.classical}, ${this._labels.measure}, ${this._labels.column} ${column + 1}`
        : `${this._labels.classical}, ${this._labels.column} ${column + 1}`);
      rowEl.appendChild(cell);
    }
    const readout = document.createElement('div');
    readout.className = 'tf-qc__readout';
    readout.setAttribute('role', 'gridcell');
    readout.setAttribute('aria-hidden', 'true');
    readout.tabIndex = -1;
    rowEl.appendChild(readout);
    return rowEl;
  }

  _renderRow(row, columns) {
    const rowEl = document.createElement('div');
    rowEl.className = 'tf-qc__row';
    rowEl.setAttribute('role', 'row');

    const label = document.createElement('div');
    label.className = 'tf-qc__rowlabel';
    label.setAttribute('role', 'rowheader');
    label.textContent = `|q${row}⟩`;
    rowEl.appendChild(label);

    for (let column = 0; column < columns; column += 1) {
      rowEl.appendChild(this._renderCell(row, column));
    }
    rowEl.appendChild(this._readoutCell(row));
    return rowEl;
  }

  _renderCell(row, column) {
    const cell = cellAt(this._grid, row, column);
    const el = document.createElement('div');
    el.className = 'tf-qc__cell';
    el.setAttribute('role', 'gridcell');
    el.dataset.row = String(row);
    el.dataset.column = String(column);
    const focused = this._caret.row === row && this._caret.column === column;
    el.tabIndex = focused ? 0 : -1;
    if (cell) {
      el.dataset.index = String(cell.index);
      el.setAttribute('aria-label', `${describeCell(cell, this._labels)}, ${this._labels.column} ${column + 1}`);
      el.draggable = !this.readonly;
      if (this._selection.includes(cell.index)) {
        el.classList.add('tf-qc__cell--selected');
        el.setAttribute('aria-selected', 'true');
      }
    } else {
      el.setAttribute('aria-label',
        `${this._labels.empty}, ${this._labels.qubit} q${row}, ${this._labels.column} ${column + 1}`);
    }
    el.addEventListener('pointerdown', (event) => this._onCellPointerDown(event, row, column));
    el.addEventListener('focus', () => {
      this._caret = { row, column };
      this._applyCaret();
      this._paint();
    });
    el.addEventListener('dblclick', () => this._openPopover(row, column));
    el.addEventListener('dragstart', (event) => {
      if (!cell || this.readonly) return;
      if (event.dataTransfer) event.dataTransfer.setData('text/plain', `op:${cell.index}`);
      this._drag = { kind: 'move', index: cell.index, anchorRow: row };
    });
    el.addEventListener('dragover', (event) => {
      if (!this._drag || this.readonly) return;
      event.preventDefault();
      el.classList.add('tf-qc__cell--drop');
    });
    el.addEventListener('dragleave', () => el.classList.remove('tf-qc__cell--drop'));
    el.addEventListener('drop', (event) => {
      event.preventDefault();
      el.classList.remove('tf-qc__cell--drop');
      this._onDrop(row, column);
    });
    return el;
  }

  /// Emitted whether or not a state is set, so the readout column exists from
  /// the first render — the animation then swaps these nodes in place instead
  /// of rebuilding the grid sixty times a second.
  _readoutCell(row) {
    const bloch = this._state && this._state.bloch;
    const readout = document.createElement('div');
    readout.className = 'tf-qc__readout';
    readout.setAttribute('role', 'gridcell');
    readout.dataset.row = String(row);
    readout.tabIndex = -1;
    if (!bloch || !bloch[row]) {
      readout.setAttribute('aria-hidden', 'true');
      return readout;
    }
    const [x, y, z] = bloch[row];
    const length = Math.sqrt(x * x + y * y + z * z);
    const one = (1 - z) / 2;
    readout.classList.add('tf-qc__readout--live');
    readout.style.setProperty('--tf-qc-p1', one.toFixed(4));
    readout.style.setProperty('--tf-qc-purity', Math.max(0.25, length).toFixed(3));
    readout.setAttribute('aria-label',
      `${this._labels.readout} q${row}: P(1) = ${(one * 100).toFixed(1)}%`
      + (length < 0.99 ? `, ${this._labels.mixed} |r| = ${length.toFixed(2)}` : ''));
    return readout;
  }

  _cellElement(row, column) {
    return this._gridEl.querySelector(`[data-row="${row}"][data-column="${column}"]`);
  }

  // -- canvas ----------------------------------------------------------------

  _colors() {
    return {
      bg: cssToken('--tf-bg', '#050818'),
      wire: cssToken('--tf-border-hover', '#2f3668'),
      text: cssToken('--tf-text', '#f5f6ff'),
      label: cssToken('--tf-text-2', '#c1c5e0'),
      accent: cssToken('--tf-accent-1', '#6366f1'),
      inkOnDark: '#ffffff',
      inkOnLight: cssToken('--tf-bg', '#050818'),
      glow: cssToken('--tf-accent-glow', 'rgba(99,102,241,0.18)'),
      head: cssToken('--tf-q-playhead', '#f472b6'),
      h: cssToken(TONE_TOKEN.h, '#6366f1'),
      x: cssToken(TONE_TOKEN.x, '#ef4444'),
      z: cssToken(TONE_TOKEN.z, '#0ea5e9'),
      s: cssToken(TONE_TOKEN.s, '#14b8a6'),
      rot: cssToken(TONE_TOKEN.rot, '#f59e0b'),
      u: cssToken(TONE_TOKEN.u, '#a78bfa'),
      plain: cssToken(TONE_TOKEN.plain, '#8b90ab'),
    };
  }

  _svgColors() {
    const live = this._colors();
    return { ...live, bg: cssToken('--tf-bg-card', '#141836') };
  }

  _paint() {
    const canvas = this._canvas;
    if (!canvas) return;
    const grid = this._grid;
    const columns = this._columnCount();
    const rows = grid.numQubits + (grid.numClbits ? 1 : 0);
    const width = PAD * 2 + LABEL_W + columns * COL_W;
    const height = PAD * 2 + rows * ROW_H;
    const ratio = (typeof window !== 'undefined' && window.devicePixelRatio) || 1;
    canvas.style.width = `${width}px`;
    canvas.style.height = `${height}px`;
    canvas.width = Math.round(width * ratio);
    canvas.height = Math.round(height * ratio);
    const ctx = typeof canvas.getContext === 'function' ? canvas.getContext('2d') : null;
    if (!ctx) return;
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, width, height);
    const colors = this._colors();
    const rowY = (row) => PAD + row * ROW_H + ROW_H / 2;
    const colX = (column) => PAD + LABEL_W + column * COL_W + COL_W / 2;

    if (this._step != null) {
      for (let column = 0; column < Math.min(this._step, columns); column += 1) {
        ctx.fillStyle = colors.glow;
        ctx.globalAlpha = 0.35;
        ctx.fillRect(PAD + LABEL_W + column * COL_W, PAD, COL_W, rows * ROW_H);
        ctx.globalAlpha = 1;
      }
      if (this._step < columns) {
        ctx.fillStyle = colors.glow;
        ctx.fillRect(PAD + LABEL_W + this._step * COL_W, PAD, COL_W, rows * ROW_H);
        ctx.strokeStyle = colors.accent;
        ctx.lineWidth = 1;
        ctx.strokeRect(PAD + LABEL_W + this._step * COL_W + 0.5, PAD + 0.5, COL_W - 1, rows * ROW_H - 1);
      }
    }

    ctx.strokeStyle = colors.wire;
    ctx.lineWidth = 1.5;
    for (let row = 0; row < grid.numQubits; row += 1) {
      ctx.beginPath();
      ctx.moveTo(PAD + LABEL_W, rowY(row));
      ctx.lineTo(width - PAD, rowY(row));
      ctx.stroke();
    }
    if (grid.numClbits) {
      const y = rowY(grid.numQubits);
      ctx.lineWidth = 1;
      for (const offset of [-2, 2]) {
        ctx.beginPath();
        ctx.moveTo(PAD + LABEL_W, y + offset);
        ctx.lineTo(width - PAD, y + offset);
        ctx.stroke();
      }
    }

    for (const cell of grid.cells) this._paintCell(ctx, cell, colors, rowY, colX, grid);

    if (this._playhead != null) {
      const x = PAD + LABEL_W + this._playhead * COL_W;
      ctx.strokeStyle = colors.head;
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(x, PAD);
      ctx.lineTo(x, PAD + rows * ROW_H);
      ctx.stroke();
      ctx.fillStyle = colors.head;
      ctx.beginPath();
      ctx.arc(x, PAD, 5, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  _paintCell(ctx, cell, colors, rowY, colX, grid) {
    const x = colX(cell.column);
    const dim = this._step != null && cell.column < this._step;
    ctx.globalAlpha = dim ? 0.55 : 1;
    const selected = this._selection.includes(cell.index);
    if (cell.type === 'barrier') {
      ctx.strokeStyle = colors.plain;
      ctx.setLineDash([4, 4]);
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(x, rowY(cell.minRow) - ROW_H / 2 + 4);
      ctx.lineTo(x, rowY(cell.maxRow) + ROW_H / 2 - 4);
      ctx.stroke();
      ctx.setLineDash([]);
      ctx.globalAlpha = 1;
      return;
    }
    if (cell.type === 'gphase') {
      drawBox(ctx, x, rowY(0), `gφ(${formatAngle(cell.params[0])})`, colors.plain, gateTextColor('plain', colors), colors.accent, selected);
      ctx.globalAlpha = 1;
      return;
    }
    if (cell.type === 'measure') {
      if (grid.numClbits) {
        ctx.strokeStyle = colors.wire;
        ctx.lineWidth = 1.5;
        ctx.beginPath();
        ctx.moveTo(x, rowY(cell.qubits[0]));
        ctx.lineTo(x, rowY(grid.numQubits));
        ctx.stroke();
      }
      drawBox(ctx, x, rowY(cell.qubits[0]), 'M', colors.plain, gateTextColor('plain', colors), colors.accent, selected);
      ctx.globalAlpha = 1;
      return;
    }
    if (cell.type === 'reset') {
      drawBox(ctx, x, rowY(cell.qubits[0]), '|0⟩', colors.plain, gateTextColor('plain', colors), colors.accent, selected);
      ctx.globalAlpha = 1;
      return;
    }
    const info = GATE_INFO[cell.id] || { label: cell.id, tone: 'plain', arity: 1 };
    const tint = colors[info.tone] || colors.plain;
    if (info.arity === 2) {
      ctx.strokeStyle = colors.text;
      ctx.lineWidth = 2;
      ctx.beginPath();
      ctx.moveTo(x, rowY(cell.qubits[0]));
      ctx.lineTo(x, rowY(cell.qubits[1]));
      ctx.stroke();
      if (info.control) {
        drawDot(ctx, x, rowY(cell.qubits[0]), colors.text);
        if (info.target === 'plus') drawTargetCross(ctx, x, rowY(cell.qubits[1]), colors);
        else if (info.target === 'dot') drawDot(ctx, x, rowY(cell.qubits[1]), colors.text);
        else drawBox(ctx, x, rowY(cell.qubits[1]), gateBoxLabel(info, cell), tint, gateTextColor(info.tone, colors), colors.accent, selected);
      } else {
        for (const qubit of cell.qubits) drawCross(ctx, x, rowY(qubit), colors.text);
      }
      ctx.globalAlpha = 1;
      return;
    }
    drawBox(ctx, x, rowY(cell.qubits[0]), gateBoxLabel(info, cell), tint, gateTextColor(info.tone, colors), colors.accent, selected);
    ctx.globalAlpha = 1;
  }

  // -- interaction -----------------------------------------------------------

  _onResize() { this._paint(); }

  _onBoardPointerDown(event) {
    if (this._popover && !this._popover.contains(event.target)) this._closePopover();
  }

  _onCellPointerDown(event, row, column) {
    const cell = cellAt(this._grid, row, column);
    this._caret = { row, column };
    this._applyCaret();
    if (event.shiftKey && this._anchor) {
      this._selectRange(this._anchor, { row, column });
    } else {
      this._anchor = { row, column };
      this._selection = cell ? [cell.index] : [];
      this._emitSelect();
    }
    this.dispatchEvent(new CustomEvent('column-click', {
      bubbles: true, composed: true, detail: { column },
    }));
    this._applySelection();
  }

  _selectRange(from, to) {
    const rows = [Math.min(from.row, to.row), Math.max(from.row, to.row)];
    const columns = [Math.min(from.column, to.column), Math.max(from.column, to.column)];
    this._selection = this._grid.cells
      .filter((cell) => cell.column >= columns[0] && cell.column <= columns[1]
        && cell.maxRow >= rows[0] && cell.minRow <= rows[1])
      .map((cell) => cell.index);
    this._emitSelect();
  }

  _onKeyDown(event) {
    const { key } = event;
    const columns = this._columnCount();
    const maxRow = Math.max(0, this._grid.numQubits - 1);
    const move = { ArrowLeft: [0, -1], ArrowRight: [0, 1], ArrowUp: [-1, 0], ArrowDown: [1, 0] }[key];
    if (move) {
      event.preventDefault();
      const row = Math.min(maxRow, Math.max(0, this._caret.row + move[0]));
      const column = Math.min(columns - 1, Math.max(0, this._caret.column + move[1]));
      if (event.shiftKey) {
        if (!this._anchor) this._anchor = { ...this._caret };
        this._caret = { row, column };
        this._selectRange(this._anchor, this._caret);
        this._applySelection();
      } else {
        this._anchor = { row, column };
        this.focusCell(row, column);
      }
      return;
    }
    if (key === 'Home' || key === 'End') {
      event.preventDefault();
      this.focusCell(this._caret.row, key === 'Home' ? 0 : columns - 1);
      return;
    }
    if (key === 'Enter' || key === ' ' || key === 'Spacebar') {
      event.preventDefault();
      this._openPopover(this._caret.row, this._caret.column);
      return;
    }
    if ((key === 'Delete' || key === 'Backspace') && !this.readonly) {
      event.preventDefault();
      const indices = this._selection.length
        ? this._selection
        : [cellAt(this._grid, this._caret.row, this._caret.column)].filter(Boolean).map((c) => c.index);
      if (indices.length) this._commit(removeOps(this._circuit, indices), []);
      return;
    }
    if (key === 'Escape') {
      this._closePopover();
      this._selection = [];
      this._emitSelect();
      this._applySelection();
      return;
    }
    const lower = key.toLowerCase();
    if ((event.ctrlKey || event.metaKey) && lower === 'z') {
      event.preventDefault();
      if (event.shiftKey) this.redo(); else this.undo();
      return;
    }
    if ((event.ctrlKey || event.metaKey) && lower === 'y') {
      event.preventDefault();
      this.redo();
    }
  }

  _onDrop(row, column) {
    const drag = this._drag;
    this._drag = null;
    if (!drag || this.readonly) return;
    if (drag.kind === 'new') this._placeFromPalette(drag.gate, column, row);
    else {
      const moved = moveOp(this._circuit, drag.index, { row, column, anchorRow: drag.anchorRow });
      this._commit(moved, [drag.index]);
    }
  }

  _placeFromPalette(id, column, row) {
    if (this.readonly) return;
    const op = this._buildOp(id, row);
    if (!op) return;
    this._commit(insertOp(this._circuit, op, { row, column }), []);
  }

  /// Builds the operation a palette entry stands for. A 2-qubit gate takes the
  /// next wire as its second operand — dropping it on the last wire wraps to
  /// the first — and the operand pickers in the cell popover (Enter or
  /// double-click) then move either end onto any other wire.
  _buildOp(id, row) {
    const numQubits = this._grid.numQubits;
    if (!numQubits) return null;
    if (id === 'Measure') {
      if (!this._grid.numClbits) return null;
      const clbit = Math.min(row, this._grid.numClbits - 1);
      return { kind: { Measure: { qubit: row, clbit } }, conditions: [] };
    }
    if (id === 'Reset') return { kind: { Reset: { qubit: row } }, conditions: [] };
    if (id === 'Barrier') {
      return { kind: { Barrier: { qubits: rangeOf(0, numQubits - 1) } }, conditions: [] };
    }
    const info = GATE_INFO[id];
    if (!info) return null;
    const qubits = info.arity === 1 ? [row] : [row, (row + 1) % numQubits];
    if (info.arity === 2 && qubits[0] === qubits[1]) return null;
    return { kind: { Gate: { gate: makeGate(id, info.params.map(() => 0)), qubits } }, conditions: [] };
  }

  _commit(circuit, selection) {
    this._circuit = normalizeCircuit(circuit);
    this._undo.push(this._circuit);
    this._selection = selection || [];
    this._afterCircuitChange();
    this._emitChange();
  }

  _emitChange() {
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true, composed: true, detail: { circuit: this.circuit },
    }));
  }

  _emitSelect() {
    this.dispatchEvent(new CustomEvent('select', {
      bubbles: true, composed: true, detail: { indices: this.selection },
    }));
  }

  // -- popover ---------------------------------------------------------------

  _closePopover() {
    if (this._popover && this._popover.parentNode) this._popover.remove();
    this._popover = null;
  }

  /// Enter on a gate edits it; Enter on an idle cell offers the palette. Both
  /// live in one popover so keyboard and pointer share the editing path.
  _openPopover(row, column) {
    this._closePopover();
    if (this.readonly) return;
    const cell = cellAt(this._grid, row, column);
    const popover = document.createElement('div');
    popover.className = 'tf-qc__popover';
    popover.style.left = `${PAD + LABEL_W + column * COL_W}px`;
    popover.style.top = `${PAD + (row + 1) * ROW_H}px`;
    popover.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        event.stopPropagation();
        this._closePopover();
        this.focusCell(row, column);
      }
    });
    if (cell) this._fillOpPopover(popover, cell, row, column);
    else this._fillPalettePopover(popover, row, column);
    this._board.appendChild(popover);
    this._popover = popover;
    const first = popover.querySelector('tf-input, tf-select, button');
    if (first && typeof first.focus === 'function') first.focus();
  }

  /// Plan §13.4 sketches keyboard insertion as a "command palette". This uses
  /// an anchored popover instead of <tf-command-palette>, which is the app's
  /// singleton Cmd+K overlay: a global overlay loses the cell the caret is on,
  /// and gate choice is a 26-entry grid of glyphs, not a text search. §19.2
  /// leaves the presentation open, so the keyboard path (Enter) and the pointer
  /// path (double-click) share this one popover.
  _fillPalettePopover(popover, row, column) {
    const title = document.createElement('div');
    title.className = 'tf-qc__popover-title';
    title.textContent = this._labels.add_gate;
    popover.appendChild(title);
    const list = document.createElement('div');
    list.className = 'tf-qc__popover-grid';
    for (const [id, info] of Object.entries(GATE_INFO)) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = `tf-qc__pal tf-qc__pal--${info.tone}`;
      button.textContent = info.label;
      button.setAttribute('aria-label', id);
      button.addEventListener('click', () => {
        this._closePopover();
        this._placeFromPalette(id, column, row);
        this.focusCell(row, column);
      });
      list.appendChild(button);
    }
    for (const id of META_OPS) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'tf-qc__pal tf-qc__pal--plain';
      button.textContent = id === 'Measure' ? 'M' : id === 'Reset' ? '|0⟩' : '┆';
      button.setAttribute('aria-label', id);
      button.addEventListener('click', () => {
        this._closePopover();
        this._placeFromPalette(id, column, row);
        this.focusCell(row, column);
      });
      list.appendChild(button);
    }
    popover.appendChild(list);
  }

  /// One editor for every operation: the angles of a parametric gate and the
  /// wires it sits on are the same commit, so a control moved to q2 and its
  /// angle retyped cost one undo step rather than two.
  _fillOpPopover(popover, cell, row, column) {
    const info = GATE_INFO[cell.id];
    const title = document.createElement('div');
    title.className = 'tf-qc__popover-title';
    title.textContent = describeCell(cell, this._labels);
    popover.appendChild(title);

    const inputs = (info ? info.params : []).map((name, i) => {
      const input = document.createElement('tf-input');
      input.setAttribute('label', name);
      input.setAttribute('value', formatAngle(cell.params[i]));
      popover.appendChild(input);
      return input;
    });
    const wires = this._wireSelects(popover, cell, info);

    if (!inputs.length && !wires) {
      popover.appendChild(this._removeButton(cell, row, column));
      return;
    }
    const actions = document.createElement('div');
    actions.className = 'tf-qc__popover-actions';
    const apply = document.createElement('button');
    apply.type = 'button';
    apply.className = 'tf-btn tf-btn-sm tf-btn-primary';
    apply.textContent = this._labels.apply;
    apply.addEventListener('click', () => {
      const values = [];
      let bad = false;
      inputs.forEach((input) => {
        const parsed = parseAngle(input.value);
        if (parsed == null) {
          input.setAttribute('error', this._labels.bad_angle);
          bad = true;
        } else {
          input.removeAttribute('error');
          values.push(parsed);
        }
      });
      if (bad) return;
      let next = this._circuit;
      if (values.length) next = setOpParams(next, cell.index, values);
      if (wires) next = setOpWires(next, cell.index, wires.read());
      this._closePopover();
      if (next !== this._circuit) this._commit(next, [cell.index]);
      this.focusCell(row, column);
    });
    actions.appendChild(apply);
    actions.appendChild(this._removeButton(cell, row, column));
    popover.appendChild(actions);
  }

  /// Wire pickers, but only where dragging cannot say the same thing: a drag
  /// translates every operand by one delta, so it can never re-pair a control
  /// with a distant target, and it cannot point a measurement at another
  /// classical bit. A single-qubit gate is left to the drag.
  _wireSelects(popover, cell, info) {
    const grid = this._grid;
    if (cell.type === 'gate' && cell.qubits.length > 1) {
      const options = rangeOf(0, grid.numQubits - 1)
        .map((q) => ({ value: String(q), label: `q${q}` }));
      const names = info && info.control
        ? [this._labels.control, this._labels.target]
        : cell.qubits.map((_, i) => `${this._labels.operand} ${i + 1}`);
      const selects = cell.qubits.map((qubit, i) => {
        const select = document.createElement('tf-select');
        select.setAttribute('label', names[i] || `${this._labels.operand} ${i + 1}`);
        popover.appendChild(select);
        select.setOptions(options, String(qubit));
        return select;
      });
      // Picking a wire another operand already holds swaps the two rather than
      // producing an operand list the IR would reject. `held` tracks what each
      // picker had BEFORE this change, so a second edit swaps the current pair
      // and not the pair the popover opened with.
      const held = cell.qubits.map(String);
      selects.forEach((select, i) => {
        select.addEventListener('change', () => {
          const chosen = select.value;
          const clash = selects.findIndex((other, j) => j !== i && other.value === chosen);
          if (clash >= 0) {
            selects[clash].value = held[i];
            held[clash] = held[i];
          }
          held[i] = chosen;
        });
      });
      return { read: () => ({ qubits: selects.map((select) => Number(select.value)) }) };
    }

    if (cell.type === 'measure' && grid.numClbits > 1) {
      const select = document.createElement('tf-select');
      select.setAttribute('label', this._labels.clbit);
      popover.appendChild(select);
      select.setOptions(
        rangeOf(0, grid.numClbits - 1).map((c) => ({ value: String(c), label: `c${c}` })),
        String(cell.op.kind.Measure.clbit)
      );
      return { read: () => ({ clbit: Number(select.value) }) };
    }
    return null;
  }

  _removeButton(cell, row, column) {
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.className = 'tf-btn tf-btn-sm tf-btn-danger';
    remove.textContent = this._labels.remove;
    remove.addEventListener('click', () => {
      this._closePopover();
      this._commit(removeOps(this._circuit, [cell.index]), []);
      this.focusCell(row, column);
    });
    return remove;
  }
}

function normalizeCircuit(value) {
  const source = value && typeof value === 'object' ? value : {};
  return {
    qubitRegisters: Array.isArray(source.qubitRegisters) ? source.qubitRegisters : [],
    clbitRegisters: Array.isArray(source.clbitRegisters) ? source.clbitRegisters : [],
    numQubits: Number(source.numQubits) || 0,
    numClbits: Number(source.numClbits) || 0,
    ops: Array.isArray(source.ops) ? source.ops : [],
  };
}

function paletteLabel(text) {
  const label = document.createElement('span');
  label.className = 'tf-qc__pal-label';
  label.textContent = text;
  return label;
}

function gateBoxLabel(info, cell) {
  if (!cell.params || !cell.params.length) return info.label;
  return `${info.label} ${formatAngle(cell.params[0])}`;
}

function drawBox(ctx, cx, cy, text, fill, ink, accent, selected) {
  const wide = text.length > 3;
  const w = wide ? Math.min(COL_W - 6, 12 + text.length * 6.5) : GATE_SIZE;
  ctx.fillStyle = fill;
  ctx.beginPath();
  if (typeof ctx.roundRect === 'function') {
    ctx.roundRect(cx - w / 2, cy - GATE_SIZE / 2, w, GATE_SIZE, 7);
  } else {
    ctx.rect(cx - w / 2, cy - GATE_SIZE / 2, w, GATE_SIZE);
  }
  ctx.fill();
  if (selected) {
    ctx.strokeStyle = accent;
    ctx.lineWidth = 2;
    ctx.stroke();
  }
  ctx.fillStyle = ink;
  ctx.font = `700 ${wide ? 9 : 13}px ${cssToken('--tf-mono', 'ui-monospace, monospace')}`;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillText(text, cx, cy + 1);
}

function drawDot(ctx, cx, cy, color) {
  ctx.fillStyle = color;
  ctx.beginPath();
  ctx.arc(cx, cy, CTRL_R, 0, Math.PI * 2);
  ctx.fill();
}

function drawTargetCross(ctx, cx, cy, colors) {
  ctx.strokeStyle = colors.text;
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.arc(cx, cy, TARGET_R, 0, Math.PI * 2);
  ctx.stroke();
  ctx.beginPath();
  ctx.moveTo(cx - TARGET_R, cy);
  ctx.lineTo(cx + TARGET_R, cy);
  ctx.moveTo(cx, cy - TARGET_R);
  ctx.lineTo(cx, cy + TARGET_R);
  ctx.stroke();
}

function drawCross(ctx, cx, cy, color) {
  const r = 8;
  ctx.strokeStyle = color;
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.moveTo(cx - r, cy - r);
  ctx.lineTo(cx + r, cy + r);
  ctx.moveTo(cx - r, cy + r);
  ctx.lineTo(cx + r, cy - r);
  ctx.stroke();
}

if (!customElements.get('tf-quantum-circuit')) {
  customElements.define('tf-quantum-circuit', TfQuantumCircuit);
}

export { TfQuantumCircuit, DEFAULT_LABELS as CIRCUIT_LABELS };
