// =============================================================================
// File: components/tf-entanglement-graph.test.js
// Description: The entanglement map draws TWO numbers per pair and must not
// merge them, so the tests pin exactly that: thickness follows mutual
// information, colour follows concurrence, and a correlated pair with zero
// concurrence looks different from a Bell pair.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  concurrenceColor, edgeWidth, graphLayout, visibleEdges, MAX_MUTUAL_INFORMATION, TfEntanglementGraph,
} = await import('./tf-entanglement-graph.js');

test('the qubits keep their circuit order, one row each', () => {
  const { nodes, height } = graphLayout(3, { row: 30 });
  assert.deepEqual(nodes.map((n) => n.qubit), [0, 1, 2]);
  assert.deepEqual(nodes.map((n) => n.y), [15, 45, 75]);
  assert.equal(height, 90);
});

test('thickness rises with mutual information and never disappears', () => {
  assert.ok(edgeWidth(MAX_MUTUAL_INFORMATION) > edgeWidth(1));
  assert.ok(edgeWidth(1) > edgeWidth(0.1));
  assert.ok(edgeWidth(0.001) >= 1, 'a whisper of correlation is still a hairline');
  assert.equal(edgeWidth(99), edgeWidth(MAX_MUTUAL_INFORMATION), 'the scale is clamped');
});

test('a classically correlated pair and a Bell pair are different colours', () => {
  assert.notEqual(concurrenceColor(0), concurrenceColor(1));
  assert.equal(concurrenceColor(-1), concurrenceColor(0), 'concurrence is clamped');
});

test('pairs are ordered, deduplicated by direction and filtered by weight', () => {
  const edges = visibleEdges([
    { qubits: [2, 0], mutualInformation: 0.4, concurrence: 0.1 },
    { qubits: [0, 1], mutualInformation: 2, concurrence: 1 },
    { qubits: [1, 2], mutualInformation: 0, concurrence: 0 },
    { qubits: [3, 3], mutualInformation: 1, concurrence: 0 },
  ]);
  assert.deepEqual(edges.map((e) => `${e.a}${e.b}`), ['01', '02'], 'strongest first, no self-pair, no empty pair');
});

test('the snake_case field of the wire is read as well as the camelCase one', () => {
  const [edge] = visibleEdges([{ qubits: [0, 1], mutual_information: 1.5, concurrence: 0.3 }]);
  assert.equal(edge.mutualInformation, 1.5);
});

test('the element draws a wire per qubit, an arc per pair and names them', () => {
  const el = new TfEntanglementGraph();
  document.body.appendChild(el);
  el.numQubits = 3;
  el.pairs = [{ qubits: [0, 1], mutualInformation: 2, concurrence: 1 }];
  assert.equal(el.querySelectorAll('.tf-entgraph__wire').length, 3);
  assert.equal(el.querySelectorAll('.tf-entgraph__edge').length, 1);
  assert.match(el.querySelector('svg').getAttribute('aria-label'), /q0–q1/);
  el.remove();
});

test('a register with no correlated pair says so instead of drawing nothing', () => {
  const el = new TfEntanglementGraph();
  document.body.appendChild(el);
  el.labels = { empty: 'nothing correlated' };
  el.numQubits = 2;
  el.pairs = [{ qubits: [0, 1], mutualInformation: 0, concurrence: 0 }];
  assert.equal(el.querySelectorAll('.tf-entgraph__edge').length, 0);
  assert.match(el.textContent, /nothing correlated/);
  el.remove();
});
