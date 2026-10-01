// =============================================================================
// File: modules/org-structure/tree.test.js
// Description: The model the org chart draws, from a structure answer: what a
//   card says (holder, vacancy, several holders), which badges it carries,
//   the numbers of a unit, and the two lookups of the Drzewo tab — the path from
//   the root and the search, which must not care about diacritics.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  buildTreeModel, fold, initialsOf, legendUnits, pathTo, searchNodes, userIdHex,
} from './tree.js';
import { meHex, sampleView, t } from './tree-fixture.js';

const build = (over = {}) => buildTreeModel(sampleView(), { t, meHex, ...over });
const node = (model, id) => model.nodes.find((n) => n.id === id);

test('a position with a holder is a person card, one without is a vacancy', () => {
  const model = build();
  const lead = node(model, 'p-lead');
  assert.equal(lead.name, 'Anna Kowalska');
  assert.equal(lead.role, 'Kierownik zespołu');
  assert.equal(lead.unitName, 'Realizacja');
  assert.equal(lead.vacant, false);
  const vacant = node(model, 'p-tester-auto');
  assert.equal(vacant.vacant, true);
  assert.equal(vacant.name, 'vacancy');
});

test('two holders of one position show the first and count the rest', () => {
  const view = sampleView();
  view.assignments.push({ ...view.assignments[0], id: 'a-extra', subject: { kind: 'external', id: 'x2' }, display_name: 'Second Holder' });
  const model = buildTreeModel(view, { t, meHex });
  const ceo = node(model, 'p-ceo');
  assert.equal(ceo.extraHolders, 1);
  assert.equal(ceo.people.length, 2);
});

test('badges: staff, deputy head, acting, and "you" for the caller', () => {
  const model = build();
  assert.deepEqual(node(model, 'p-assistant').badges, ['staff']);
  assert.deepEqual(node(model, 'p-cto-deputy').badges, ['deputy']);
  assert.deepEqual(node(model, 'p-lead-deputy').badges, ['acting']);
  assert.deepEqual(node(model, 'p-lead').badges, ['me']);
  assert.deepEqual(node(model, 'p-dev1').badges, []);
});

test('absence is a badge only for people the caller says are away', () => {
  assert.deepEqual(node(build(), 'p-dev1').badges, []);
  const away = build({ absentKeys: new Set(['external:m-nowak']) });
  assert.deepEqual(node(away, 'p-dev1').badges, ['absent']);
  assert.deepEqual(node(away, 'p-sales-2').badges, ['absent'], 'a person on two seats is away on both');
});

test('a deputy in force is a "covering" badge, next to "absent" for somebody who is both', () => {
  const both = build({
    absentKeys: new Set(['external:m-nowak']),
    coveringKeys: new Set(['external:m-nowak']),
  });
  const holder = both.nodes.find((n) => n.people.some((p) => p.key === 'external:m-nowak'));
  assert.ok(holder.badges.includes('absent') && holder.badges.includes('covering'));
  const covering = build({ coveringKeys: new Set(['external:m-nowak']) });
  assert.equal(covering.nodes.filter((n) => n.badges.includes('covering')).length, 2, 'that person holds two seats, and covers on both');
  assert.equal(build().nodes.filter((n) => n.badges.includes('covering')).length, 0, 'nobody is marked without an answer');
});

test('"my position" lists the seats of the caller, the primary one first', () => {
  const view = sampleView();
  view.assignments = view.assignments.map((a) => (a.position_id === 'p-lead' ? { ...a, is_primary: false } : a));
  view.assignments.push({
    id: 'a-second', position_id: 'p-dev2', subject: { kind: 'user', id: '11111111-2222-3333-4444-555555555555' },
    assignment_type: 'permanent', share: 0.5, is_primary: true, valid_from: '2026-01-01', display_name: 'Anna Kowalska',
  });
  const model = buildTreeModel(view, { t, meHex });
  assert.deepEqual(model.meIds, ['p-dev2', 'p-lead']);
});

test('the caller is recognised from the raw session bytes as well as from a string', () => {
  const bytes = Uint8Array.from([0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00]);
  assert.equal(userIdHex(bytes), '112233445566778899aabbccddeeff00');
  assert.equal(userIdHex('11223344-5566-7788-99AA-BBCCDDEEFF00'), '112233445566778899aabbccddeeff00');
  assert.equal(userIdHex(null), '');
});

test('parents, children and the count of people below', () => {
  const model = build();
  assert.equal(node(model, 'p-ceo').parentId, null);
  assert.equal(node(model, 'p-lead').parentId, 'p-delivery');
  assert.deepEqual(node(model, 'p-lead').childIds.sort(), ['p-dev1', 'p-dev2', 'p-lead-deputy', 'p-tester', 'p-tester-auto']);
  assert.equal(node(model, 'p-lead').below, 4, 'four occupied seats below, the vacancy is not a person');
  assert.equal(node(model, 'p-delivery').below, 5);
});

test('a parent that is not in the answer makes the position a root instead of dropping it', () => {
  const view = sampleView();
  view.positions.find((p) => p.position_id === 'p-fin').primary_parent_position_id = 'gone';
  const model = buildTreeModel(view, { t, meHex });
  assert.equal(node(model, 'p-fin').parentId, null);
});

test('unit numbers: people, vacancies and the average span of control', () => {
  const model = build();
  const delivery = model.units.find((u) => u.id === 'u-delivery');
  assert.equal(delivery.people, 6);
  assert.equal(delivery.vacancies, 1);
  // Managers of the unit: Dyrektor Realizacji (1 report) and Kierownik (5 reports).
  assert.equal(delivery.span, 3);
  assert.equal(delivery.headId, 'p-delivery');
  const board = model.units.find((u) => u.id === 'u-board');
  assert.equal(board.total, 14, 'the board counts everyone below it');
  assert.equal(model.units.find((u) => u.id === 'u-fin').parentId, 'u-board');
});

test('functional lines are the pairs of positions the answer names', () => {
  assert.deepEqual(build().functional, [{ from: 'p-dev1', to: 'p-sales' }]);
});

test('a unit keeps the colour it was given, else one that is stable per id', () => {
  const view = sampleView();
  view.units[1].color = '#123456';
  view.units[2].color = 'javascript:alert(1)';
  const model = buildTreeModel(view, { t, meHex });
  const colours = Object.fromEntries(model.units.map((u) => [u.id, u.color]));
  assert.equal(colours['u-tech'], '#123456');
  assert.match(colours['u-delivery'], /^#[0-9a-f]{6}$/i, 'a colour that is not a hex value is replaced');
  assert.equal(buildTreeModel(sampleView(), { t, meHex }).units[0].color, model.units[0].color);
});

test('the path runs from the root down to the node, and is empty for an unknown one', () => {
  const model = build();
  assert.deepEqual(pathTo(model, 'p-dev1'), ['p-ceo', 'p-cto', 'p-delivery', 'p-lead', 'p-dev1']);
  assert.deepEqual(pathTo(model, 'p-ceo'), ['p-ceo']);
  assert.deepEqual(pathTo(model, 'nope'), []);
});

test('search ignores case and diacritics and lists the shallowest hit first', () => {
  const model = build();
  assert.deepEqual(searchNodes(model, 'wozniak'), ['p-cto']);
  assert.deepEqual(searchNodes(model, 'ZAJAC'), ['p-sales']);
  const developers = searchNodes(model, 'developer');
  assert.deepEqual(developers.sort(), ['p-dev1', 'p-dev2']);
  assert.deepEqual(searchNodes(model, 'kamin').length, 1);
  assert.deepEqual(searchNodes(model, '   '), []);
  assert.deepEqual(searchNodes(model, 'no such person'), []);
  const byUnit = searchNodes(model, 'realizacja');
  assert.equal(byUnit[0], 'p-delivery', 'the head of the matching unit outranks its team');
});

test('fold and initials handle Polish letters and one-word names', () => {
  assert.equal(fold('Łukasz Żółć'), 'lukasz zolc');
  assert.equal(initialsOf('Anna Maria Kowalska'), 'AM');
  assert.equal(initialsOf('Cher'), 'C');
  assert.equal(initialsOf(''), '?');
});

test('the legend lists the top units with the people under each', () => {
  const legend = legendUnits(build());
  assert.deepEqual(legend.map((u) => [u.name, u.total]), [
    ['Zarząd', 14], ['Technologia', 8], ['Handlowy', 3], ['Finanse', 2],
  ]);
});
