// =============================================================================
// File: modules/org-structure/edit-templates.test.js
// Description: The starter structures. Each is consistent (every manager and
//   parent exists, one head per unit, codes are unique and carry the template's
//   prefix) and becomes the operations of a draft: units and positions with
//   temporary ids that depend on each other, heads and ordered deputies, and no
//   person anywhere.
// =============================================================================

import '../../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { TEMPLATES, templateCounts, templateOps, templateOutline } = await import('./edit-templates.js');
const { withDependents } = await import('./edit-ops.js');

const t = (key) => `«${key}»`;
const DAY = '2026-09-30';

test('there are the three templates of the plan, by id', () => {
  assert.deepEqual(TEMPLATES.map((x) => x.id), ['small', 'departments', 'divisions']);
});

for (const template of TEMPLATES) {
  test(`${template.id}: every parent unit and manager exists, each unit has exactly one head, and the codes are unique`, () => {
    const unitCodes = new Set();
    const positionCodes = new Set();
    for (const [code, , parent, positions] of template.units) {
      assert.ok(!unitCodes.has(code), `unit ${code} twice`);
      if (parent) assert.ok(unitCodes.has(parent), `${code} names a parent that is not declared before it`);
      unitCodes.add(code);
      assert.equal(positions.filter((p) => p[3].head).length, 1, `${code} needs one head`);
      for (const [pos] of positions) {
        assert.ok(!positionCodes.has(pos), `position ${pos} twice`);
        positionCodes.add(pos);
      }
    }
    for (const [, , , positions] of template.units) {
      for (const [pos, , manager] of positions) {
        if (manager) assert.ok(positionCodes.has(manager), `${pos} reports to an unknown ${manager}`);
      }
    }
    assert.equal(template.units.flatMap((u) => u[3]).filter((p) => !p[2]).length, 1, 'exactly one seat has no manager');
    const prefix = template.units[0][0];
    for (const code of [...unitCodes, ...positionCodes]) assert.ok(code.startsWith(prefix), `${code} does not carry the prefix ${prefix}`);
  });

  test(`${template.id}: the operations create what the template says, in an order where everything exists before it is used`, () => {
    const list = templateOps(template, t, DAY);
    const counts = templateCounts(template);
    assert.equal(list.filter((o) => o.kind === 'unitCreate').length, counts.units);
    assert.equal(list.filter((o) => o.kind === 'positionCreate').length, counts.positions);
    assert.equal(list.filter((o) => o.kind === 'headSet').length, counts.units, 'a head for every unit');
    assert.ok(list.every((o) => (o.from ?? o.validFrom) === DAY));
    assert.ok(list.every((o) => !('subject' in o) && o.kind !== 'assign'), 'no person');
    const made = new Set();
    for (const op of list) {
      const used = [op.parentUnitId, op.unitId, op.parentPositionId, op.headPositionId, ...(op.positionIds ?? [])].filter(Boolean);
      for (const id of used) assert.ok(made.has(id), `${op.kind} uses ${id} before it is made`);
      if (op.tempId) {
        assert.ok(op.tempId.length <= 64 && op.tempId.startsWith('tmp:'));
        assert.ok(!made.has(op.tempId), 'a temporary id is defined once');
        made.add(op.tempId);
      }
    }
  });
}

test('the divisions template gives each division a deputy director in the order of the file', () => {
  const list = templateOps(TEMPLATES.find((x) => x.id === 'divisions'), t, DAY);
  const deputies = list.filter((o) => o.kind === 'deputyHeadsSet');
  assert.equal(deputies.length, 3);
  for (const d of deputies) assert.equal(d.positionIds.length, 1);
});

test('the staff position is created as staff, and the whole template goes when its first unit is taken away', () => {
  const list = templateOps(TEMPLATES.find((x) => x.id === 'departments'), t, DAY);
  assert.ok(list.some((o) => o.kind === 'positionCreate' && o.isStaff));
  assert.equal(withDependents(list, 0).length, list.length, 'everything hangs off the root unit');
});

test('the same template loaded twice into one draft does not define one temporary id twice', () => {
  const a = templateOps(TEMPLATES[0], t, DAY).map((o) => o.tempId).filter(Boolean);
  const b = templateOps(TEMPLATES[0], t, DAY).map((o) => o.tempId).filter(Boolean);
  assert.equal(new Set([...a, ...b]).size, a.length + b.length);
});

test('the outline indents a unit under its parent and lists the position names', () => {
  const outline = templateOutline(TEMPLATES.find((x) => x.id === 'divisions'), t);
  assert.deepEqual(outline.map((o) => o.depth), [0, 1, 2, 1, 2, 1]);
  assert.deepEqual(outline[0].positions, ['«tpl_pos_ceo»', '«tpl_pos_assistant»']);
});
