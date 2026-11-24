// ===== File: modules/tentabus/node-label.test.js — how a node is named on every screen =====

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { nodeLabel, nodeTitle, nodeNamer, nodeTitler, shortNodeId } = await import('./node-label.js');

const HEX = '41291e2758d793a2d740f5db7d7c99141f32f02199c1427803d4373636e7ca57';

test('a node with a label is named by it, with no tooltip', () => {
  const node = { nodeId: HEX, label: 'Serwer A' };
  assert.equal(nodeLabel(node), 'Serwer A');
  assert.equal(nodeTitle(node), '');
});

test('a node without a label, or whose label only repeats its id, is named by its first 8 characters with the full id as tooltip', () => {
  for (const label of [undefined, null, '', '   ', HEX]) {
    const node = { nodeId: HEX, label };
    assert.equal(nodeLabel(node), '41291e27', JSON.stringify(label));
    assert.equal(nodeTitle(node), HEX);
  }
  assert.equal(shortNodeId(HEX), '41291e27');
  assert.equal(shortNodeId('ab'), 'ab');
  assert.equal(shortNodeId(undefined), '');
});

test('names by id over a node list: known nodes by their label, unknown ones shortened, no id as nothing', () => {
  const nodes = [{ nodeId: 'a', label: 'Serwer A' }, { nodeId: HEX, label: '' }];
  const name = nodeNamer(nodes);
  const title = nodeTitler(nodes);
  assert.deepEqual([name('a'), name(HEX), name(`${HEX}ff`), name(''), name(null)], ['Serwer A', '41291e27', '41291e27', '', '']);
  assert.deepEqual([title('a'), title(HEX), title(''), title(undefined)], ['', HEX, '', '']);
  assert.equal(nodeNamer(null)('0123456789abcdef'), '01234567');
});
