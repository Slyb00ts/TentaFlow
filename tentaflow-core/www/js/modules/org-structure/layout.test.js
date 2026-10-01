// =============================================================================
// File: modules/org-structure/layout.test.js
// Description: The tidy-tree layout: what has to hold for any chart — no two
//   boxes overlap, a parent sits centred over its children — and the shapes
//   the org chart adds (staff to the side, leaf teams stacked, stacks wrapped
//   into columns, folded nodes never stacked).
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { DEFAULT_METRICS, layoutForest, swapSizes, transposeLayout } from './layout.js';

const node = (id, w = 100, h = 40, extra = {}) => ({ id, w, h, ...extra });
const tree = (id, children, extra = {}) => node(id, 100, 40, { children, ...extra });

function overlaps(items) {
  const clashes = [];
  for (let i = 0; i < items.length; i += 1) {
    for (let j = i + 1; j < items.length; j += 1) {
      const a = items[i];
      const b = items[j];
      const apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
      if (!apart) clashes.push([a.id, b.id]);
    }
  }
  return clashes;
}

const centre = (item) => item.x + item.w / 2;

test('a single node sits at the padding with the padded size around it', () => {
  const { items, width, height } = layoutForest([node('a', 120, 50)]);
  assert.equal(items.length, 1);
  assert.equal(items[0].x, DEFAULT_METRICS.pad);
  assert.equal(items[0].y, DEFAULT_METRICS.pad);
  assert.equal(width, 120 + DEFAULT_METRICS.pad * 2);
  assert.equal(height, 50 + DEFAULT_METRICS.pad * 2);
});

test('an empty forest is just the padding', () => {
  const result = layoutForest([]);
  assert.equal(result.items.length, 0);
  assert.equal(result.width, DEFAULT_METRICS.pad * 2);
});

test('a parent is centred over its first and last child', () => {
  const { byId } = layoutForest([tree('p', [
    tree('a', [node('a1'), node('a2')]),
    tree('b', [node('b1')]),
    tree('c', [node('c1'), node('c2'), node('c3')]),
  ])]);
  const p = byId.get('p');
  const first = byId.get('a');
  const last = byId.get('c');
  assert.equal(centre(p), (centre(first) + centre(last)) / 2);
  assert.equal(centre(byId.get('b')) > centre(first), true);
});

test('a wide level never overlaps and keeps the horizontal gap', () => {
  const kids = Array.from({ length: 40 }, (_, i) => tree(`k${i}`, [node(`k${i}x`), node(`k${i}y`)]));
  const { items, byId } = layoutForest([tree('root', kids)]);
  assert.deepEqual(overlaps(items), []);
  const a = byId.get('k0');
  const b = byId.get('k1');
  assert.ok(b.x - (a.x + a.w) >= DEFAULT_METRICS.hgap);
});

test('a deep chain grows downward by one row per level without exhausting the stack', () => {
  let leaf = node('n2000');
  for (let i = 1999; i >= 0; i -= 1) leaf = tree(`n${i}`, [leaf]);
  const { items, height } = layoutForest([leaf]);
  assert.equal(items.length, 2001);
  assert.ok(height > 2000 * 40);
  assert.equal(new Set(items.map((item) => item.x)).size, 1);
});

test('variable widths: a wide subtree pushes its narrow neighbour away and nothing collides', () => {
  const wide = node('wide', 400, 40, { children: [node('w1', 300), node('w2', 300)] });
  const narrow = node('narrow', 60, 40, { children: [node('n1', 60), node('n2', 60)] });
  const { items, byId } = layoutForest([tree('root', [wide, narrow])]);
  assert.deepEqual(overlaps(items), []);
  assert.ok(byId.get('narrow').x >= byId.get('w2').x + 300);
});

test('a deep subtree on the left keeps a shallow right sibling out of its lower rows', () => {
  const left = tree('l', [tree('l1', [tree('l2', [node('l3', 200), node('l4', 200)])])]);
  const right = tree('r', [tree('r1', [node('r2', 200), node('r3', 200)])]);
  const { items } = layoutForest([tree('root', [left, right])]);
  assert.deepEqual(overlaps(items), []);
});

test('leaf-only children stack under the manager, indented, on tight rows', () => {
  const { byId } = layoutForest([tree('boss', [node('a'), node('b'), node('c')])]);
  const boss = byId.get('boss');
  const a = byId.get('a');
  const b = byId.get('b');
  assert.equal(a.role, 'stacked');
  assert.equal(a.x, boss.x + DEFAULT_METRICS.stackIndent);
  assert.equal(b.x, a.x);
  assert.equal(a.y - (boss.y + boss.h), DEFAULT_METRICS.stackTopGap);
  assert.equal(b.y - (a.y + a.h), DEFAULT_METRICS.tightGap);
});

test('a single leaf child is a normal child, not a stack', () => {
  const { byId } = layoutForest([tree('boss', [node('only')])]);
  assert.equal(byId.get('only').role, 'node');
  assert.equal(centre(byId.get('only')), centre(byId.get('boss')));
});

test('a child folded away (more > 0) still stacks: its pill sits beside the card', () => {
  const { byId } = layoutForest([tree('boss', [node('a', 100, 40, { more: 5 }), node('b')])]);
  assert.equal(byId.get('a').role, 'stacked');
  assert.equal(byId.get('b').role, 'stacked');
});

test('a child with staff of its own is not a leaf', () => {
  const { byId } = layoutForest([tree('boss', [node('a', 100, 40, { staff: [node('s')] }), node('b')])]);
  assert.equal(byId.get('a').role, 'node');
  assert.equal(byId.get('a').y, byId.get('b').y);
});

test('a long stack wraps into columns filled top-down', () => {
  const kids = Array.from({ length: 20 }, (_, i) => node(`m${i}`));
  const { items, byId } = layoutForest([tree('boss', kids)]);
  const columns = new Set(kids.map((k) => byId.get(k.id).x));
  assert.equal(columns.size, 3);
  assert.equal(byId.get('m0').x, byId.get('m1').x);
  assert.ok(byId.get('m6').y > byId.get('m0').y);
  assert.equal(byId.get('m7').y, byId.get('m0').y, 'the next column starts back at the top');
  assert.deepEqual(overlaps(items), []);
});

test('a stack claims its rows: a neighbouring deeper subtree cannot slide underneath it', () => {
  const team = tree('team', Array.from({ length: 6 }, (_, i) => node(`t${i}`)));
  const other = tree('other', [tree('o1', [node('o2'), node('o3')])]);
  const { items } = layoutForest([tree('root', [team, other])]);
  assert.deepEqual(overlaps(items), []);
});

test('staff sit to the right of their manager on the same row and do not move the reports', () => {
  const plain = layoutForest([tree('boss', [tree('a', [node('a1'), node('a2')]), tree('b', [node('b1'), node('b2')])])]);
  const withStaff = layoutForest([tree('boss', [tree('a', [node('a1'), node('a2')]), tree('b', [node('b1'), node('b2')])], {
    staff: [node('s1'), node('s2')],
  })]);
  const boss = withStaff.byId.get('boss');
  const s1 = withStaff.byId.get('s1');
  const s2 = withStaff.byId.get('s2');
  assert.equal(s1.role, 'staff');
  assert.equal(s1.y, boss.y);
  assert.equal(s1.x, boss.x + boss.w + DEFAULT_METRICS.staffGap);
  assert.equal(s2.x, s1.x + s1.w + DEFAULT_METRICS.staffGap);
  assert.equal(withStaff.byId.get('a').x - withStaff.byId.get('boss').x,
    plain.byId.get('a').x - plain.byId.get('boss').x);
  assert.deepEqual(overlaps(withStaff.items), []);
});

test('staff of one manager push the neighbouring subtree so they never collide', () => {
  const left = tree('left', [node('l1'), node('l2')], { staff: [node('ls', 300)] });
  const right = tree('right', [tree('r1', [node('r2'), node('r3')])]);
  const { items } = layoutForest([tree('root', [left, right])]);
  assert.deepEqual(overlaps(items), []);
});

test('several roots are laid side by side with a wider gap', () => {
  const { items, byId } = layoutForest([tree('r1', [node('a'), node('b')]), tree('r2', [node('c'), node('d')])]);
  assert.deepEqual(overlaps(items), []);
  const r1 = byId.get('r1');
  const r2 = byId.get('r2');
  assert.equal(r1.y, r2.y);
  assert.ok(r2.x > r1.x);
});

test('rows are aligned by depth even when heights differ', () => {
  const { byId } = layoutForest([tree('root', [
    node('tall', 100, 90, { children: [node('t1'), node('t2', 100, 40, { more: 2 })] }),
    tree('short', [tree('s1', [node('s2')])]),
  ])]);
  assert.equal(byId.get('tall').y, byId.get('short').y);
  assert.equal(byId.get('t1').y, byId.get('s1').y);
  assert.ok(byId.get('t1').y >= byId.get('tall').y + 90);
});

test('a random forest of variable boxes never overlaps', () => {
  let seed = 7;
  const rand = () => { seed = (seed * 1664525 + 1013904223) % 4294967296; return seed / 4294967296; };
  let id = 0;
  const build = (depth) => {
    const w = 60 + Math.floor(rand() * 200);
    const count = depth >= 5 ? 0 : Math.floor(rand() * 5);
    const kids = Array.from({ length: count }, () => build(depth + 1));
    const staff = rand() < 0.15 ? [node(`s${id += 1}`, 80 + Math.floor(rand() * 100))] : [];
    return node(`n${id += 1}`, w, 40, { children: kids, staff, more: rand() < 0.1 ? 3 : 0 });
  };
  const { items } = layoutForest([build(0), build(0), build(0)]);
  assert.ok(items.length > 30);
  assert.deepEqual(overlaps(items), []);
});

test('a transposed layout of transposed boxes is a sideways chart with the same shape', () => {
  const roots = () => [tree('r', [tree('a', [node('a1', 60, 30)]), node('b', 80, 30)])];
  const upright = layoutForest(roots());
  const sideways = roots();
  swapSizes(sideways);
  const turned = layoutForest(sideways);
  swapSizes(sideways);
  transposeLayout(turned);
  const r = turned.byId.get('r');
  const a = turned.byId.get('a');
  assert.ok(a.x >= r.x + r.w, 'children are to the right');
  assert.equal(sideways[0].w, 100, 'the caller boxes are restored');
  assert.equal(upright.items.length, turned.items.length);
  assert.deepEqual(overlaps(turned.items), []);
});
