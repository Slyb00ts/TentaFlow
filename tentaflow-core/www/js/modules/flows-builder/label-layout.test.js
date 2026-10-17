// =============================================================================
// File: modules/flows-builder/label-layout.test.js
// Description: Outside labels of BPMN elements never cover an element and no
//   leader line runs through another element, even on a crowded diagram.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { planProcessLabels, segmentHitsRect } from './label-layout.js';

const TASK = { width: 200, height: 72 };
const EVENT = 36;

// 12 tasks 320 px apart on a row, each with 4 boundary timers on its borders.
function crowded() {
  const nodes = [];
  const labels = [];
  for (let index = 0; index < 12; index += 1) {
    const left = index * 320;
    const top = 300;
    const task = { id: `task${index}`, left, top, right: left + TASK.width, bottom: top + TASK.height };
    nodes.push(task);
    const spots = [[left + 20, top + TASK.height], [left + 80, top + TASK.height], [left + 140, top + TASK.height], [left + TASK.width, top + 36]];
    spots.forEach(([cx, cy], slot) => {
      const event = { id: `timer${index}_${slot}`, left: cx - EVENT / 2, top: cy - EVENT / 2, right: cx + EVENT / 2, bottom: cy + EVENT / 2 };
      nodes.push(event);
      labels.push({ ...event, host: task, external: false, width: 130, height: 40 });
    });
  }
  return { nodes, labels };
}

const shrink = (rect) => ({ left: rect.left + 3, top: rect.top + 3, right: rect.right - 3, bottom: rect.bottom - 3 });

test('on a crowded diagram no label covers an element and no leader crosses one', () => {
  const { nodes, labels } = crowded();
  const plan = planProcessLabels({ nodes, labels });
  assert.equal(plan.size, labels.length);
  let placed = 0;
  for (const label of labels) {
    const spot = plan.get(label.id);
    if (!spot) continue;
    placed += 1;
    const { rect, leader } = spot;
    for (const node of nodes) {
      assert.ok(rect.right <= node.left || rect.left >= node.right || rect.bottom <= node.top || rect.top >= node.bottom,
        `${label.id} covers ${node.id}`);
      if (node.id === label.id) continue;
      assert.equal(segmentHitsRect(leader.sourceX, leader.sourceY, leader.targetX, leader.targetY, shrink(node)), false,
        `the leader of ${label.id} crosses ${node.id}`);
    }
    assert.ok(Math.hypot(leader.targetX - leader.sourceX, leader.targetY - leader.sourceY) <= 260, `${label.id} leader is short`);
  }
  assert.ok(placed > 0, 'some labels are placed');
});

test('a label whose only free spots lie beyond a wall is deferred, not forced through it', () => {
  const event = { id: 'event', left: 0, top: 0, right: 36, bottom: 36 };
  const walls = [
    { id: 'w1', left: -40, top: -40, right: 76, bottom: -30 }, { id: 'w2', left: -40, top: 66, right: 76, bottom: 76 },
    { id: 'w3', left: -40, top: -40, right: -30, bottom: 76 }, { id: 'w4', left: 66, top: -40, right: 76, bottom: 76 },
  ];
  const plan = planProcessLabels({ nodes: [event, ...walls], labels: [{ ...event, host: null, external: false, width: 80, height: 30 }] });
  assert.equal(plan.get('event'), null);
});
