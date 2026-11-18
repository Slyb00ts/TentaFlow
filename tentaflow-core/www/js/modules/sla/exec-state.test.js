import '../tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { startSlaState } = await import('./exec-state.js');

test('a second poll patches the node row in place: cell nodes survive, values change', async () => {
  let inFlight = 1;
  const call = async (kind) => {
    if (kind === 'slaPolicyGetRequest') return { policy: { id: 'p1', spec: { name: 'Std', maxConcurrency: 4, maxQueue: 0, callWindowSecs: 60, ordering: 'partition' } } };
    if (kind === 'slaStatusRequest') return { statuses: [{ flowId: 'f1', state: 'ok', inFlight, maxConcurrency: 4, openBreaches: [] }] };
    if (kind === 'slaBreachListRequest') return { breaches: [] };
    return {};
  };
  const host = document.createElement('div');
  const card = startSlaState({
    host, flow: { id: 'f1', slaPolicyId: 'p1' }, intervalMs: 3_600_000,
    deps: { call, nodes: async () => [{ nodeId: 'n1', hostname: 'alpha', isLocal: true }], now: () => 1_000_000 },
  });
  await card.ready;
  const row = host.querySelector('tbody tr');
  assert.ok(row, 'the node row is drawn');
  const cell = row.querySelector('td');
  const before = host.textContent;
  inFlight = 3;
  await card.refresh();
  assert.equal(host.querySelector('tbody tr'), row, 'the row node survives a poll');
  assert.equal(row.querySelector('td'), cell);
  assert.notEqual(host.textContent, before, 'the values changed');
  assert.match(host.textContent, /3/);
  card.dispose();
});

const SPEC = { name: 'Std', maxConcurrency: 4, maxQueue: 0, callWindowSecs: 60, ordering: 'partition' };
const NODES = [{ nodeId: 'n1', hostname: 'alpha', isLocal: true }, { nodeId: 'n2', hostname: 'beta', isLocal: false }];

function fakeCall({ statuses = {}, consumers = {}, breaches = [], total = breaches.length, onBreach = () => {} } = {}) {
  return async (kind, payload, options) => {
    const node = options?.targetNodeId ?? 'n1';
    if (kind === 'slaPolicyGetRequest') return { policy: { id: 'p1', spec: SPEC } };
    if (kind === 'slaStatusRequest') {
      const answer = statuses[node];
      if (answer instanceof Promise) return answer;
      if (answer === undefined) return { statuses: [] };
      return { statuses: [answer] };
    }
    if (kind === 'flowListRequest') return { flows: [{ id: 'f1', busConsumer: consumers[node] ?? null }] };
    if (kind === 'slaBreachListRequest') { onBreach(payload); return { breaches: breaches.slice(0, payload.limit), total }; }
    return {};
  };
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 5));

test('one slow node is shown as unreachable after its deadline, and the others are painted as they answer', async () => {
  const slow = new Promise(() => {});
  const host = document.createElement('div');
  const card = startSlaState({
    host, flow: { id: 'f1', slaPolicyId: 'p1' }, intervalMs: 3_600_000, nodeDeadlineMs: 40,
    deps: {
      call: fakeCall({ statuses: { n1: { state: 'ok', inFlight: 2, maxConcurrency: 4 }, n2: slow } }),
      nodes: async () => NODES, now: () => 1_000_000,
    },
  });
  await settle();
  const alpha = host.querySelector('tr[data-node="n1"]');
  const beta = host.querySelector('tr[data-node="n2"]');
  assert.match(alpha.textContent, /2\/4/, 'the answering node is painted before the slow one gives up');
  assert.equal(beta.querySelector('[data-v="chip"]').getAttribute('label') ?? '', '', 'the slow node is still waiting');
  await card.ready;
  assert.equal(beta.querySelector('[data-v="chip"]').getAttribute('label'), 'brak odpowiedzi');
  assert.equal(host.querySelector('tr[data-node="n1"]'), alpha);
  card.dispose();
});

test('a bus flow throttled by its policy says "wstrzymany limitem" in the header chip and shows the leader', async () => {
  const host = document.createElement('div');
  const card = startSlaState({
    host, flow: { id: 'f1', slaPolicyId: 'p1', busConsumer: { state: 'running' } }, intervalMs: 3_600_000,
    deps: {
      call: fakeCall({
        statuses: { n1: { state: 'ok', inFlight: 3, maxConcurrency: 4 }, n2: { state: 'ok', inFlight: 0, maxConcurrency: 4 } },
        consumers: { n1: { state: 'standby' }, n2: { state: 'running', throttledUntil: 2_000_000 } },
      }),
      nodes: async () => NODES, now: () => 1_000_000,
    },
  });
  await card.ready;
  const header = host.querySelector('[data-v="overall"]');
  assert.equal(header.hidden, false);
  assert.equal(header.getAttribute('label'), 'wstrzymany limitem');
  assert.equal(header.getAttribute('status'), 'warn');
  assert.equal(host.querySelectorAll('tr[data-node]').length, 1);
  assert.match(host.querySelector('tr[data-node="n2"]').textContent, /prowadzi odbiór/);
  assert.match(host.textContent, /Wstrzymania limitem/);
  card.dispose();
});

test('the open count comes from every node and the history pages by the total', async () => {
  const breaches = Array.from({ length: 45 }, (_, i) => ({ id: `b${i}`, nodeId: 'n1', kind: 'latency', peakValue: 900, threshold: 800, raisedAtMs: 1_000_000 - i, resolvedAtMs: 1_000_500 }));
  const asked = [];
  const host = document.createElement('div');
  const open = { kind: 'latency' };
  const card = startSlaState({
    host, flow: { id: 'f1', slaPolicyId: 'p1' }, intervalMs: 3_600_000,
    deps: {
      call: fakeCall({ statuses: { n1: { state: 'breach', openBreaches: [open, open] }, n2: { state: 'breach', openBreaches: [open] } }, breaches, total: 45, onBreach: (p) => asked.push(p.limit) }),
      nodes: async () => NODES, now: () => 1_000_000,
    },
  });
  await card.ready;
  assert.equal(host.querySelector('[data-v="open-count"]').getAttribute('label'), '3 otwarte', 'counted from the nodes, not from the 20 rows');
  assert.equal(host.querySelectorAll('[data-v="breaches"] tr').length, 20);
  assert.equal(host.querySelector('[data-v="breach-more-row"]').hidden, false);
  assert.match(host.querySelector('[data-v="breach-shown"]').textContent, /20 z 45/);
  host.querySelector('[data-v="breach-more"]').click();
  await settle();
  assert.deepEqual(asked.slice(-1), [40]);
  assert.equal(host.querySelectorAll('[data-v="breaches"] tr').length, 40);
  host.querySelector('[data-v="breach-more"]').click();
  await settle();
  assert.equal(host.querySelectorAll('[data-v="breaches"] tr').length, 45);
  assert.equal(host.querySelector('[data-v="breach-more-row"]').hidden, true);
  card.dispose();
});

test('dispose stops the polling: no timer fires and no request leaves afterwards', async () => {
  let calls = 0;
  const base = fakeCall({ statuses: { n1: { state: 'ok' } } });
  const host = document.createElement('div');
  const card = startSlaState({
    host, flow: { id: 'f1', slaPolicyId: 'p1' }, intervalMs: 10,
    deps: { call: (...args) => { calls += 1; return base(...args); }, nodes: async () => [NODES[0]], now: () => 1 },
  });
  await card.ready;
  await new Promise((resolve) => setTimeout(resolve, 35));
  assert.ok(calls > 4, 'it polled while alive');
  card.dispose();
  const after = calls;
  await new Promise((resolve) => setTimeout(resolve, 60));
  assert.equal(calls, after);
  assert.equal(host.childNodes.length, 0);
});

test('refusals "dziś": the window count with today beside it, painted in place on the next poll', async () => {
  let today = 41;
  const call = async (kind) => {
    if (kind === 'slaPolicyGetRequest') return { policy: { id: 'p1', spec: SPEC } };
    if (kind === 'slaStatusRequest') {
      return { statuses: [{ state: 'ok', inFlight: 1, maxConcurrency: 4, evaluation: { rejectedCalls: 2, rejectedConcurrency: 1 }, rejectedCallsToday: today - 5, rejectedConcurrencyToday: 5 }] };
    }
    if (kind === 'slaBreachListRequest') return { breaches: [], total: 0 };
    return {};
  };
  const host = document.createElement('div');
  const card = startSlaState({ host, flow: { id: 'f1', slaPolicyId: 'p1' }, intervalMs: 3_600_000, deps: { call, nodes: async () => [NODES[0]], now: () => 1 } });
  await card.ready;
  const main = host.querySelector('[data-v="refused"]');
  const sub = host.querySelector('[data-v="refused-sub"]');
  assert.equal(main.textContent, '3');
  assert.equal(sub.textContent, 'w tym oknie · dziś 41');
  today = 44;
  await card.refresh();
  assert.equal(host.querySelector('[data-v="refused"]'), main, 'same node, repainted in place');
  assert.equal(sub.textContent, 'w tym oknie · dziś 44');
  card.dispose();
});

test('a bus flow counts today in its "Wstrzymania limitem" column', async () => {
  const call = fakeCall({
    statuses: { n1: { state: 'ok', inFlight: 3, maxConcurrency: 4, rejectedCallsToday: 10, rejectedConcurrencyToday: 4 } },
    consumers: { n1: { state: 'running' } },
  });
  const host = document.createElement('div');
  const card = startSlaState({ host, flow: { id: 'f1', slaPolicyId: 'p1', busConsumer: { state: 'running' } }, intervalMs: 3_600_000, deps: { call, nodes: async () => [NODES[0]], now: () => 1 } });
  await card.ready;
  assert.equal(host.querySelector('[data-v="refused"]').textContent, '14');
  assert.equal(host.querySelector('[data-v="refused-sub"]').textContent, 'dziś');
  card.dispose();
});
