// =============================================================================
// File: modules/org-structure/tree-fixture.js
// Description: Structure answers for the tests and the benchmark of the org
//   chart: a small hand-made company that exercises every shape the chart
//   draws (staff, vacancy, acting, deputy head, a person with two seats, a
//   functional line), and a synthetic generator of a given size.
// =============================================================================

const ME = '11111111-2222-3333-4444-555555555555';

export const meHex = ME.replace(/-/g, '');

export const labels = {
  tree: 'Org chart',
  hint: 'Drag to pan',
  vacancy: 'vacancy',
  badge: (kind) => ({ me: 'You', acting: 'acting', deputy: 'deputy', staff: 'staff', absent: 'absent' })[kind],
  people: (n) => `${n} people`,
  vacancies: (n) => `${n} vacancies`,
  span: (n) => `span ${n}`,
  expand: (n) => `Expand ${n}`,
  menu: (name) => `Actions: ${name}`,
  tools: {
    zoomIn: 'Zoom in', zoomOut: 'Zoom out', zoomReset: 'Reset zoom', fit: 'Fit', present: 'Present', export: 'Export',
  },
};

export const t = (key) => ({ vacancy: 'vacancy', unknown_person: 'unknown' })[key] ?? key;

function unit(id, name, parent, head, extra = {}) {
  return {
    id: `v-${id}`, unit_id: id, name, code: null, type_id: null, parent_unit_id: parent, color: null,
    head_position_id: head, deputy_head_position_ids: [], valid_from: '2026-01-01', ...extra,
  };
}

function position(id, unitId, name, parent, extra = {}) {
  return {
    id: `v-${id}`, position_id: id, unit_id: unitId, name, primary_parent_position_id: parent,
    functional_parent_position_ids: [], is_staff: false, valid_from: '2026-01-01', ...extra,
  };
}

function holder(positionId, key, name, extra = {}) {
  return {
    id: `a-${positionId}-${key}`, position_id: positionId, subject: { kind: key === 'me' ? 'user' : 'external', id: key === 'me' ? ME : key },
    assignment_type: 'permanent', share: 1, is_primary: true, valid_from: '2026-01-01', display_name: name, ...extra,
  };
}

/** A company of about twenty seats: five levels deep on the Realizacja branch. */
export function sampleView() {
  const units = [
    unit('u-board', 'Zarząd', null, 'p-ceo'),
    unit('u-tech', 'Technologia', 'u-board', 'p-cto', { deputy_head_position_ids: ['p-cto-deputy'] }),
    unit('u-delivery', 'Realizacja', 'u-tech', 'p-delivery'),
    unit('u-sales', 'Handlowy', 'u-board', 'p-sales'),
    unit('u-fin', 'Finanse', 'u-board', 'p-fin'),
  ];
  const positions = [
    position('p-ceo', 'u-board', 'Prezes Zarządu', null),
    position('p-assistant', 'u-board', 'Asystentka Zarządu', 'p-ceo', { is_staff: true }),
    position('p-cto', 'u-tech', 'Dyrektor Technologii', 'p-ceo'),
    position('p-cto-deputy', 'u-tech', 'Zastępca dyrektora', 'p-cto'),
    position('p-delivery', 'u-delivery', 'Dyrektor Realizacji', 'p-cto'),
    position('p-lead', 'u-delivery', 'Kierownik zespołu', 'p-delivery'),
    position('p-lead-deputy', 'u-delivery', 'Zastępca kierownika', 'p-lead'),
    position('p-dev1', 'u-delivery', 'Developer', 'p-lead', { functional_parent_position_ids: ['p-sales'] }),
    position('p-dev2', 'u-delivery', 'Developer', 'p-lead'),
    position('p-tester', 'u-delivery', 'Tester', 'p-lead'),
    position('p-tester-auto', 'u-delivery', 'Tester automatyzujący', 'p-lead'),
    position('p-sales', 'u-sales', 'Dyrektor Handlowy', 'p-ceo'),
    position('p-sales-1', 'u-sales', 'Handlowiec', 'p-sales'),
    position('p-sales-2', 'u-sales', 'Handlowiec', 'p-sales'),
    position('p-fin', 'u-fin', 'Dyrektor Finansów', 'p-ceo'),
    position('p-fin-1', 'u-fin', 'Księgowa', 'p-fin'),
  ];
  const assignments = [
    holder('p-ceo', 'k-malinowski', 'Krzysztof Malinowski'),
    holder('p-assistant', 'j-kaczmarek', 'Julia Kaczmarek'),
    holder('p-cto', 'a-wozniak', 'Adam Woźniak'),
    holder('p-cto-deputy', 'b-sikora', 'Beata Sikora'),
    holder('p-delivery', 'm-kaminska', 'Magdalena Kamińska'),
    holder('p-lead', 'me', 'Anna Kowalska'),
    holder('p-lead-deputy', 'p-szymanski', 'Paweł Szymański', { assignment_type: 'acting' }),
    holder('p-dev1', 'm-nowak', 'Marek Nowak'),
    holder('p-dev2', 'p-zielinski', 'Piotr Zieliński'),
    holder('p-tester', 'e-wisniewska', 'Ewa Wiśniewska'),
    holder('p-sales', 'm-zajac', 'Michał Zając'),
    holder('p-sales-1', 'a-lis', 'Aneta Lis'),
    holder('p-sales-2', 'm-nowak', 'Marek Nowak', { is_primary: false }),
    holder('p-fin', 'j-pawlak', 'Joanna Pawlak'),
    holder('p-fin-1', 'r-kot', 'Renata Kot'),
  ];
  const occupied = new Set(assignments.map((a) => a.position_id));
  return {
    at: '2026-09-30',
    timezone: 'Europe/Warsaw',
    units,
    positions,
    assignments,
    vacancies: positions.filter((p) => !occupied.has(p.position_id)).map((p) => p.position_id),
    warnings: [],
  };
}

/**
 * A synthetic company of `people` seats: `fanout` reports per manager, so the
 * depth follows from the size. Every tenth manager's team has a vacancy.
 */
export function syntheticView(people, fanout = 6) {
  const units = [];
  const positions = [];
  const assignments = [];
  let unitCount = 0;
  const addUnit = (parent, head) => {
    unitCount += 1;
    const id = `u${unitCount}`;
    units.push(unit(id, `Unit ${unitCount}`, parent, head));
    return id;
  };
  const rootUnit = addUnit(null, 'p0');
  positions.push(position('p0', rootUnit, 'Head', null));
  assignments.push(holder('p0', 'x0', 'Person 0'));
  const queue = [{ id: 'p0', unit: rootUnit }];
  let made = 1;
  while (made < people && queue.length) {
    const parent = queue.shift();
    const parentUnit = made % 40 === 1 ? addUnit(parent.unit, `p${made}`) : parent.unit;
    for (let i = 0; i < fanout && made < people; i += 1) {
      const id = `p${made}`;
      positions.push(position(id, parentUnit, `Role ${made % 17}`, parent.id));
      if (made % 10 !== 0) assignments.push(holder(id, `x${made}`, `Person ${made}`));
      queue.push({ id, unit: parentUnit });
      made += 1;
    }
  }
  const occupied = new Set(assignments.map((a) => a.position_id));
  return {
    at: '2026-09-30',
    timezone: 'Europe/Warsaw',
    units,
    positions,
    assignments,
    vacancies: positions.filter((p) => !occupied.has(p.position_id)).map((p) => p.position_id),
    warnings: [],
  };
}

/**
 * A company shaped like a real one: every manager has 5 to 8 reports (seeded, so runs compare),
 * a unit per second-level manager, and the depth follows from the size (about four levels
 * for two thousand people).
 */
export function realisticView(people) {
  let seed = 20260930;
  const rand = () => { seed = (seed * 1664525 + 1013904223) % 4294967296; return seed / 4294967296; };
  const units = [];
  const positions = [];
  const assignments = [];
  const addUnit = (parent, head) => {
    const id = `u${units.length + 1}`;
    units.push(unit(id, `Unit ${units.length + 1}`, parent, head));
    return id;
  };
  const rootUnit = addUnit(null, 'p0');
  positions.push(position('p0', rootUnit, 'Head', null));
  assignments.push(holder('p0', 'x0', 'Person 0'));
  const queue = [{ id: 'p0', unit: rootUnit, depth: 0 }];
  let made = 1;
  while (made < people && queue.length) {
    const parent = queue.shift();
    const parentUnit = parent.depth === 1 ? addUnit(rootUnit, `p${made}`) : parent.unit;
    const reports = Math.min(people - made, 5 + Math.floor(rand() * 4));
    for (let i = 0; i < reports; i += 1) {
      const id = `p${made}`;
      positions.push(position(id, parentUnit, `Role ${made % 17}`, parent.id));
      if (made % 25 !== 0) assignments.push(holder(id, `x${made}`, `Person ${made}`));
      queue.push({ id, unit: parentUnit, depth: parent.depth + 1 });
      made += 1;
    }
  }
  const occupied = new Set(assignments.map((a) => a.position_id));
  return {
    at: '2026-09-30',
    timezone: 'Europe/Warsaw',
    units,
    positions,
    assignments,
    vacancies: positions.filter((p) => !occupied.has(p.position_id)).map((p) => p.position_id),
    warnings: [],
  };
}
