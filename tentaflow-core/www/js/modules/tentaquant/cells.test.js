// =============================================================================
// File: modules/tentaquant/cells.test.js
// Description: The notebook cell model (Q06) — building, moving and deleting
// cells, the round trip through `cells_json` the wire carries, the dirty check
// the save button reads and what a lost optimistic lock looks like. All pure,
// so none of it needs a DOM.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  CELL_KINDS, DEFAULT_CIRCUIT_SOURCE, addCell, createCell, indexOfCell, isDirty,
  isRenderableKind, isVersionConflict, lastCircuitCell, moveCell, notebookState,
  parseCells, removeCell, serializeCells, updateCell,
} = await import('./cells.js');

const notebook = () => [
  createCell('markdown', { id: 'a', source: '# Grover' }),
  createCell('circuit', { id: 'b' }),
  createCell('markdown', { id: 'c', source: 'wynik' }),
];

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

test('a new circuit cell starts from a runnable OpenQASM 3 program', () => {
  const cell = createCell('circuit');
  assert.equal(cell.kind, 'circuit');
  assert.equal(cell.source, DEFAULT_CIRCUIT_SOURCE);
  assert.match(cell.source, /^OPENQASM 3\.0;/);
  assert.ok(cell.id);
});

test('a new markdown cell is empty and every kind the screen renders is known', () => {
  assert.equal(createCell('markdown').source, '');
  assert.deepEqual(CELL_KINDS, ['markdown', 'circuit']);
  assert.equal(isRenderableKind('markdown'), true);
  assert.equal(isRenderableKind('code'), false);
});

test('two cells minted in a row never share an id', () => {
  const ids = new Set(Array.from({ length: 50 }, () => createCell('markdown').id));
  assert.equal(ids.size, 50);
});

// ---------------------------------------------------------------------------
// Add / move / delete
// ---------------------------------------------------------------------------

test('a cell is added at the position asked for and the new id comes back', () => {
  const { cells, id } = addCell(notebook(), 'circuit', 1);
  assert.equal(cells.length, 4);
  assert.equal(cells[1].id, id);
  assert.equal(cells[1].kind, 'circuit');
  assert.deepEqual(cells.map((c) => c.id).filter((x) => x !== id), ['a', 'b', 'c']);
});

test('an out-of-range index appends instead of losing the cell', () => {
  assert.equal(addCell(notebook(), 'markdown', 99).cells.length, 4);
  assert.equal(addCell(notebook(), 'markdown', 99).cells[3].kind, 'markdown');
  assert.equal(addCell(notebook(), 'markdown', -5).cells[0].kind, 'markdown');
  assert.equal(addCell(notebook(), 'markdown').cells.length, 4);
});

test('moving a cell reorders it and a move past either end changes nothing', () => {
  assert.deepEqual(moveCell(notebook(), 'a', 1).map((c) => c.id), ['b', 'a', 'c']);
  assert.deepEqual(moveCell(notebook(), 'c', -1).map((c) => c.id), ['a', 'c', 'b']);
  const cells = notebook();
  assert.equal(moveCell(cells, 'a', -1), cells, 'the first cell cannot move up');
  assert.equal(moveCell(cells, 'c', 1), cells, 'the last cell cannot move down');
  assert.equal(moveCell(cells, 'missing', 1), cells);
});

test('deleting keeps the other cells and an unknown id is a no-op', () => {
  assert.deepEqual(removeCell(notebook(), 'b').map((c) => c.id), ['a', 'c']);
  const cells = notebook();
  assert.equal(removeCell(cells, 'zz'), cells);
});

test('updating a cell replaces only the fields handed in', () => {
  const cells = updateCell(notebook(), 'a', { source: '# nowy' });
  assert.equal(cells[0].source, '# nowy');
  assert.equal(cells[0].kind, 'markdown');
  assert.equal(cells[1].source, DEFAULT_CIRCUIT_SOURCE);
  assert.equal(indexOfCell(cells, 'c'), 2);
  assert.equal(indexOfCell(cells, 'nope'), -1);
});

test('the state panel follows the LAST circuit cell of the notebook', () => {
  const cells = notebook().concat(createCell('circuit', { id: 'd', source: 'h q[0];' }));
  assert.equal(lastCircuitCell(cells).id, 'd');
  assert.equal(lastCircuitCell([createCell('markdown')]), null);
  assert.equal(lastCircuitCell([]), null);
});

// ---------------------------------------------------------------------------
// cells_json
// ---------------------------------------------------------------------------

test('cells survive a round trip through cells_json unchanged', () => {
  const cells = notebook();
  const json = serializeCells(cells);
  assert.deepEqual(parseCells(json), cells);
  // Only the three persisted fields travel: an output is recomputed, never stored.
  assert.deepEqual(Object.keys(JSON.parse(json)[0]).sort(), ['id', 'kind', 'source']);
});

test('runtime fields on a cell are dropped by the serialisation', () => {
  const cells = [{ ...createCell('circuit', { id: 'b' }), outputs: [{ counts: {} }], running: true }];
  assert.deepEqual(JSON.parse(serializeCells(cells)), [{ id: 'b', kind: 'circuit', source: DEFAULT_CIRCUIT_SOURCE }]);
});

test('a broken or non-array cells_json reads as an empty notebook', () => {
  assert.deepEqual(parseCells('not json'), []);
  assert.deepEqual(parseCells('{"kind":"markdown"}'), []);
  assert.deepEqual(parseCells(''), []);
  assert.deepEqual(parseCells(undefined), []);
  assert.deepEqual(parseCells('[1, "x", null]'), []);
});

test('a cell whose kind this build cannot draw is still carried through a save', () => {
  const stored = '[{"id":"k","kind":"code","source":"print(1)"}]';
  const cells = parseCells(stored);
  assert.equal(cells[0].kind, 'code');
  assert.equal(serializeCells(cells), stored);
});

test('a missing or duplicated id is minted so the list stays keyable', () => {
  const cells = parseCells('[{"kind":"markdown"},{"id":"x","kind":"markdown"},{"id":"x","kind":"markdown"}]');
  const ids = cells.map((c) => c.id);
  assert.equal(new Set(ids).size, 3);
  assert.equal(ids[1], 'x');
});

// ---------------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------------

test('the editor state comes from the wire normalised, so nothing is dirty on load', () => {
  const state = notebookState({
    cellsJson: '[{"id":"a","kind":"markdown","source":"# Grover"}]',
    version: 7,
    notebook: { currentVersion: 7 },
  });
  assert.equal(state.version, 7);
  assert.equal(state.cells.length, 1);
  assert.equal(isDirty(state.cells, state.savedJson), false);
});

test('an old version is read from `version`, not from the notebook head', () => {
  const state = notebookState({ cellsJson: '[]', version: 3, notebook: { currentVersion: 9 } });
  assert.equal(state.version, 3);
});

test('every edit makes the notebook dirty', () => {
  const state = notebookState({ cellsJson: serializeCells(notebook()), version: 1 });
  assert.equal(isDirty(updateCell(state.cells, 'a', { source: 'x' }), state.savedJson), true);
  assert.equal(isDirty(removeCell(state.cells, 'a'), state.savedJson), true);
  assert.equal(isDirty(moveCell(state.cells, 'a', 1), state.savedJson), true);
  assert.equal(isDirty(addCell(state.cells, 'markdown').cells, state.savedJson), true);
});

test('a save that lost the optimistic lock is recognised by its protocol code', () => {
  const conflict = new Error('the notebook changed since it was loaded');
  conflict.code = 'Conflict';
  assert.equal(isVersionConflict(conflict), true);
  const other = new Error('offline');
  other.code = 'Internal';
  assert.equal(isVersionConflict(other), false);
  assert.equal(isVersionConflict(null), false);
});
