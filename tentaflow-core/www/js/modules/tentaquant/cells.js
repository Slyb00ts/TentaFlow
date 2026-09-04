// ===== File: modules/tentaquant/cells.js — the notebook cell model (Q06) =====
//
// A notebook is an ordered list of cells and nothing else: the wire carries it
// as `cells_json`, a free JSON array the server only checks is an array, so the
// schema lives HERE and every rule about it is a pure function that can be
// tested without a DOM.
//
// A cell is `{id, kind, source}`. `source` is the canonical artefact of its
// kind — markdown text, or OpenQASM 3 for a circuit (plan §0: OQ3 is the
// canonical circuit artefact, so a circuit cell stores the program, never the
// IR, which is derived by the parser on every load).
//
// Outputs are deliberately NOT persisted: a T0 result is recomputed in
// milliseconds in the browser, and a stored run belongs to the run store, which
// arrives with its own backend.
//
// A cell whose `kind` this build cannot render is still carried through a save
// untouched. Notebooks are versioned with optimistic locking, so dropping a
// cell an older screen does not understand would silently delete somebody's
// work on the next save.

export const CELL_KINDS = ['markdown', 'circuit'];

// What a new circuit cell starts from: the smallest program that shows the
// point of the state panel — one superposition, one entanglement, one readout.
export const DEFAULT_CIRCUIT_SOURCE = `OPENQASM 3.0;
include "stdgates.inc";

qubit[2] q;
bit[2] c;

h q[0];
cx q[0], q[1];
c = measure q;
`;

let sequence = 0;

// Cell ids only have to be unique inside one notebook and stable across a save,
// so they are minted locally; `crypto.randomUUID` is not available in every
// context the dashboard runs in (an insecure origin during development).
export function cellId() {
  sequence += 1;
  return `c${Date.now().toString(36)}${sequence.toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

export function createCell(kind, patch = {}) {
  const source = patch.source !== undefined
    ? String(patch.source)
    : (kind === 'circuit' ? DEFAULT_CIRCUIT_SOURCE : '');
  return { id: patch.id || cellId(), kind, source };
}

/// The stored array as cells. Anything that is not an object is dropped (it
/// could not be edited or saved back), a missing or duplicated id is minted so
/// the list can be keyed, and an unknown `kind` is kept as it stands.
export function parseCells(cellsJson) {
  let raw;
  try {
    raw = JSON.parse(cellsJson || '[]');
  } catch {
    return [];
  }
  if (!Array.isArray(raw)) return [];
  const seen = new Set();
  const cells = [];
  for (const entry of raw) {
    if (!entry || typeof entry !== 'object' || Array.isArray(entry)) continue;
    const kind = typeof entry.kind === 'string' && entry.kind ? entry.kind : 'markdown';
    const wanted = typeof entry.id === 'string' ? entry.id : '';
    const id = wanted && !seen.has(wanted) ? wanted : cellId();
    seen.add(id);
    cells.push({ id, kind, source: typeof entry.source === 'string' ? entry.source : '' });
  }
  return cells;
}

export function serializeCells(cells) {
  return JSON.stringify((cells || []).map((c) => ({
    id: c.id,
    kind: c.kind,
    source: String(c.source ?? ''),
  })));
}

export function indexOfCell(cells, id) {
  return (cells || []).findIndex((c) => c.id === id);
}

/// Inserts a new cell and answers the list AND the new id, because the caller
/// has to focus what it just created.
export function addCell(cells, kind, index) {
  const list = (cells || []).slice();
  const cell = createCell(kind);
  const at = Number.isInteger(index) ? Math.max(0, Math.min(index, list.length)) : list.length;
  list.splice(at, 0, cell);
  return { cells: list, id: cell.id };
}

/// Moves a cell by `delta` positions. Moving the first cell up (or the last one
/// down) is a no-op that returns the SAME array, which is what lets a caller
/// skip a redraw.
export function moveCell(cells, id, delta) {
  const list = (cells || []).slice();
  const from = indexOfCell(list, id);
  if (from < 0) return cells || [];
  const to = from + Number(delta || 0);
  if (to < 0 || to >= list.length || to === from) return cells || [];
  const [cell] = list.splice(from, 1);
  list.splice(to, 0, cell);
  return list;
}

export function removeCell(cells, id) {
  const list = (cells || []).filter((c) => c.id !== id);
  return list.length === (cells || []).length ? (cells || []) : list;
}

export function updateCell(cells, id, patch) {
  let changed = false;
  const list = (cells || []).map((c) => {
    if (c.id !== id) return c;
    changed = true;
    return { ...c, ...patch };
  });
  return changed ? list : (cells || []);
}

/// The cell the right-hand state panel follows (Q06): the last circuit in the
/// notebook, or null while there is none.
export function lastCircuitCell(cells) {
  for (let i = (cells || []).length - 1; i >= 0; i -= 1) {
    if (cells[i].kind === 'circuit') return cells[i];
  }
  return null;
}

export function isRenderableKind(kind) {
  return CELL_KINDS.includes(kind);
}

export function isDirty(cells, savedJson) {
  return serializeCells(cells) !== String(savedJson ?? '');
}

/// Editor state from a `NotebookGetResponse` / `NotebookResponse`. `savedJson`
/// is the NORMALISED serialisation of what was loaded, so the dirty check
/// compares like with like instead of against whatever whitespace the server
/// happened to store.
export function notebookState(response) {
  const cells = parseCells(response && response.cellsJson);
  const notebook = (response && response.notebook) || {};
  const version = Number(
    response && response.version !== undefined ? response.version : notebook.currentVersion,
  ) || 0;
  return { cells, version, savedJson: serializeCells(cells) };
}

/// A save that lost the optimistic lock. The handler answers `Conflict` and
/// nothing else does, so this is the one place the code compares that string.
export function isVersionConflict(error) {
  return Boolean(error) && error.code === 'Conflict';
}
