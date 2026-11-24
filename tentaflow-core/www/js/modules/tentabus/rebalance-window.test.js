// ===== File: modules/tentabus/rebalance-window.test.js — the window "Rozłóż prowadzenie": preview, blocks, progress, partial result, plan_changed =====
//
// Runs the real window in happy-dom against fake plan/apply calls: the rows of
// the preview, the topics that stay and why, the progress that follows the
// replica lists of a poll and keeps its table element, a partly failed result
// in readable words, and a plan that changed under the window being fetched
// again instead of being applied.

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { openRebalanceWindow, afterBalance, afterSentence, blockerText, errorCodeOf, isTransportFailure, moveErrorText } = await import('./rebalance-window.js');
const { leadershipUnits } = await import('./leader-balance.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const plain = (html) => norm(String(html).replace(/<[^>]+>/g, ' '));
const tick = () => new Promise((r) => setTimeout(r, 0));
const MIB = 1024 ** 2;

const A = { nodeId: 'a', label: 'Serwer A', reachable: true, isLocal: true, forwardCapable: true };
const B = { nodeId: 'b', label: 'Serwer B', reachable: true, forwardCapable: true };

function led(topic, count, leader, preferred, lagging = []) {
  return {
    topic,
    partitions: Array.from({ length: count }, (_, partition) => ({
      partition,
      leaderNodeId: leader,
      replicas: preferred === 'a' ? ['a', 'b'] : ['b', 'a'],
      preferredLeaderNodeId: preferred,
      lagging: lagging.filter((l) => l.partition === partition).map((l) => ({ nodeId: l.nodeId, lagBytes: l.lagBytes })),
    })),
  };
}
const startTopics = () => [led('wyniki', 6, 'a', 'b', [{ partition: 2, nodeId: 'b', lagBytes: 12 * MIB }]), led('przyjecia', 4, 'a', 'b'), led('faktury', 3, 'a', 'b'), led('odczyty', 12, 'a', 'a'), led('rejestracja', 3, 'a', 'b')];

const moves = [
  { topic: 'przyjecia', partitions: [0, 1, 2, 3], fromNodeId: 'a', toNodeId: 'b', returnsToPreferred: true },
  { topic: 'wyniki', partitions: [0, 1, 2, 3, 4, 5], fromNodeId: 'a', toNodeId: 'b', returnsToPreferred: true },
  { topic: 'faktury', partitions: [0, 1, 2], fromNodeId: 'a', toNodeId: 'b', returnsToPreferred: false },
];
const plan = { moves, perNode: [], blockers: [{ topic: 'rejestracja', reason: 'consumer_group_paused' }] };

function setup({ loadPlan, apply } = {}) {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const view = { nodes: [A, B], replicaTopics: startTopics(), topics: startTopics().map((t) => ({ name: t.topic })), leaderReturn: true, pausedConsumers: [{ topic: 'rejestracja', label: 'rejestracja-online' }] };
  const done = [];
  const handle = openRebalanceWindow({
    instanceLabel: 'Produkcja',
    loadPlan: loadPlan || (async () => plan),
    apply: apply || (async () => []),
    observe: () => view,
    describeError: (e) => `opis: ${e.message}`,
    onDone: (r) => done.push(r),
  });
  const q = (role) => handle.win.querySelector(`[data-role="${role}"]`);
  return { ...handle, view, done, q, act: (n) => handle.win.querySelector(`[data-act="${n}"]`) };
}

test('the preview lists each move with why, the topic that stays with its reason, and what changes', async () => {
  const w = setup();
  await tick();
  assert.match(w.win._titleEl.textContent, /Rozłóż prowadzenie — instancja Produkcja/);
  const rows = w.q('moves').rows;
  assert.equal(rows.length, 3);
  assert.equal(plain(rows[0].topic), 'przyjecia');
  assert.equal(rows[0].parts, '4');
  assert.match(plain(rows[0].route), /Serwer A.*Serwer B/);
  assert.match(plain(rows[0].detail), /powrót do preferowanego nodu/i);
  assert.match(plain(rows[1].detail), /partycji 2.*Serwer B.*2 s/, 'a copy still catching up is announced');
  const blockers = w.q('blockers').rows;
  assert.equal(blockers.length, 1);
  assert.equal(plain(blockers[0].topic).startsWith('rejestracja'), true);
  assert.match(plain(blockers[0].reason), /^Odbiorca rejestracja-online jest wstrzymany\. Wstrzymanie obowiązuje tylko na nodzie prowadzącym/, 'the paused consumer is named');
  assert.equal(w.q('impact').hidden, false);
  assert.match(plain(w.q('impact').innerHTML), /Co się stanie po rozłożeniu.*Serwer A będzie prowadził.*Serwer B/);
  assert.equal(w.act('apply').hasAttribute('disabled'), false);
  assert.equal(w.act('close').hidden, true);
});

test('nothing to move: the window says so and offers no apply', async () => {
  const w = setup({ loadPlan: async () => ({ moves: [], perNode: [], blockers: [] }) });
  await tick();
  assert.equal(w.q('moves-field').hidden, true);
  assert.equal(w.act('apply').hasAttribute('disabled'), true);
  assert.equal(w.q('top').querySelector('tf-alert').getAttribute('tone'), 'info');
});

test('a plan that cannot be loaded is shown as an error with a way out', async () => {
  const w = setup({ loadPlan: async () => { throw new Error('bus down'); } });
  await tick();
  assert.equal(w.q('error').hidden, false);
  assert.match(w.q('error').textContent, /opis: bus down/);
  assert.equal(w.act('close').hidden, false);
  assert.equal(w.act('apply').hasAttribute('disabled'), true);
});

test('progress follows the polled replica lists in the same rows, with Cancel and Apply disabled', async () => {
  let release;
  const w = setup({ apply: () => new Promise((r) => { release = r; }) });
  await tick();
  w.act('apply').click();
  await tick();
  assert.equal(w.act('apply').hasAttribute('disabled'), true);
  assert.equal(w.act('cancel').hasAttribute('disabled'), true);
  const table = w.q('moves');
  const rows = () => table.rows.map((r) => plain(r.detail));
  assert.match(rows()[0], /przenosz/i, 'the first unfinished move is under way');
  assert.match(rows()[1], /w kolejce/i);

  // A poll: przyjecia now led by B.
  w.view.replicaTopics = [...w.view.replicaTopics.filter((t) => t.topic !== 'przyjecia'), led('przyjecia', 4, 'b', 'b')];
  const before = table.rows;
  w.refresh();
  assert.equal(w.q('moves'), table, 'the table element survives');
  assert.notEqual(table.rows, before, 'the changed rows are handed over');
  assert.match(rows()[0], /przeniesiono/i);
  assert.match(rows()[1], /czeka na kopi|przenosz/i);

  const unchanged = table.rows;
  w.refresh();
  assert.equal(table.rows, unchanged, 'a poll that changed nothing does not rewrite the rows');

  release(moves.map((m) => ({ ...m, status: 'moved' })));
  await tick();
  assert.equal(w.act('close').hidden, false);
  assert.equal(w.q('moves'), table);
  assert.deepEqual(w.done.map((d) => [d.moved, d.total]), [[3, 3]]);
  assert.equal(w.q('top').querySelector('tf-alert').getAttribute('tone'), 'success');
});

test('a partial result says what moved, what stayed and why, in readable words', async () => {
  const results = [
    { ...moves[0], status: 'moved' },
    { ...moves[1], status: 'failed_rolled_back', errorCode: 'bus.transfer_target_behind' },
    { ...moves[2], status: 'not_attempted' },
  ];
  const w = setup({ apply: async () => results });
  await tick();
  w.act('apply').click();
  await tick();
  await tick();
  const alert = w.q('top').querySelector('tf-alert');
  assert.equal(alert.getAttribute('tone'), 'danger');
  assert.match(alert.getAttribute('title'), /1 z 3/);
  assert.match(norm(alert.getAttribute('message')), /wyniki i faktury prowadzi dalej Serwer A.*gdy kopia na Serwer B dogoni/);
  const rows = w.q('moves').rows.map((r) => plain(r.detail));
  assert.equal(rows[0], 'przeniesiono 4 z 4', 'the partitions that moved, of all the topic has');
  assert.match(rows[1], /nie przeniesiono.*nie dogoni/i);
  assert.match(rows[2], /nie przenoszono.*przy wyniki/i);
  assert.doesNotMatch(rows.join(' '), /bus\./, 'no raw code reaches the reader');
  assert.deepEqual(w.done.map((d) => d.moved), [1]);
});

test('bus.plan_changed fetches the plan again instead of applying the old one', async () => {
  let loads = 0;
  const fresh = { moves: [moves[0]], perNode: [], blockers: [] };
  const w = setup({
    loadPlan: async () => (++loads === 1 ? plan : fresh),
    apply: async () => { throw new Error('bus.plan_changed: the plan changed'); },
  });
  await tick();
  w.act('apply').click();
  await tick();
  await tick();
  assert.equal(loads, 2);
  assert.equal(w.q('moves').rows.length, 1);
  assert.equal(w.q('top').querySelector('tf-alert').getAttribute('tone'), 'warning');
  assert.equal(w.act('apply').hasAttribute('disabled'), false, 'the new plan can be applied');
  assert.deepEqual(w.done, []);
});

test('a busy rebalance returns to the preview with the error and nothing marked as started', async () => {
  const w = setup({ apply: async () => { throw new Error('bus.rebalance_busy'); } });
  await tick();
  w.act('apply').click();
  await tick();
  await tick();
  assert.equal(w.q('error').hidden, false);
  assert.equal(w.act('apply').hasAttribute('disabled'), false);
  assert.equal(w.act('close').hidden, true);
});

test('a failure the server answered with a code ends the window as failed, with the reason', async () => {
  const w = setup({ apply: async () => { throw new Error('bus.transfer_outcome_unknown: not known'); } });
  await tick();
  w.act('apply').click();
  await tick();
  await tick();
  assert.equal(w.q('top').querySelector('tf-alert').getAttribute('tone'), 'danger');
  assert.match(w.q('top').textContent, /opis: bus\.transfer_outcome_unknown/);
  assert.equal(w.act('close').hidden, false);
});

test('a server that refused before starting (replication off, internal error) leaves the window on its preview', async () => {
  for (const code of ['bus.replication_disabled', 'bus.replication_internal_error']) {
    const w = setup({ apply: async () => { throw new Error(`${code}: no`); } });
    await tick();
    w.act('apply').click();
    await tick();
    await tick();
    assert.equal(w.q('error').hidden, false, code);
    assert.equal(w.act('apply').hasAttribute('disabled'), false, `${code}: the plan can be applied again`);
    assert.equal(w.act('close').hidden, true);
  }
});

test('no answer in time is "result unknown": the rows follow the poll and nothing is called failed', async () => {
  const w = setup({ apply: async () => { throw new Error('request busLeaderBalanceApplyRequest timed out after 120000ms'); } });
  await tick();
  w.act('apply').click();
  await tick();
  await tick();
  const table = w.q('moves');
  const alert = () => w.q('top').querySelector('tf-alert');
  assert.equal(alert().getAttribute('tone'), 'warning');
  assert.match(alert().getAttribute('title'), /Nie wiadomo jeszcze/);
  const rows = () => table.rows.map((r) => plain(r.detail));
  assert.deepEqual(rows(), ['sprawdzam', 'sprawdzam', 'sprawdzam'], 'no row claims "nie przenoszono"');
  assert.doesNotMatch(w.win.textContent, /nie przenoszono|nie przeniesiono/);
  assert.deepEqual(w.done.map((d) => d.unknown), [true], 'the list is told to refresh');
  assert.equal(w.act('close').hidden, false, 'the window can be closed');

  // The node kept moving: the next polls show przyjecia moved, then everything.
  w.view.replicaTopics = [...w.view.replicaTopics.filter((t) => t.topic !== 'przyjecia'), led('przyjecia', 4, 'b', 'b')];
  w.refresh();
  assert.equal(w.q('moves'), table, 'the same table');
  assert.deepEqual(rows(), ['przeniesiono 4 z 4', 'sprawdzam', 'sprawdzam']);
  w.view.replicaTopics = [led('przyjecia', 4, 'b', 'b'), led('wyniki', 6, 'b', 'b'), led('faktury', 3, 'b', 'b'), led('rejestracja', 3, 'a', 'b')];
  w.refresh();
  assert.deepEqual(rows(), ['przeniesiono 4 z 4', 'przeniesiono 6 z 6', 'przeniesiono 3 z 3']);
  assert.equal(alert().getAttribute('tone'), 'success', 'what the poll shows is known to have happened');
});

test('what counts as a transport failure: no code from the server and no `bus.` code in the text', () => {
  assert.equal(isTransportFailure(new Error('request x timed out after 30000ms')), true);
  assert.equal(isTransportFailure(new Error('offline: reconnect in progress')), true);
  assert.equal(isTransportFailure(Object.assign(new Error('boom'), { code: 'internal' })), false);
  assert.equal(isTransportFailure(new Error('bus.transfer_refused: x')), false);
  assert.equal(isTransportFailure(new Error('bus.replication_internal_error: x')), false);
});

test('a copy that did not catch up names the partition and how far it trails; the partial text follows the switch', async () => {
  const results = [
    { ...moves[0], status: 'moved' },
    { ...moves[1], status: 'failed_rolled_back', errorCode: 'bus.transfer_target_behind', detail: "'b' has not caught up with the leader of 'wyniki'/2" },
    { ...moves[2], status: 'failed_partial', errorCode: 'bus.transfer_outcome_unknown', detail: 'x' },
  ];
  const w = setup({ apply: async () => results });
  w.view.replicaTopics = [...w.view.replicaTopics.filter((t) => t.topic !== 'wyniki'), led('wyniki', 6, 'a', 'b', [{ partition: 2, nodeId: 'b', lagBytes: 2.8 * MIB }])];
  await tick();
  w.act('apply').click();
  await tick();
  await tick();
  let rows = w.q('moves').rows.map((r) => plain(r.detail));
  assert.match(rows[1], /nie przeniesiono Kopia partycji 2 na Serwer B nie dogoniła w 2 s \(w tyle o 2,8 MB\)\. Żadna partycja tego topiku nie zmieniła nodu\./);
  assert.match(rows[2], /instancja zbierze topik na jednym nodzie sama/, 'with the automatic return on a gather is promised');
});

test('with the automatic return off a partly moved topic is not promised a gather', async () => {
  const results = [{ ...moves[0], status: 'moved' }, { ...moves[1], status: 'moved' }, { ...moves[2], status: 'failed_partial', errorCode: 'bus.transfer_outcome_unknown', detail: 'x' }];
  const w = setup({ apply: async () => results });
  w.view.leaderReturn = false;
  await tick();
  w.act('apply').click();
  await tick();
  await tick();
  const row = plain(w.q('moves').rows[2].detail);
  assert.doesNotMatch(row, /sama/);
  assert.match(row, /rozkładając prowadzenie ponownie/);
});

test('the preview\'s "after" sentence counts the way the table does, whatever totals the plan carries', async () => {
  // The plan's per-node totals once counted the broker\'s own `__bus.metrics` (A 9 where the table said 8): they are not used.
  const perNode = [{ nodeId: 'a', partitionsBefore: 40, partitionsAfter: 23 }, { nodeId: 'b', partitionsBefore: 0, partitionsAfter: 17 }];
  const w = setup({ loadPlan: async () => ({ ...plan, perNode }) });
  w.view.replicaTopics = [...w.view.replicaTopics, led('__bus.metrics', 1, 'a', 'a')];
  w.view.topics = [...w.view.topics, { name: '__bus.metrics' }];
  await tick();
  assert.match(plain(w.q('impact').innerHTML), /Serwer A będzie prowadził 15 partycji \(2 topiki\), a Serwer B 13 partycji \(3 topiki\)\./);
});

test('the after-state counts a topic\'s __dlq partitions with it and moves them along', () => {
  const withDlq = startTopics().map((t) => (t.topic === 'przyjecia' ? { ...t, dlqPartitions: led('__dlq.przyjecia', 4, 'a', 'b').partitions } : t));
  const units = leadershipUnits({ topics: withDlq.map((t) => ({ name: t.topic })), perTopic: withDlq });
  const rows = (after) => after.map((n) => [n.nodeId, n.partitions, n.topics]);
  // przyjecia weighs 4 + 4, wyniki 6, faktury 3 (moved to B); odczyty 12 and rejestracja 3 stay on A.
  assert.deepEqual(rows(afterBalance({ moves, units, nodes: [A, B] })), [['a', 15, 2], ['b', 17, 3]]);
  assert.deepEqual(rows(afterBalance({ moves: [], units, nodes: [A, B] })), [['a', 32, 5], ['b', 0, 0]]);
});

test('the pure helpers: after-state of the nodes, the sentence, blocker words and error codes', () => {
  const units = leadershipUnits({ topics: startTopics().map((t) => ({ name: t.topic })), perTopic: startTopics() });
  const after = afterBalance({ moves, units, nodes: [A, B] });
  // przyjecia, wyniki and faktury (4 + 6 + 3 partitions) go to B; odczyty (12) and rejestracja (3) stay on A.
  assert.deepEqual(after.map((n) => [n.nodeId, n.topics]), [['a', 2], ['b', 3]]);
  assert.deepEqual(afterBalance({ moves, units, nodes: [A, B] }).map((n) => [n.nodeId, n.partitions, n.topics]), [['a', 15, 2], ['b', 13, 3]]);
  assert.equal(afterSentence([{ label: 'Serwer A', partitions: 15, topics: 2 }, { label: 'Serwer B', partitions: 13, topics: 3 }]), 'Serwer A będzie prowadził 15 partycji (2 topiki), a Serwer B 13 partycji (3 topiki).');
  const label = (id) => ({ a: 'Serwer A', b: 'Serwer B' }[id]);
  assert.match(blockerText({ reason: 'target_behind', nodeId: 'b' }, label), /Serwer B/);
  assert.equal(blockerText({ reason: 'something-new' }, label), blockerText({ reason: 'x' }, label));
  assert.equal(errorCodeOf('bus.plan_changed: x'), 'plan_changed');
  assert.equal(errorCodeOf('timeout'), null);
});

test('the progress says what is going on from its first moment, and the result says how many partitions moved', async () => {
  let finish;
  const w = setup({ apply: () => new Promise((resolve) => { finish = resolve; }) });
  await tick();
  w.act('apply').click();
  const alert = w.q('top').querySelector('tf-alert');
  assert.equal(alert.getAttribute('title'), 'Rozkładam prowadzenie…', 'the alert area is never blank while moves run');
  assert.match(alert.getAttribute('message'), /^Przeniesiono 0 z 3 topików\./);
  assert.deepEqual(w.q('moves').rows.map((r) => plain(r.detail)), ['przenoszę', 'w kolejce', 'w kolejce']);
  finish([
    { topic: 'przyjecia', status: 'moved', fromNodeId: 'a', toNodeId: 'b' },
    { topic: 'wyniki', status: 'moved', fromNodeId: 'a', toNodeId: 'b' },
    { topic: 'faktury', status: 'moved', fromNodeId: 'a', toNodeId: 'b' },
  ]);
  await tick();
  await tick();
  assert.deepEqual(w.q('moves').rows.map((r) => plain(r.detail)), ['przeniesiono 4 z 4', 'przeniesiono 6 z 6', 'przeniesiono 3 z 3']);
});

test('a node without a label is named by its short id in the route, with the full id as a tooltip', async () => {
  const hexA = '41291e2758d793a2d740f5db7d7c99141f32f02199c1427803d4373636e7ca57';
  const hexB = '365123c5d3d331c7ddff1a60767636af18b66eb8dba890b57cbeebba600f6a54';
  const w = setup({ loadPlan: async () => ({ ...plan, moves: moves.map((m) => ({ ...m, fromNodeId: hexA, toNodeId: hexB })) }) });
  w.view.nodes = [{ ...A, nodeId: hexA, label: '' }, { ...B, nodeId: hexB, label: '' }];
  w.view.replicaTopics = startTopics().map((t) => ({ ...t, partitions: t.partitions.map((p) => ({ ...p, leaderNodeId: hexA, preferredLeaderNodeId: hexB })) }));
  await tick();
  const route = w.q('moves').rows[0].route;
  assert.equal(plain(route), '41291e27 → 365123c5');
  assert.match(route, new RegExp(`title="${hexA}"`));
  assert.match(route, new RegExp(`title="${hexB}"`));
  assert.doesNotMatch(plain(w.q('impact').innerHTML), new RegExp(hexA.slice(0, 20)));
  assert.match(plain(w.q('impact').innerHTML), /41291e27 będzie prowadził/);
});

test('the result of a split topic says how many partitions moved against all the topic has', async () => {
  const splitMove = { ...moves[0], partitions: [0, 1, 3] };
  const w = setup({ loadPlan: async () => ({ ...plan, moves: [splitMove] }), apply: async () => [{ ...splitMove, status: 'moved' }] });
  await tick();
  w.act('apply').click();
  await tick();
  await tick();
  assert.deepEqual(w.q('moves').rows.map((r) => plain(r.detail)), ['przeniesiono 3 z 4']);
});
