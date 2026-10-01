// =============================================================================
// File: modules/org-structure/edit-rules.js
// Description: The pure rules of the edit mode: what a drag or a menu may do to a
//   position or a unit, before anything is sent. The server checks every one of
//   these again (a cycle, a staff manager, a non-empty unit are typed refusals),
//   so this module exists to say "no" — and why — at the moment of the drop and
//   to count who a move touches, not to be the authority. No DOM, no transport.
// =============================================================================

import { subjectKey } from '/js/modules/org-structure/model.js';

/** Ids of `rootId` and everything below it in a tree of `{ id, childIds }` entries. Iterative and cycle-safe. */
export function branchIds(byId, rootId) {
  const seen = new Set();
  const pending = [rootId];
  while (pending.length) {
    const id = pending.pop();
    if (seen.has(id) || !byId.has(id)) continue;
    seen.add(id);
    pending.push(...byId.get(id).childIds);
  }
  return seen;
}

const nodeMap = (model) => new Map(model.nodes.map((n) => [n.id, n]));
const unitMap = (model) => new Map(model.units.map((u) => [u.id, u]));

/**
 * Whom moving `positionId` under a new manager concerns: the people on the position and on
 * everything below it (their chain of command changes), and how many of them report to it directly.
 */
export function reparentImpact(model, positionId) {
  const byId = nodeMap(model);
  const people = new Set();
  for (const id of branchIds(byId, positionId)) {
    for (const person of byId.get(id).people) people.add(person.key);
  }
  const node = byId.get(positionId);
  return { people: people.size, direct: node ? node.childIds.length : 0 };
}

/**
 * Whether `sourceId` may be placed under `targetId` (null = no manager).
 * `{ ok: true }` or `{ ok: false, reason }` with reason `unknown | self | cycle | staff | same`.
 */
export function positionMoveCheck(model, sourceId, targetId) {
  const byId = nodeMap(model);
  const source = byId.get(sourceId);
  if (!source) return { ok: false, reason: 'unknown' };
  if (targetId == null) return source.parentId == null ? { ok: false, reason: 'same' } : { ok: true };
  const target = byId.get(targetId);
  if (!target) return { ok: false, reason: 'unknown' };
  if (targetId === sourceId) return { ok: false, reason: 'self' };
  if (branchIds(byId, sourceId).has(targetId)) return { ok: false, reason: 'cycle' };
  if (target.staff) return { ok: false, reason: 'staff' };
  if (source.parentId === targetId) return { ok: false, reason: 'same' };
  return { ok: true };
}

/** The same question for units: a unit cannot move under itself or under one of its own descendants. */
export function unitMoveCheck(model, sourceId, targetId) {
  const byId = unitMap(model);
  const source = byId.get(sourceId);
  if (!source) return { ok: false, reason: 'unknown' };
  if (targetId == null) return source.parentId == null ? { ok: false, reason: 'same' } : { ok: true };
  if (!byId.has(targetId)) return { ok: false, reason: 'unknown' };
  if (targetId === sourceId) return { ok: false, reason: 'self' };
  if (branchIds(byId, sourceId).has(targetId)) return { ok: false, reason: 'cycle' };
  if (source.parentId === targetId) return { ok: false, reason: 'same' };
  return { ok: true };
}

/** A person dropped from the list of people without a seat may only land on a position nobody holds. */
export function vacancyDropCheck(model, positionId) {
  const node = nodeMap(model).get(positionId);
  if (!node) return { ok: false, reason: 'unknown' };
  return node.vacant ? { ok: true } : { ok: false, reason: 'occupied' };
}

/** What the chart's `dropRule` answers for a drag of a card: positions move under positions, units under units. */
export function cardDropRule(model) {
  return (source, target) => {
    if (source.kind !== target.kind) return { ok: false, reason: 'unknown' };
    return source.kind === 'unit'
      ? unitMoveCheck(model, source.id, target.id)
      : positionMoveCheck(model, source.id, target.id);
  };
}

/**
 * Candidates for "reports to" of `positionId`, as the shared move window wants them:
 * `{ id, label, disabled? }`, where `disabled` is the reason the position cannot be chosen.
 */
export function moveTargets(model, positionId, { describe, reasons }) {
  return model.nodes
    .filter((n) => n.id !== positionId)
    .map((n) => {
      const check = positionMoveCheck(model, positionId, n.id);
      const target = { id: n.id, label: describe(n) };
      if (!check.ok) target.disabled = reasons[check.reason];
      return target;
    });
}

/** Unit candidates for "parent unit" of `unitId`, in the same shape. */
export function unitMoveTargets(model, unitId, { reasons }) {
  return model.units
    .filter((u) => u.id !== unitId)
    .map((u) => {
      const check = unitMoveCheck(model, unitId, u.id);
      const target = { id: u.id, label: u.name };
      if (!check.ok) target.disabled = reasons[check.reason];
      return target;
    });
}

/** Moves the entry at `from` to `to` in a copy of `list` (the deputy heads' order). */
export function reorder(list, from, to) {
  if (from === to || from < 0 || to < 0 || from >= list.length || to >= list.length) return list.slice();
  const next = list.slice();
  const [moved] = next.splice(from, 1);
  next.splice(to, 0, moved);
  return next;
}

/** Positions of `unitId` that may still be named its head or a deputy: neither of the two already. */
export function unitSeatCandidates(view, unitId) {
  const unit = (view.units ?? []).find((u) => u.unit_id === unitId);
  if (!unit) return [];
  const taken = new Set([unit.head_position_id, ...(unit.deputy_head_position_ids ?? [])].filter(Boolean));
  return (view.positions ?? []).filter((p) => p.unit_id === unitId && !taken.has(p.position_id));
}

/** Users with no assignment in `view`: the people the "without a position" strip offers. */
export function peopleWithoutPosition(users, view) {
  const seated = new Set((view.assignments ?? []).map((a) => subjectKey(a.subject)));
  return users.filter((u) => u.isActive !== false && !seated.has(`user:${u.id}`));
}
