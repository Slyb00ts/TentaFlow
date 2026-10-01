// =============================================================================
// File: modules/org-structure/list-model.js
// Description: Pure model of the Lista tab: the rows of the people table (one
//   per assignment, plus one per vacant position), the filter chips with their
//   counts, the search, the row menu and what ending an assignment takes with
//   it. No DOM and no transport, so every rule is tested without a page. Wire
//   fields are read in their snake_case spelling, as in model.js.
//   The table shows only what the structure holds per row: who, on which
//   position, in which unit, under whom, since when. The origin of a field
//   (manual / import) is written to the audit trail, not to the structure, so
//   there is no "source" column — a column the data cannot back would lie.
// =============================================================================

import { subjectKey } from '/js/modules/org-structure/model.js';
import { fold } from '/js/modules/org-structure/tree.js';

/** Filter chips in the order of the mockup. `changes` needs a day, chosen next to it. */
export const CHIPS = ['all', 'vacant', 'changes', 'no_account'];

/** How far back "Zmiany od…" looks until the administrator picks a day. */
export const DEFAULT_CHANGES_DAYS = 30;

function positionHolders(view) {
  const holders = new Map();
  for (const assignment of view.assignments ?? []) {
    if (!holders.has(assignment.position_id)) holders.set(assignment.position_id, []);
    holders.get(assignment.position_id).push(assignment);
  }
  return holders;
}

// Positions in reading order: a manager, then everyone under them. Iterative, because the chain of
// command is data and a pathological one must not exhaust the call stack.
function orderedPositions(view, unitName) {
  const positions = view.positions ?? [];
  const ids = new Set(positions.map((p) => p.position_id));
  const byName = (a, b) => (unitName.get(a.unit_id) ?? '').localeCompare(unitName.get(b.unit_id) ?? '')
    || a.name.localeCompare(b.name);
  const children = new Map();
  const roots = [];
  for (const position of positions) {
    const parent = position.primary_parent_position_id;
    if (parent && ids.has(parent)) {
      if (!children.has(parent)) children.set(parent, []);
      children.get(parent).push(position);
    } else {
      roots.push(position);
    }
  }
  const out = [];
  const seen = new Set();
  const pending = roots.sort(byName).reverse();
  while (pending.length) {
    const position = pending.pop();
    if (seen.has(position.position_id)) continue;
    seen.add(position.position_id);
    out.push(position);
    pending.push(...(children.get(position.position_id) ?? []).sort(byName).reverse());
  }
  // A cycle that survived in the data has no root: its members are still people on the list.
  for (const position of positions) if (!seen.has(position.position_id)) out.push(position);
  return out;
}

/**
 * The rows of the people table, in reading order. A person on two positions has two rows; a vacant
 * position has one row with no person. `reportsTo` names the holders of the position above, or the
 * position itself when nobody sits there.
 */
export function listRows(view, t) {
  const unitName = new Map((view.units ?? []).map((u) => [u.unit_id, u.name]));
  const positions = new Map((view.positions ?? []).map((p) => [p.position_id, p]));
  const holders = positionHolders(view);

  const reportsTo = (position) => {
    const parent = position.primary_parent_position_id ? positions.get(position.primary_parent_position_id) : null;
    if (!parent) return '';
    const who = holders.get(parent.position_id) ?? [];
    return who.length
      ? who.map((a) => a.display_name || t('unknown_person')).join(', ')
      : `${parent.name} — ${t('vacancy')}`;
  };

  const rows = [];
  for (const position of orderedPositions(view, unitName)) {
    const base = {
      positionId: position.position_id,
      unitId: position.unit_id,
      positionName: position.name,
      unitName: unitName.get(position.unit_id) ?? '',
      reportsTo: reportsTo(position),
      parentPositionId: position.primary_parent_position_id ?? null,
      staff: Boolean(position.is_staff),
    };
    const who = holders.get(position.position_id) ?? [];
    if (!who.length) {
      rows.push({
        ...base, _id: `vacant:${position.position_id}`, vacant: true, assignmentId: null, subject: null,
        personName: t('vacancy'), external: false, since: position.valid_from, share: null, type: null, primary: false,
      });
      continue;
    }
    for (const assignment of who) {
      rows.push({
        ...base,
        _id: assignment.id,
        vacant: false,
        assignmentId: assignment.id,
        subject: assignment.subject,
        personName: assignment.display_name || t('unknown_person'),
        external: assignment.subject?.kind === 'external',
        since: assignment.valid_from,
        validTo: assignment.valid_to ?? null,
        share: assignment.share,
        type: assignment.assignment_type,
        primary: Boolean(assignment.is_primary),
      });
    }
  }
  return rows;
}

/** How many rows each chip would show. `since` is the day "Zmiany od…" counts from. */
export function chipCounts(rows, since) {
  return {
    all: rows.length,
    vacant: rows.filter((r) => r.vacant).length,
    changes: rows.filter((r) => isChange(r, since)).length,
    no_account: rows.filter((r) => r.external).length,
  };
}

function isChange(row, since) {
  return Boolean(since) && row.since >= since;
}

/** Rows a chip and the search box let through. The search is blind to case and diacritics. */
export function filterRows(rows, { chip = 'all', query = '', since = null } = {}) {
  const needle = fold(query).trim();
  return rows.filter((row) => {
    if (chip === 'vacant' && !row.vacant) return false;
    if (chip === 'no_account' && !row.external) return false;
    if (chip === 'changes' && !isChange(row, since)) return false;
    if (!needle) return true;
    return fold([row.personName, row.positionName, row.unitName, row.reportsTo].join(' ')).includes(needle);
  });
}

/** The row menu of the mockup's `org-person`; admin-only, so a non-admin gets none. */
export function menuItems(row, { isAdmin }) {
  if (!isAdmin) return [];
  if (row.vacant) {
    return [
      { id: 'assign', icon: 'edit' },
      { separator: true },
      { id: 'end_position', icon: 'trash', danger: true },
    ];
  }
  return [
    { id: 'edit', icon: 'edit' },
    { id: 'move', icon: 'arrow' },
    // A deputy is an account covering an account: a person without one has nobody to be covered.
    ...(row.subject?.kind === 'user' ? [{ id: 'deputy', icon: 'user' }] : []),
    { separator: true },
    // Only an account holds work to hand over; a person without one has none.
    ...(row.subject?.kind === 'user' ? [{ id: 'handover', icon: 'send', danger: true }] : []),
    { id: 'end', icon: 'trash', danger: true },
  ];
}

/**
 * Where a person can be moved to: the vacant positions, in any unit — the own unit too, a team can have
 * more than one seat to fill. A position belongs to one unit for good, so a move is an assignment change:
 * the person leaves this position and takes a vacant one.
 */
export function moveTargets(rows) {
  return rows
    .filter((r) => r.vacant)
    .map((r) => ({ id: r.positionId, unitId: r.unitId, label: `${r.unitName} — ${r.positionName}` }));
}

/**
 * What ending `row`'s assignment takes with it, as message keys the window turns into sentences.
 * Read from the structure the caller sees, so nothing is promised the server would not do.
 */
export function endConsequences(row, view, rows) {
  const out = [];
  const others = rows.filter((r) => r.positionId === row.positionId && r.assignmentId && r.assignmentId !== row.assignmentId);
  if (!others.length) out.push({ key: 'becomes_vacant', params: { position: row.positionName, unit: row.unitName } });
  const unit = (view.units ?? []).find((u) => u.unit_id === row.unitId);
  if (unit?.head_position_id === row.positionId && !others.length) {
    out.push({ key: 'unit_without_head', params: { unit: row.unitName } });
  }
  const reports = new Set(rows.filter((r) => r.parentPositionId === row.positionId).map((r) => r.positionId)).size;
  if (reports > 0) out.push({ key: 'keeps_reports', params: { count: reports } });
  const key = subjectKey(row.subject);
  const elsewhere = rows.filter((r) => r.assignmentId && r.assignmentId !== row.assignmentId && subjectKey(r.subject) === key);
  out.push({ key: 'person_leaves', params: { person: row.personName } });
  if (row.primary && elsewhere.length) out.push({ key: 'primary_moves', params: { count: elsewhere.length } });
  return out;
}

/** Everything the "assign to a position" write of `row` needs to be repeated as it was (the undo of an end). */
export function assignmentSnapshot(row) {
  return {
    positionId: row.positionId,
    subject: row.subject,
    assignmentType: row.type,
    share: row.share,
    isPrimary: row.primary,
    validTo: row.validTo ?? null,
  };
}
