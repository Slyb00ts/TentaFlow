// =============================================================================
// File: modules/org-structure/tree.js
// Description: Turns an `OrgStructureBody` structure answer into the model the
//   tf-org-tree component draws — one node per position (a card for its holder,
//   or a vacancy), one entry per unit with its numbers — and answers the
//   questions the Drzewo tab asks of it: which node is mine, what is the path
//   from the root to a found person, who matches a search. Pure: no DOM, no
//   transport, so every rule is tested without a page. Wire fields are read in
//   their snake_case spelling, as in model.js.
// =============================================================================

import { subjectKey } from '/js/modules/org-structure/model.js';

// Unit colours when the unit has none of its own: stable per unit id so a unit
// keeps its colour across reloads and across the persons and units views.
const PALETTE = ['#6366f1', '#38bdf8', '#34d399', '#f59e0b', '#a78bfa', '#f472b6', '#fb7185', '#2dd4bf'];
const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{6})$/i;

function hash(text) {
  let h = 2166136261;
  for (let i = 0; i < text.length; i += 1) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

/** The colour of a unit: its own, or one derived from its id so it never changes between loads. */
export function unitColor(unit) {
  if (unit.color && HEX_COLOR.test(unit.color)) return unit.color;
  return PALETTE[hash(unit.unit_id) % PALETTE.length];
}

/** First letters of the first two words; the card shows them where a photo will go later. */
export function initialsOf(name) {
  const parts = String(name ?? '').trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return '?';
  return parts.slice(0, 2).map((part) => Array.from(part)[0]).join('').toUpperCase();
}

/** Lower-cased, diacritics-free text so "Wozniak" finds "Woźniak". */
export function fold(text) {
  return String(text ?? '').normalize('NFD').replace(/[̀-ͯ]/g, '').replace(/ł/gi, 'l').toLowerCase();
}

/** The session user id arrives as 16 raw bytes; subjects carry a canonical UUID string. */
export function userIdHex(userId) {
  if (!userId) return '';
  if (typeof userId === 'string') return userId.toLowerCase().replace(/-/g, '');
  return Array.from(userId, (b) => (b & 0xff).toString(16).padStart(2, '0')).join('');
}

/** The same id as the canonical dashed UUID text (`8-4-4-4-12`); an id that is not 32 hex digits comes back as it is. */
export function userIdText(userId) {
  const hex = userIdHex(userId);
  if (!/^[0-9a-f]{32}$/.test(hex)) return typeof userId === 'string' ? userId : '';
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}

function isMe(subject, meHex) {
  return Boolean(meHex) && subject?.kind === 'user' && userIdHex(subject.id) === meHex;
}

function groupBy(items, keyOf) {
  const map = new Map();
  for (const item of items) {
    const key = keyOf(item);
    if (!map.has(key)) map.set(key, []);
    map.get(key).push(item);
  }
  return map;
}

/**
 * @param {object} view the `view` of a structure answer
 * @param {{ unitTypes?: Array, meHex?: string, absentKeys?: Set<string>, coveringKeys?: Set<string>,
 *   t: (key: string) => string }} options
 *   `absentKeys` holds `subjectKey`s of people away on the day ("nieobecny", never why);
 *   `coveringKeys` those of people who are a deputy in force ("zastępstwo").
 */
export function buildTreeModel(view, options) {
  const {
    unitTypes = [], meHex = '', absentKeys = new Set(), coveringKeys = new Set(), t,
  } = options;
  const units = view.units ?? [];
  const positions = view.positions ?? [];
  const unitById = new Map(units.map((u) => [u.unit_id, u]));
  const typeName = new Map(unitTypes.map((type) => [type.id, type.name]));
  const holders = groupBy(view.assignments ?? [], (a) => a.position_id);
  const deputyPositions = new Set(units.flatMap((u) => u.deputy_head_position_ids ?? []));
  const positionIds = new Set(positions.map((p) => p.position_id));

  const nodes = positions.map((position) => {
    const unit = unitById.get(position.unit_id);
    const people = (holders.get(position.position_id) ?? []).map((a) => ({
      key: subjectKey(a.subject),
      name: a.display_name || t('unknown_person'),
      share: a.share,
      type: a.assignment_type,
      primary: Boolean(a.is_primary),
      me: isMe(a.subject, meHex),
    }));
    const vacant = people.length === 0;
    const badges = [];
    if (position.is_staff) badges.push('staff');
    if (deputyPositions.has(position.position_id)) badges.push('deputy');
    if (people.some((p) => p.type === 'acting')) badges.push('acting');
    if (people.some((p) => absentKeys.has(p.key))) badges.push('absent');
    if (people.some((p) => coveringKeys.has(p.key))) badges.push('covering');
    if (people.some((p) => p.me)) badges.push('me');
    const name = vacant ? t('vacancy') : people[0].name;
    return {
      id: position.position_id,
      // A parent outside the answer (ended, or another cut of history) makes this a root
      // rather than dropping the branch.
      parentId: position.primary_parent_position_id && positionIds.has(position.primary_parent_position_id)
        ? position.primary_parent_position_id
        : null,
      name,
      extraHolders: Math.max(0, people.length - 1),
      role: position.name,
      unitId: position.unit_id,
      unitName: unit?.name ?? '',
      color: unit ? unitColor(unit) : PALETTE[0],
      vacant,
      staff: Boolean(position.is_staff),
      badges,
      people,
      since: position.valid_from,
      below: 0,
      searchText: fold([name, ...people.map((p) => p.name), position.name, unit?.name ?? ''].join(' ')),
    };
  });

  const nodeById = new Map(nodes.map((n) => [n.id, n]));
  const childrenOf = groupBy(nodes.filter((n) => n.parentId), (n) => n.parentId);
  for (const n of nodes) n.childIds = (childrenOf.get(n.id) ?? []).map((c) => c.id);
  fillBelow(nodes, nodeById);

  const functional = [];
  for (const position of positions) {
    for (const parent of position.functional_parent_position_ids ?? []) {
      if (positionIds.has(parent)) functional.push({ from: position.position_id, to: parent });
    }
  }

  const unitModels = buildUnits(units, nodes, nodeById, typeName);
  const unitOfNode = new Map(unitModels.map((u) => [u.id, u]));
  const headOf = new Set(unitModels.map((u) => u.headId).filter(Boolean));
  for (const n of nodes) {
    n.isUnitHead = headOf.has(n.id);
    n.unitPeople = unitOfNode.get(n.unitId)?.people ?? 0;
  }
  const meIds = nodes
    .filter((n) => n.people.some((p) => p.me))
    .sort((a, b) => Number(b.people.some((p) => p.me && p.primary)) - Number(a.people.some((p) => p.me && p.primary)))
    .map((n) => n.id);

  return { nodes, units: unitModels, functional, meIds };
}

// Post-order sums of occupied positions below each node. Iterative: the chain of command is
// data, and a pathological one must not exhaust the call stack.
function fillBelow(nodes, nodeById) {
  const order = [];
  const pending = nodes.filter((n) => !n.parentId);
  const seen = new Set();
  while (pending.length) {
    const n = pending.pop();
    if (seen.has(n.id)) continue;
    seen.add(n.id);
    order.push(n);
    for (const id of n.childIds) pending.push(nodeById.get(id));
  }
  for (let i = order.length - 1; i >= 0; i -= 1) {
    const n = order[i];
    n.below = n.childIds.reduce((sum, id) => {
      const child = nodeById.get(id);
      return sum + child.below + (child.vacant ? 0 : 1);
    }, 0);
  }
}

function buildUnits(units, nodes, nodeById, typeName) {
  const byUnit = groupBy(nodes, (n) => n.unitId);
  const unitIds = new Set(units.map((u) => u.unit_id));
  const models = units.map((unit) => {
    const members = byUnit.get(unit.unit_id) ?? [];
    const people = new Set(members.flatMap((m) => m.people.map((p) => p.key)));
    const managers = members.filter((m) => m.childIds.length > 0);
    const reports = managers.reduce((sum, m) => sum + m.childIds.length, 0);
    return {
      id: unit.unit_id,
      parentId: unit.parent_unit_id && unitIds.has(unit.parent_unit_id) ? unit.parent_unit_id : null,
      name: unit.name,
      code: unit.code ?? '',
      typeName: (unit.type_id && typeName.get(unit.type_id)) || '',
      color: unitColor(unit),
      headId: unit.head_position_id && nodeById.has(unit.head_position_id) ? unit.head_position_id : null,
      memberIds: members.map((m) => m.id),
      people: people.size,
      vacancies: members.filter((m) => m.vacant).length,
      span: managers.length ? Math.round((reports / managers.length) * 10) / 10 : null,
      total: 0,
      childIds: [],
    };
  });
  const byId = new Map(models.map((u) => [u.id, u]));
  for (const u of models) if (u.parentId) byId.get(u.parentId).childIds.push(u.id);
  // People of the unit and everything under it (each person once): the number the legend shows.
  for (const u of models) {
    const people = new Set();
    const visited = new Set();
    const pending = [u];
    while (pending.length) {
      const cursor = pending.pop();
      if (visited.has(cursor.id)) continue;
      visited.add(cursor.id);
      for (const m of byUnit.get(cursor.id) ?? []) for (const p of m.people) people.add(p.key);
      for (const id of cursor.childIds) pending.push(byId.get(id));
    }
    u.total = people.size;
  }
  return models;
}

/** Ids from the root down to `id`, inclusive; empty when the node is unknown. */
export function pathTo(model, id) {
  const byId = new Map(model.nodes.map((n) => [n.id, n]));
  const path = [];
  const seen = new Set();
  let cursor = byId.get(id);
  while (cursor && !seen.has(cursor.id)) {
    seen.add(cursor.id);
    path.push(cursor.id);
    cursor = cursor.parentId ? byId.get(cursor.parentId) : null;
  }
  return path.reverse();
}

/** Nodes whose holder, position or unit contains the query, shallowest first so the best hit leads. */
export function searchNodes(model, query) {
  const needle = fold(query).trim();
  if (!needle) return [];
  const byId = new Map(model.nodes.map((n) => [n.id, n]));
  const depth = (n) => {
    let d = 0;
    for (let c = n; c.parentId && d < 1000; c = byId.get(c.parentId)) d += 1;
    return d;
  };
  return model.nodes
    .filter((n) => n.searchText.includes(needle))
    .map((n) => ({ n, d: depth(n) }))
    .sort((a, b) => a.d - b.d || a.n.name.localeCompare(b.n.name))
    .map(({ n }) => n.id);
}

/** Units near the top of the tree with the people under each — the legend under the chart. */
export function legendUnits(model, limit = 8) {
  const byId = new Map(model.units.map((u) => [u.id, u]));
  const depthOf = (u) => {
    let d = 0;
    for (let c = u; c.parentId && d < 1000; c = byId.get(c.parentId)) d += 1;
    return d;
  };
  return model.units
    .map((u) => ({ u, d: depthOf(u) }))
    .filter(({ d }) => d <= 1)
    .sort((a, b) => a.d - b.d || b.u.total - a.u.total)
    .slice(0, limit)
    .map(({ u }) => ({ id: u.id, name: u.name, color: u.color, total: u.total }));
}
