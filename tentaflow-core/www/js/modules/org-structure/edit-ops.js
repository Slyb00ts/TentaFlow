// =============================================================================
// File: modules/org-structure/edit-ops.js
// Description: The operations of the edit mode's draft, and the algebra of the
//   draft as a list. An operation is what `orgBatchRequest` takes:
//   `{ kind, tempId?, ...camelCaseFields }` — `kind` is the write's request name
//   without "Request" (`positionMove`, `assign`, ...), and a creating operation
//   carries a `tempId` ("tmp:...") that later operations use wherever they need
//   the id of what it makes.
//
//   The builders turn what the administrator did (a card dropped, a field
//   changed) into operations against the structure as the preview shows it. The
//   draft stays short and readable because operations are FOLDED where they
//   describe the same thing: a second edit of one field set merges into the
//   first, a move replaces the previous move, an edit of something the draft
//   itself creates goes into its creating operation, and ending such a thing
//   removes it (with everything that depended on it). Pure: no DOM, no transport.
// =============================================================================

import { subjectKey } from '/js/modules/org-structure/model.js';

export const TEMP_PREFIX = 'tmp:';

export const isTempId = (id) => typeof id === 'string' && id.startsWith(TEMP_PREFIX);

const positionOf = (view, id) => (view.positions ?? []).find((p) => p.position_id === id);
const unitOf = (view, id) => (view.units ?? []).find((u) => u.unit_id === id);

// ---- builders ------------------------------------------------------------------

export const movePositionOp = (positionId, newParentPositionId, from) => ({ kind: 'positionMove', positionId, newParentPositionId, from });

export const moveUnitOp = (unitId, newParentUnitId, from) => ({ kind: 'unitMove', unitId, newParentUnitId, from });

export const setHeadOp = (unitId, headPositionId, from) => ({ kind: 'headSet', unitId, headPositionId, from });

export const setDeputiesOp = (unitId, positionIds, from) => ({ kind: 'deputyHeadsSet', unitId, positionIds, from });

// A patch names the fields to change; `null` for a clearable field clears it, by the name the wire knows it by.
const CLEAR_NAMES = { code: 'code', roleId: 'role_id', typeId: 'type_id', color: 'color' };
const POSITION_FIELDS = { name: 'name', code: 'code', roleId: 'role_id', isStaff: 'is_staff' };
const UNIT_FIELDS = { name: 'name', code: 'code', typeId: 'type_id', color: 'color' };

function fieldOf(entity, wire, key) {
  return key === 'isStaff' ? Boolean(entity[wire]) : entity[wire] ?? null;
}

function patchOp(kind, idField, id, entity, patch, fields, clearable, from) {
  const changed = Object.entries(patch).filter(([key, value]) => fieldOf(entity, fields[key], key) !== (value ?? null));
  if (!changed.length) return null;
  const op = { kind, [idField]: id, from, clear: [] };
  for (const [key, value] of changed) {
    if (value === null && clearable.includes(key)) op.clear.push(CLEAR_NAMES[key]);
    else op[key] = value;
  }
  return op;
}

/** Field edits of a position (`name`, `code`, `roleId`, `isStaff`); null when nothing differs from `view`. */
export function updatePositionOp(view, positionId, patch, from) {
  const position = positionOf(view, positionId);
  return position ? patchOp('positionUpdate', 'positionId', positionId, position, patch, POSITION_FIELDS, ['code', 'roleId'], from) : null;
}

/** Field edits of a unit (`name`, `code`, `typeId`, `color`). */
export function updateUnitOp(view, unitId, patch, from) {
  const unit = unitOf(view, unitId);
  return unit ? patchOp('unitUpdate', 'unitId', unitId, unit, patch, UNIT_FIELDS, ['code', 'typeId', 'color'], from) : null;
}

/** The assignment of `ref` (`{ positionId, subjectKey }`) in `view`. */
export function findAssignment(view, ref) {
  return (view.assignments ?? []).find((a) => a.position_id === ref.positionId && subjectKey(a.subject) === ref.subjectKey) ?? null;
}

export const assignmentRef = (assignment) => ({ positionId: assignment.position_id, subjectKey: subjectKey(assignment.subject) });

/** Type, share or primary flag of one assignment (`assignmentType`, `share`, `isPrimary`); null when nothing differs. */
export function updateAssignmentOp(view, ref, patch, from) {
  const current = findAssignment(view, ref);
  if (!current) return null;
  const wire = { assignmentType: 'assignment_type', share: 'share', isPrimary: 'is_primary' };
  const changed = Object.entries(patch).filter(([key, value]) => current[wire[key]] !== value);
  if (!changed.length) return null;
  return { kind: 'assignmentUpdate', assignmentId: current.id, from, ...Object.fromEntries(changed) };
}

export const createUnitOp = (fields, from) => ({
  kind: 'unitCreate', name: fields.name, code: fields.code ?? null, typeId: fields.typeId ?? null,
  parentUnitId: fields.parentUnitId ?? null, color: fields.color ?? null, validFrom: from,
});

export const createPositionOp = (fields, from) => ({
  kind: 'positionCreate', unitId: fields.unitId, name: fields.name, code: fields.code ?? null, roleId: fields.roleId ?? null,
  isStaff: Boolean(fields.isStaff), parentPositionId: fields.parentPositionId ?? null, validFrom: from,
});

/**
 * Puts a person on a position. `person` is `{ kind: 'user'|'external', id }` or `{ kind: 'new_external', displayName, email? }`,
 * which adds the operation creating the external person first. `endPrevious` (an assignment) is ended the same day.
 */
export function assignOps(positionId, person, assignment, from, { endPrevious = null } = {}) {
  const ops = [];
  let subject = { kind: person.kind, id: person.id };
  if (person.kind === 'new_external') {
    subject = { kind: 'external', id: null };
    ops.push({ kind: 'externalPersonCreate', displayName: person.displayName, email: person.email ?? null, createsSubject: true });
  }
  ops.push({
    kind: 'assign', positionId, subject, assignmentType: assignment.assignmentType ?? 'permanent', share: assignment.share ?? 1,
    isPrimary: assignment.isPrimary ?? null, validFrom: from,
  });
  if (endPrevious) ops.push({ kind: 'assignmentEnd', assignmentId: endPrevious.id, from });
  return ops;
}

export const endAssignmentOp = (assignment, from) => ({ kind: 'assignmentEnd', assignmentId: assignment.id, from });

export const endPositionOp = (positionId, from) => ({ kind: 'positionEnd', positionId, from });

export const endUnitOp = (unitId, from) => ({ kind: 'unitEnd', unitId, from });

// ---- the draft as a list -----------------------------------------------------------

/** Every string in an operation's id fields, whatever their names: what "depends on a temporary id" is looked up in. */
function referencedIds(op) {
  const found = [];
  const walk = (value) => {
    if (typeof value === 'string') found.push(value);
    else if (Array.isArray(value)) value.forEach(walk);
    else if (value && typeof value === 'object') Object.values(value).forEach(walk);
  };
  const { kind, tempId, ...fields } = op;
  walk(fields);
  return found;
}

/** Indices of `ops[index]` and of every later operation that (transitively) uses a temporary id it makes. */
export function withDependents(ops, index) {
  const removed = new Set([index]);
  const made = new Set();
  const note = (i) => { if (ops[i].tempId) made.add(ops[i].tempId); };
  note(index);
  for (let i = index + 1; i < ops.length; i += 1) {
    if (referencedIds(ops[i]).some((id) => made.has(id))) {
      removed.add(i);
      note(i);
    }
  }
  return [...removed].sort((a, b) => a - b);
}

/** The list without `index` and its dependents. */
export function withoutOp(ops, index) {
  const gone = new Set(withDependents(ops, index));
  return ops.filter((_, i) => !gone.has(i));
}

const creatorOf = (ops, tempId) => ops.findIndex((op) => op.tempId === tempId);

// What an edit of a thing the draft creates writes into its creating operation.
const FOLD = {
  unitUpdate: { creator: 'unitCreate', id: 'unitId', fields: ['name', 'code', 'typeId', 'color'] },
  positionUpdate: { creator: 'positionCreate', id: 'positionId', fields: ['name', 'code', 'roleId', 'isStaff'] },
  assignmentUpdate: { creator: 'assign', id: 'assignmentId', fields: ['assignmentType', 'share', 'isPrimary'] },
};
const CLEAR_FIELDS = { code: 'code', role_id: 'roleId', type_id: 'typeId', color: 'color' };

function foldUpdate(creating, op, spec) {
  const next = { ...creating };
  for (const key of spec.fields) if (key in op && op[key] !== undefined) next[key] = op[key];
  for (const name of op.clear ?? []) if (CLEAR_FIELDS[name]) next[CLEAR_FIELDS[name]] = null;
  return next;
}

// Edits of one thing on one day merge: the newest word on a field wins, whether it sets the field or clears it.
const FIELD_OF_CLEAR = Object.fromEntries(Object.entries(CLEAR_NAMES).map(([field, wire]) => [wire, field]));

function mergeUpdate(previous, op) {
  const { clear: earlier = [], ...rest } = previous;
  const next = { ...rest };
  const cleared = new Set(earlier);
  // Not every update names clearable fields (an assignment's does not), so `clear` is written only where one exists.
  const clearable = 'clear' in previous || 'clear' in op;
  for (const [key, value] of Object.entries(op)) {
    if (key === 'clear' || key === 'kind') continue;
    next[key] = value;
    cleared.delete(CLEAR_NAMES[key]);
  }
  for (const wire of op.clear ?? []) {
    cleared.add(wire);
    delete next[FIELD_OF_CLEAR[wire]];
  }
  return clearable ? { ...next, clear: [...cleared] } : next;
}

const SAME_TARGET = {
  unitUpdate: (a, b) => a.unitId === b.unitId && a.from === b.from,
  positionUpdate: (a, b) => a.positionId === b.positionId && a.from === b.from,
  assignmentUpdate: (a, b) => a.assignmentId === b.assignmentId && a.from === b.from,
};
const REPLACES = {
  unitMove: (a, b) => a.unitId === b.unitId,
  positionMove: (a, b) => a.positionId === b.positionId,
  headSet: (a, b) => a.unitId === b.unitId,
  deputyHeadsSet: (a, b) => a.unitId === b.unitId,
};

/**
 * `op` added to `ops`, folded where it describes what an earlier operation already does. Returns a new list.
 * `op` has its ids already in the draft's terms (temporary ids for what the draft creates).
 */
export function addOp(ops, op) {
  const fold = FOLD[op.kind];
  if (fold && isTempId(op[fold.id])) {
    const i = creatorOf(ops, op[fold.id]);
    if (i >= 0 && ops[i].kind === fold.creator) return ops.map((existing, j) => (j === i ? foldUpdate(existing, op, fold) : existing));
  }
  if (op.kind === 'unitMove' && isTempId(op.unitId)) {
    const i = creatorOf(ops, op.unitId);
    if (i >= 0) return ops.map((existing, j) => (j === i ? { ...existing, parentUnitId: op.newParentUnitId } : existing));
  }
  if (op.kind === 'positionMove' && isTempId(op.positionId)) {
    const i = creatorOf(ops, op.positionId);
    if (i >= 0) return ops.map((existing, j) => (j === i ? { ...existing, parentPositionId: op.newParentPositionId } : existing));
  }
  const ended = { unitEnd: op.unitId, positionEnd: op.positionId, assignmentEnd: op.assignmentId }[op.kind];
  if (isTempId(ended)) {
    const i = creatorOf(ops, ended);
    if (i >= 0) return withoutOp(ops, i);
  }
  const same = SAME_TARGET[op.kind];
  if (same) {
    const i = ops.findIndex((existing) => existing.kind === op.kind && same(existing, op));
    if (i >= 0) return ops.map((existing, j) => (j === i ? mergeUpdate(existing, op) : existing));
  }
  const replaces = REPLACES[op.kind];
  if (replaces) {
    const i = ops.findIndex((existing) => existing.kind === op.kind && replaces(existing, op));
    if (i >= 0) return ops.map((existing, j) => (j === i ? op : existing));
  }
  return [...ops, op];
}

/** The operation dated `day`; operations without a day (an external person) stay as they are. */
export function withDay(op, day) {
  if ('from' in op) return { ...op, from: day };
  if ('validFrom' in op) return { ...op, validFrom: day };
  return op;
}
