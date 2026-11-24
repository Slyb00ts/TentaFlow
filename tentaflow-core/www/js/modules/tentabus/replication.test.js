// =============================================================================
// File: modules/tentabus/replication.test.js
// Description: The Kopie i nody tab (T10): node cards counted over the
// reader's topics (leads, holds, copies in sync, last signal), the
// partitions that need attention with "Przenieś prowadzenie" only for an
// administrator and only where a node can take over, partitions per topic
// leading to the topic's own section, and leadership changes in words.
// There is no button to change the copies.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { drawReplication, lastSignal, topicLeadRows, readerFailovers, failoverReason, mergeFailovers, collectFailovers } = await import('./replication.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ');
const tick = () => new Promise((r) => setTimeout(r, 20));
const MIB = 1024 ** 2;

const nodes = [
  { nodeId: 'n-rig', label: 'rig26', reachable: true, isLocal: true, lastHeartbeatMsAgo: 0 },
  { nodeId: 'n-main', label: 'mainpc', reachable: true, lastHeartbeatMsAgo: 400 },
  { nodeId: 'n-mac', label: 'mac-studio', reachable: true, lastHeartbeatMsAgo: 1200 },
];
const all3 = ['n-rig', 'n-main', 'n-mac'];
const replicaTopics = [
  { topic: 'odczyty-urzadzen', partitions: [
    { partition: 0, leaderNodeId: 'n-main', replicas: all3, isr: ['n-rig', 'n-main'], lagging: [{ nodeId: 'n-mac', lagBytes: 87 * MIB, lagMs: 3500 }] },
    { partition: 1, leaderNodeId: 'n-rig', replicas: all3, isr: all3, lagging: [] },
  ] },
  { topic: 'wizyty', partitions: [{ partition: 0, leaderNodeId: 'n-mac', replicas: all3, isr: all3, lagging: [] }] },
];
const topics = [
  { name: 'odczyty-urzadzen', partitions: 2 },
  { name: 'wizyty', partitions: 1 },
  { name: '__dlq.wizyty', partitions: 1, isDlq: true },
];
const failovers = [
  { atMs: 1000, topic: 'wizyty', partition: 0, fromNode: 'n-rig', toNode: 'n-mac', durationMs: 1800, reason: 'lease_expired', toEpoch: 3 },
  { atMs: 2000, topic: 'odczyty-urzadzen', partition: 1, fromNode: 'n-mac', toNode: 'n-rig', durationMs: 900, reason: 'manual_transfer', actorLabel: 'Anna Kowalska', toEpoch: 5 },
  { atMs: 3000, topic: '__dlq.wizyty', partition: 0, fromNode: 'n-rig', toNode: 'n-mac', durationMs: 1, reason: 'lease_expired', toEpoch: 2 },
];

function mount({ canAdmin = true, justMoved = new Set(), notice = null, nodesList = nodes } = {}) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  const ctx = {
    view: () => ({ nodes: nodesList, replicaTopics, failovers, topics, canAdmin, notice, justMoved, nowMs: Date.now() }),
    go: (a) => moves.push(a),
  };
  drawReplication(body, ctx);
  return { body, moves };
}

test('the last signal: this node answers now, others by their heartbeat', () => {
  assert.equal(lastSignal(nodes[0]), 'teraz (ten node)');
  assert.equal(norm(lastSignal(nodes[1])), '0,4 s temu');
  assert.match(lastSignal({ ...nodes[1], reachable: false }), /nie odpowiada/);
});

test('partitions per topic and who leads them; leadership changes of the reader\'s topics only, newest first', () => {
  const rows = topicLeadRows({ topics, perTopic: replicaTopics, nodes, attention: [{ topic: 'odczyty-urzadzen' }] });
  assert.deepEqual(rows.map((r) => [r.name, r.partitions, r.attention]), [['odczyty-urzadzen', 2, 1], ['wizyty', 1, 0]]);
  assert.deepEqual(rows[0].leads.map((l) => `${l.label} ${l.n}`), ['rig26 1', 'mainpc 1', 'mac-studio 0']);
  const changes = readerFailovers(failovers, topics);
  assert.deepEqual(changes.map((f) => f.topic), ['odczyty-urzadzen', 'wizyty']);
  assert.equal(failoverReason(changes[0]), 'przeniesione ręcznie przez: Anna Kowalska');
  assert.equal(failoverReason({ ...changes[1], fromNode: 'rig26' }), 'rig26 przestał odpowiadać');
});

test('node cards count leads, copies and copies in sync over the reader\'s topics', () => {
  const { body } = mount();
  const cards = [...body.querySelectorAll('.tb-node-card')];
  assert.deepEqual(cards.map((c) => c.dataset.node), ['n-rig', 'n-main', 'n-mac']);
  const text = (c, role) => norm(c.querySelector(`[data-role="${role}"]`).textContent);
  assert.deepEqual([text(cards[2], 'leads'), text(cards[2], 'holds'), text(cards[2], 'sync')], ['1', '2', '2 z 3']);
  assert.ok(cards[2].querySelector('[data-role="sync"]').classList.contains('is-warn'));
  assert.equal(text(cards[0], 'signal'), 'teraz (ten node)');
});

test('the partition that needs attention, with how far the copy trails and the move for an administrator', async () => {
  const { body, moves } = mount();
  await tick();
  const table = body.querySelector('[data-role="attention"]');
  assert.equal(table.rows.length, 1);
  assert.match(table.rows[0].where, /odczyty-urzadzen/);
  assert.match(norm(table.rows[0].behind), /mac-studio: 87 MB · 4 s/);
  assert.equal(table.rows[0]._blocker, null, 'rig26 holds an in-sync copy');
  assert.equal(typeof table.rowActions, 'function');
  const button = table.rowActions(table.rows[0], 0, () => table.rows[0]);
  button.click();
  assert.deepEqual(moves, [{ kind: 'transfer', topic: 'odczyty-urzadzen', partition: 0 }]);
  assert.equal(body.textContent.includes('Zmień repliki'), false, 'no button changes the copies');
});

test('a reader sees the same state without the move', async () => {
  const { body } = mount({ canAdmin: false });
  await tick();
  assert.equal(body.querySelector('[data-role="attention"]').rowActions, null);
});

test('a moved partition is marked and not offered again until refreshed', async () => {
  const { body } = mount({ justMoved: new Set(['odczyty-urzadzen:0']), notice: { title: 'Przeniesiono prowadzenie.', text: 'Partycję 0 topiku odczyty-urzadzen prowadzi teraz node rig26.' } });
  await tick();
  const table = body.querySelector('[data-role="attention"]');
  assert.match(table.rows[0].where, /zmieniono przed chwilą/);
  assert.match(table.rows[0]._blocker, /przed chwilą/);
  assert.equal(body.querySelector('[data-role="attn-foot"]').hidden, false);
  assert.equal(body.querySelector('tf-alert').getAttribute('title'), 'Przeniesiono prowadzenie.');
});

test('a topic row leads to that topic\'s partitions; a single node says every partition has one copy', () => {
  const { body, moves } = mount({ nodesList: [nodes[0]] });
  body.querySelector('[data-go="topic"][data-topic="wizyty"]').click();
  assert.deepEqual(moves, [{ kind: 'topic', topic: 'wizyty' }]);
  assert.match(body.querySelector('[data-role="nodes-sub"]').textContent, /jeden node/);
  const changes = [...body.querySelectorAll('[data-role="changes"] .job-row')].map((r) => norm(r.querySelector('[data-role="name"]').textContent));
  assert.deepEqual(changes, ['odczyty-urzadzen · partycja 1: prowadzenie przeszło z n-mac na rig26', 'wizyty · partycja 0: prowadzenie przeszło z rig26 na n-mac']);
});

// ---------------------------------------------------------------------------
// "Rozkład prowadzenia" (TB-04): per-node load, the topics away from their
// preferred node, the spread button with its reason, the switch of the
// automatic return and the new reasons of a leadership change.
// ---------------------------------------------------------------------------

const A = { nodeId: 'a', label: 'Serwer A', reachable: true, isLocal: true, lastHeartbeatMsAgo: 0, forwardCapable: true };
const B = { nodeId: 'b', label: 'Serwer B', reachable: true, lastHeartbeatMsAgo: 400, forwardCapable: true };
const pair = [A, B];

/** A topic of `count` partitions led by `leader`, whose preferred node is `preferred`. */
function led(topic, count, leader, preferred, lagging = []) {
  return {
    topic,
    partitions: Array.from({ length: count }, (_, partition) => ({
      partition,
      leaderNodeId: leader,
      replicas: preferred === 'a' ? ['a', 'b'] : ['b', 'a'],
      preferredLeaderNodeId: preferred,
      isr: lagging.length ? [leader] : ['a', 'b'],
      lagging: lagging.map((nodeId) => ({ nodeId, lagBytes: 3 * MIB, lagMs: 1000 })),
    })),
  };
}
const listOf = (...perTopic) => perTopic.map((t) => ({ name: t.topic, partitions: t.partitions.length }));

const evenTopics = [led('odczyty', 12, 'a', 'a'), led('powiadomienia', 3, 'a', 'a'), led('wyniki', 6, 'b', 'b'), led('przyjecia', 4, 'b', 'b'), led('wizyty', 4, 'b', 'b'), led('faktury', 3, 'b', 'b')];
const unevenTopics = [led('wyniki', 6, 'a', 'b', ['b']), led('przyjecia', 4, 'a', 'b'), led('wizyty', 4, 'a', 'b', ['b']), led('faktury', 3, 'a', 'b'), led('odczyty', 12, 'a', 'a'), led('powiadomienia', 3, 'a', 'a')];

function mountBalance({ nodesList = pair, perTopic = evenTopics, canAdmin = true, platformAdmin = canAdmin, leaderReturn = true, notice = null, reachableSince = new Map() } = {}) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const calls = [];
  const state = { nodesList, perTopic, canAdmin, platformAdmin, leaderReturn, notice, reachableSince };
  const ctx = {
    view: () => ({
      nodes: state.nodesList, replicaTopics: state.perTopic, failovers: [], topics: listOf(...state.perTopic), canAdmin: state.canAdmin,
      notice: state.notice, justMoved: new Set(), leaderReturn: state.leaderReturn, platformAdmin: state.platformAdmin, reachableSince: state.reachableSince, nowMs: Date.now(),
    }),
    go: (a) => calls.push(a),
  };
  drawReplication(body, ctx);
  return { body, calls, state, ctx, repaint: () => drawReplication(body, ctx) };
}

const text = (el) => norm(el.textContent);
const plain = (html) => norm(String(html).replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim());

test('an even spread: per node what it leads, the distance from an even split and the topics away from home', async () => {
  const { body } = mountBalance();
  await tick();
  const rows = body.querySelector('[data-role="balance"]').rows;
  assert.equal(rows.length, 2);
  assert.match(plain(rows[0].leads), /^15 partycji/);
  assert.match(plain(rows[0].leads), /2 topiki: odczyty, powiadomienia/);
  assert.match(plain(rows[1].leads), /17 partycji.*4 topiki: faktury, przyjecia, wizyty, wyniki/);
  assert.equal(norm(rows[0].dev), '−1 partycja');
  assert.equal(norm(rows[1].dev), '+1 partycja');
  assert.equal(rows[0].away, '0');
  assert.equal(text(body.querySelector('[data-role="balance-summary"]')), 'Równy podział to 16 partycji na node. Rozkład jest równy: różnica między nodami (2 partycje) jest mniejsza niż najmniejszy topik (3 partycje).');
  assert.equal(body.querySelector('[data-role="balance-alert"]').children.length, 0, 'nothing to warn about');
  assert.equal(body.querySelector('[data-role="balance-card"]').hidden, false);
});

test('an uneven spread names the idle node, the topics waiting for it and the copies still catching up', async () => {
  const { body } = mountBalance({ perTopic: unevenTopics });
  await tick();
  const rows = body.querySelector('[data-role="balance"]').rows;
  assert.match(plain(rows[0].leads), /^32 partycje/);
  assert.match(plain(rows[1].leads), /^0 partycji.*0 topików/);
  assert.match(rows[0].dev, /tb-warn-text/, 'the distance is flagged');
  assert.match(plain(rows[1].away), /^4 topiki .*faktury, przyjecia, wizyty, wyniki — prowadzi je teraz Serwer A$/);
  const alert = body.querySelector('[data-role="balance-alert"] tf-alert');
  assert.equal(alert.getAttribute('tone'), 'warning');
  assert.equal(alert.getAttribute('title'), 'Serwer B odpowiada, ale nie prowadzi żadnego topiku.', 'without a known time the sentence says no "od ..."');
  assert.equal(norm(alert.getAttribute('message')), '4 topiki wrócą na Serwer B same, gdy ich kopie będą tam zgodne nieprzerwanie przez 5 min; kopie wizyty i wyniki jeszcze doganiają. Możesz też rozłożyć prowadzenie teraz.');
  assert.match(text(body.querySelector('[data-role="balance-summary"]')), /Różnica między nodami \(32 partycje\) jest większa niż najmniejszy topik \(3 partycje\)\.$/);
});

test('the warning says since when the node answers and when its topics can return at the earliest', async () => {
  const reachableSince = new Map([['b', Date.now() - 3 * 60_000 - 5000]]);
  const { body } = mountBalance({ perTopic: unevenTopics, reachableSince });
  await tick();
  const alert = body.querySelector('[data-role="balance-alert"] tf-alert');
  assert.equal(alert.getAttribute('title'), 'Serwer B odpowiada od 3 min, ale nie prowadzi żadnego topiku.');
  assert.equal(norm(alert.getAttribute('message')), '4 topiki wrócą na Serwer B same, gdy ich kopie będą tam zgodne nieprzerwanie przez 5 min — najwcześniej za ok. 2 min; kopie wizyty i wyniki jeszcze doganiają. Możesz też rozłożyć prowadzenie teraz.');
  const past = mountBalance({ perTopic: unevenTopics, reachableSince: new Map([['b', Date.now() - 9 * 60_000]]) });
  await tick();
  assert.doesNotMatch(past.body.querySelector('[data-role="balance-alert"] tf-alert').getAttribute('message'), /najwcześniej/, 'after 5 min there is nothing left to wait for');
});

test('with the automatic return off the warning says the topics stay until the spread', async () => {
  const { body } = mountBalance({ perTopic: unevenTopics, leaderReturn: false });
  await tick();
  const message = body.querySelector('[data-role="balance-alert"] tf-alert').getAttribute('message');
  assert.match(message, /Automatyczny powrót do preferowanego nodu jest wyłączony/);
  assert.doesNotMatch(message, /wrócą/);
  assert.equal(body.querySelector('[data-role="return"]').hasAttribute('checked'), false);
});

test('the spread button is the administrator\'s, and a disabled one says why — an older node included', async () => {
  const ok = mountBalance();
  await tick();
  const button = ok.body.querySelector('[data-role="rebalance"]');
  assert.equal(button.hidden, false);
  assert.equal(button.hasAttribute('disabled'), false);
  button.click();
  assert.deepEqual(ok.calls, [{ kind: 'rebalance' }]);

  const reader = mountBalance({ canAdmin: false });
  await tick();
  assert.equal(reader.body.querySelector('[data-role="rebalance"]').hidden, true);

  // The switch is a platform setting: being the administrator of this bus is not enough.
  const busAdmin = mountBalance({ canAdmin: true, platformAdmin: false });
  await tick();
  const locked = busAdmin.body.querySelector('[data-role="return"]');
  assert.equal(locked.hasAttribute('disabled'), true, 'a bus administrator who is no platform administrator sees the switch, not changes it');
  assert.match(text(busAdmin.body.querySelector('[data-role="return-sub"]')), /Ustawienie zmienia administrator\./);
  assert.equal(busAdmin.body.querySelector('[data-role="rebalance"]').hidden, false, 'the spread is still the bus administrator\'s');
  const platform = mountBalance({ platformAdmin: true });
  await tick();
  assert.equal(platform.body.querySelector('[data-role="return"]').hasAttribute('disabled'), false);

  const old = mountBalance({ nodesList: [A, { ...B, forwarding: 'lacking', forwardCapable: false }], perTopic: unevenTopics });
  await tick();
  const blocked = old.body.querySelector('[data-role="rebalance"]');
  assert.equal(blocked.hasAttribute('disabled'), true);
  assert.equal(blocked.getAttribute('title'), 'Zaktualizuj wszystkie nody, aby rozłożyć prowadzenie. Serwer B ma starszą wersję TentaFlow.');
  blocked.click();
  assert.deepEqual(old.calls, [], 'a disabled button sends nothing');
  const chip = old.body.querySelector('.tb-node-card[data-node="b"] [data-role="version"]');
  assert.equal(chip.hidden, false);
  assert.equal(chip.getAttribute('label'), 'starsza wersja');
  assert.match(chip.getAttribute('title'), /Serwer B ma starszą wersję TentaFlow: nie przekazuje zapisów/);
  assert.equal(old.body.querySelector('.tb-node-card[data-node="a"] [data-role="version"]').hidden, true);
  const info = old.body.querySelector('[data-role="balance-alert"] tf-alert');
  assert.equal(info.getAttribute('tone'), 'info');
  assert.equal(info.getAttribute('title'), 'Rozkład prowadzenia ruszy po aktualizacji Serwer B.');
  assert.doesNotMatch(text(old.body.querySelector('[data-role="balance-summary"]')), /Różnica/, 'no verdict while the spread cannot run');

  // A node that is down is sent as the server sends it: not capable (nothing is known of it), "unknown", no heartbeat.
  for (const sent of [{ forwarding: 'unknown' }, {}]) {
    const down = mountBalance({ nodesList: [A, { ...B, reachable: false, forwardCapable: false, lastHeartbeatMsAgo: 90_000, ...sent }] });
    await tick();
    assert.match(down.body.querySelector('[data-role="rebalance"]').getAttribute('title'), /Żaden inny node nie odpowiada/, 'the reason is that no node answers, not an old version');
    assert.equal(down.body.querySelector('.tb-node-card[data-node="b"] [data-role="version"]').hidden, true, 'a node that is down is not an older version');
    assert.doesNotMatch(down.body.querySelector('[data-role="balance-alert"]').textContent, /po aktualizacji/);
  }
});

test('one node has nothing to spread between, and shows no such card', async () => {
  const { body } = mountBalance({ nodesList: [A] });
  await tick();
  assert.equal(body.querySelector('[data-role="balance-card"]').hidden, true);
});

test('the switch of the automatic return reports what the administrator chose', async () => {
  const { body, calls } = mountBalance();
  await tick();
  const toggle = body.querySelector('[data-role="return"]');
  assert.equal(toggle.hasAttribute('checked'), true);
  toggle.querySelector('.tf-toggle').click();
  assert.deepEqual(calls, [{ kind: 'leader-return', enabled: false }]);
});

test('a poll patches the card in place: the nodes, the table, the button and the switch keep their elements', async () => {
  const { body, state, repaint } = mountBalance({ perTopic: unevenTopics });
  await tick();
  const kept = {
    card: body.querySelector('.tb-node-card[data-node="b"]'),
    table: body.querySelector('[data-role="balance"]'),
    button: body.querySelector('[data-role="rebalance"]'),
    toggle: body.querySelector('[data-role="return"]'),
    alert: body.querySelector('[data-role="balance-alert"] tf-alert'),
    rows: body.querySelector('[data-role="balance"]').rows,
  };
  repaint();
  await tick();
  assert.equal(body.querySelector('.tb-node-card[data-node="b"]'), kept.card);
  assert.equal(body.querySelector('[data-role="balance"]'), kept.table);
  assert.equal(body.querySelector('[data-role="rebalance"]'), kept.button);
  assert.equal(body.querySelector('[data-role="return"]'), kept.toggle);
  assert.equal(body.querySelector('[data-role="balance-alert"] tf-alert'), kept.alert, 'an unchanged warning is not rewritten');
  assert.equal(body.querySelector('[data-role="balance"]').rows, kept.rows, 'an unchanged table is not handed its rows again');

  // The next poll: two topics came home and the other node reports an old version.
  state.perTopic = [...unevenTopics.slice(0, 2).map((t) => led(t.topic, t.partitions.length, 'b', 'b')), ...unevenTopics.slice(2)];
  state.nodesList = [A, { ...B, forwardCapable: false }];
  repaint();
  await tick();
  assert.equal(body.querySelector('.tb-node-card[data-node="b"]'), kept.card, 'the node card is the same element');
  assert.equal(body.querySelector('[data-role="balance"]'), kept.table);
  assert.equal(body.querySelector('[data-role="rebalance"]'), kept.button);
  assert.equal(body.querySelector('[data-role="return"]'), kept.toggle);
  assert.match(plain(body.querySelector('[data-role="balance"]').rows[1].leads), /^10 partycji/);
  assert.equal(body.querySelector('[data-role="rebalance"]').hasAttribute('disabled'), true);
  assert.equal(body.querySelector('.tb-node-card[data-node="b"] [data-role="version"]').hidden, false);
});

test('the leadership changes name the system\'s own reasons', () => {
  const f = (reason, extra = {}) => ({ reason, ...extra });
  assert.equal(failoverReason(f('post_upgrade_spread')), 'automatyczny rozkład po aktualizacji nodów');
  assert.equal(failoverReason(f('preferred_restore')), 'automatyczny powrót do preferowanego nodu');
  assert.equal(failoverReason(f('split_gather')), 'automatyczne zebranie topiku rozdzielonego między nody');
  assert.equal(failoverReason(f('manual_transfer', { actorLabel: 'Administrator' })), 'przeniesione ręcznie przez: Administrator');
  assert.equal(failoverReason(f('manual_transfer')), 'przeniesione ręcznie');
  assert.equal(failoverReason(f('rebalance', { actorLabel: 'Administrator' })), 'rozłożone przez: Administrator');
  assert.equal(failoverReason(f('rollback')), 'cofnięte po nieudanym przeniesieniu topiku');
  assert.equal(failoverReason(f('something-new')), 'przyczyna nieznana');
});

test('the history lists each partition\'s change with its reason, newest first', async () => {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const at = Date.now();
  const history = [
    { atMs: at - 3000, topic: 'wyniki', partition: 0, fromNode: 'b', toNode: 'a', toEpoch: 4, durationMs: 700, reason: 'post_upgrade_spread' },
    { atMs: at - 2000, topic: 'wyniki', partition: 1, fromNode: 'a', toNode: 'b', toEpoch: 5, durationMs: 700, reason: 'preferred_restore' },
    { atMs: at - 1000, topic: 'wyniki', partition: 2, fromNode: 'b', toNode: 'a', toEpoch: 6, durationMs: 700, reason: 'manual_transfer', actorLabel: 'Administrator' },
  ];
  drawReplication(body, {
    view: () => ({ nodes: pair, replicaTopics: evenTopics, failovers: history, topics: listOf(...evenTopics), canAdmin: true, notice: null, justMoved: new Set(), leaderReturn: true, nowMs: at }),
    go() {},
  });
  await tick();
  const rows = [...body.querySelectorAll('[data-change]')];
  assert.deepEqual(rows.map((r) => text(r.querySelector('[data-role="why"]'))), [
    'przeniesione ręcznie przez: Administrator',
    'automatyczny powrót do preferowanego nodu',
    'automatyczny rozkład po aktualizacji nodów',
  ]);
  assert.match(text(rows[0].querySelector('[data-role="name"]')), /wyniki · partycja 2: prowadzenie przeszło z Serwer B na Serwer A/);
});

// ---------------------------------------------------------------------------
// TB-04 K10: node names, the warning that matches the button, a truthful
// "since", the history of every node, "<1 s" and counts that agree.
// ---------------------------------------------------------------------------

const HEX_A = '41291e2758d793a2d740f5db7d7c99141f32f02199c1427803d4373636e7ca57';
const HEX_B = '365123c5d3d331c7ddff1a60767636af18b66eb8dba890b57cbeebba600f6a54';

test('a node without a label is named by its first 8 characters, the full id in a tooltip, everywhere it is named', async () => {
  const unnamed = [{ ...A, nodeId: HEX_A, label: '' }, { ...B, nodeId: HEX_B, label: HEX_B }];
  const hexLed = (topic, count, id) => ({
    topic,
    partitions: Array.from({ length: count }, (_, partition) => ({
      partition, leaderNodeId: id, replicas: [id, id === HEX_A ? HEX_B : HEX_A], preferredLeaderNodeId: id, isr: [HEX_A, HEX_B], lagging: [],
    })),
  });
  const { body } = mountBalance({ nodesList: unnamed, perTopic: [hexLed('wyniki', 6, HEX_B), hexLed('odczyty', 4, HEX_A)] });
  await tick();
  const cards = [...body.querySelectorAll('.tb-node-card .tb-node-name')];
  assert.deepEqual(cards.map((c) => c.textContent), ['41291e27', '365123c5']);
  assert.deepEqual(cards.map((c) => c.getAttribute('title')), [HEX_A, HEX_B]);
  const rows = body.querySelector('[data-role="balance"]').rows;
  assert.match(rows[0].node, /41291e27/);
  assert.match(rows[0].node, new RegExp(`title="${HEX_A}"`));
  // The full id is only ever a tooltip, never visible text.
  const visible = (body.innerHTML + rows.map(({ node, leads, dev, away }) => [node, leads, dev, away].join(' ')).join(' ')).replace(/(?:title|data-node)="[^"]*"/g, '');
  assert.doesNotMatch(visible, /[0-9a-f]{20}/);
  const sub = body.querySelector('[data-role="topics"] [data-topic="wyniki"] [data-role="sub"]');
  assert.match(sub.textContent, /prowadzą: 41291e27 0, 365123c5 6/);
  assert.match(sub.getAttribute('title'), new RegExp(HEX_B));
});

test('a labelled node keeps its label and carries no tooltip', async () => {
  const { body } = mountBalance();
  await tick();
  const name = body.querySelector('.tb-node-card[data-node="a"] .tb-node-name');
  assert.equal(name.textContent, 'Serwer A');
  assert.equal(name.hasAttribute('title'), false);
});

test('the warning does not offer the spread while its button is disabled or hidden', async () => {
  const down = mountBalance({ nodesList: [A, { ...B, reachable: false }], perTopic: unevenTopics });
  await tick();
  assert.equal(down.body.querySelector('[data-role="rebalance"]').hasAttribute('disabled'), true);
  const downAlert = down.body.querySelector('[data-role="balance-alert"] tf-alert');
  assert.ok(downAlert, 'the topics away from home are still said');
  assert.doesNotMatch(downAlert.getAttribute('message'), /Możesz też rozłożyć/);
  const reader = mountBalance({ perTopic: unevenTopics, canAdmin: false });
  await tick();
  assert.doesNotMatch(reader.body.querySelector('[data-role="balance-alert"] tf-alert').getAttribute('message'), /Możesz też rozłożyć/);
  const admin = mountBalance({ perTopic: unevenTopics });
  await tick();
  assert.match(admin.body.querySelector('[data-role="balance-alert"] tf-alert').getAttribute('message'), /Możesz też rozłożyć prowadzenie teraz\.$/);
});

test('"odpowiada od" is rounded down and says "przed chwilą" under a minute', async () => {
  const title = async (agoMs) => {
    const { body } = mountBalance({ perTopic: unevenTopics, reachableSince: new Map([['b', Date.now() - agoMs]]) });
    await tick();
    return body.querySelector('[data-role="balance-alert"] tf-alert').getAttribute('title');
  };
  assert.equal(await title(11_000), 'Serwer B odpowiada dopiero przed chwilą i nie prowadzi jeszcze żadnego topiku.');
  assert.equal(await title(59_000), 'Serwer B odpowiada dopiero przed chwilą i nie prowadzi jeszcze żadnego topiku.');
  assert.equal(await title(61_000), 'Serwer B odpowiada od 1 min, ale nie prowadzi żadnego topiku.');
  assert.equal(await title(119_000), 'Serwer B odpowiada od 1 min, ale nie prowadzi żadnego topiku.', 'rounded down, never up to 2');
});

test('a change that took milliseconds says "<1 s"; a longer one keeps its seconds', async () => {
  const at = Date.now();
  const body = document.createElement('div');
  document.body.appendChild(body);
  const history = [
    { atMs: at - 3000, topic: 'wyniki', partition: 0, fromNode: 'b', toNode: 'a', toEpoch: 4, durationMs: 32, reason: 'manual_transfer' },
    { atMs: at - 2000, topic: 'wyniki', partition: 1, fromNode: 'a', toNode: 'b', toEpoch: 5, durationMs: 1800, reason: 'manual_transfer' },
  ];
  drawReplication(body, { view: () => ({ nodes: pair, replicaTopics: evenTopics, failovers: history, topics: listOf(...evenTopics), canAdmin: true, notice: null, justMoved: new Set(), leaderReturn: true, nowMs: at }), go() {} });
  await tick();
  const took = [...body.querySelectorAll('[data-change] [data-role="took"]')].map((e) => text(e));
  assert.ok(took[0].includes('trwało 1,8 s'), took[0]);
  assert.ok(took[1].includes('trwało <1 s'), took[1]);
});

const change = (topic, partition, toEpoch, atMs, extra = {}) => ({ topic, partition, toEpoch, atMs, fromNode: 'a', toNode: 'b', durationMs: 10, reason: 'manual_transfer', ...extra });

test('the history is every node\'s audit merged, a change both nodes know counted once', () => {
  const fromA = [change('wyniki', 0, 4, 1000), change('wyniki', 1, 4, 1000), change('faktury', 0, 2, 500)];
  const fromB = [change('wyniki', 0, 4, 1000), change('przyjecia', 0, 7, 2000, { fromNode: 'b', toNode: 'a' })];
  const merged = mergeFailovers([fromA, fromB, null, undefined]);
  assert.equal(merged.length, 4);
  assert.deepEqual(merged.map((f) => `${f.topic}/${f.partition}@${f.toEpoch}`).sort(), ['faktury/0@2', 'przyjecia/0@7', 'wyniki/0@4', 'wyniki/1@4']);
  // The same partition and moment under another epoch is another change.
  assert.equal(mergeFailovers([[change('wyniki', 0, 4, 1000)], [change('wyniki', 0, 5, 1000)]]).length, 2);
});

test('asking every node for its changes: a node that does not answer breaks nothing, a node that is down is not asked', async () => {
  const asked = [];
  const own = [change('wyniki', 0, 4, 1000)];
  const nodes3 = [A, B, { nodeId: 'c', label: 'Serwer C', reachable: true }, { nodeId: 'd', label: 'Serwer D', reachable: false }];
  const merged = await collectFailovers({
    own,
    nodes: nodes3,
    read: async (nodeId) => {
      asked.push(nodeId);
      if (nodeId === 'c') throw new Error('timed out');
      return { failovers: [change('przyjecia', 0, 7, 2000), change('wyniki', 0, 4, 1000)] };
    },
  });
  assert.deepEqual(asked.sort(), ['b', 'c'], 'this node and the node that is down are not asked');
  assert.deepEqual(merged.map((f) => f.topic).sort(), ['przyjecia', 'wyniki']);
  // Nobody answers: the node\'s own history stands.
  assert.deepEqual(await collectFailovers({ own, nodes: pair, read: async () => { throw new Error('no'); } }), own);
});

test('the leadership history costs one call per reachable remote node, whatever the number of topics', async () => {
  const calls = [];
  const nodes3 = [A, B, { nodeId: 'c', label: 'Serwer C', reachable: true }, { nodeId: 'd', label: 'Serwer D', reachable: false }];
  await collectFailovers({ own: [], nodes: nodes3, read: async (nodeId) => { calls.push(nodeId); return { failovers: [] }; } });
  assert.deepEqual(calls.sort(), ['b', 'c']);
});
