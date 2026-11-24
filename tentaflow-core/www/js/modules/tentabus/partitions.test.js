// =============================================================================
// File: modules/tentabus/partitions.test.js
// Description: Partitions and their copies (U2 "Partycje i kopie", T10): the
// state of every copy, which node may take a partition's leadership and why
// the others may not, the rows of a topic's table and of the partitions that
// need attention, and the "Przenieś prowadzenie" window (only an in-sync copy
// can be chosen; a refusal stays in the window; success reports the node),
// the preferred node's pin and its legend, and the leader's own verdict on a
// node that is behind when the page is open on a node that does not lead.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  copyStates, transferChoices, topicWideBlocker, transferBlocker, partitionRows, attentionRows, behindText, unavailableText, rangeText, openLeaderTransfer, copyChipHtml, preferredLegendHtml, askLeaders, overlayLeaderRows, nodeNameHtml, needsLeaderVerdict,
} = await import('./partitions.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ');
const tick = () => new Promise((r) => setTimeout(r, 0));
const MIB = 1024 ** 2;

const nodes = [
  { nodeId: 'n-rig', label: 'rig26', reachable: true, isLocal: true },
  { nodeId: 'n-main', label: 'mainpc', reachable: true },
  { nodeId: 'n-mac', label: 'mac-studio', reachable: true },
];
const healthy = { partition: 1, leaderNodeId: 'n-main', replicas: ['n-rig', 'n-main', 'n-mac'], isr: ['n-rig', 'n-main', 'n-mac'], lagging: [] };
const behind = {
  partition: 0,
  leaderNodeId: 'n-main',
  replicas: ['n-rig', 'n-main', 'n-mac'],
  isr: ['n-rig', 'n-main'],
  lagging: [{ nodeId: 'n-mac', lagBytes: 87 * MIB, lagMs: 3400, reason: 'lag_bytes=1 exceeds max_bytes=0' }],
};

test('each copy has a state: leader, in sync, behind (with how far), out of sync', () => {
  assert.deepEqual(copyStates(behind, nodes).map((c) => [c.label, c.state]), [['rig26', 'ok'], ['mainpc', 'leader'], ['mac-studio', 'lag']]);
  assert.equal(copyStates(behind, nodes)[2].lagBytes, 87 * MIB);
  const out = { ...healthy, isr: ['n-main'] };
  assert.deepEqual(copyStates(out, nodes).map((c) => c.state), ['out', 'leader', 'out']);
});

test('only an in-sync copy on a node that answers may take over, and the reason is said otherwise', () => {
  assert.deepEqual(transferChoices(behind, nodes).map((c) => c.state), ['ok', 'leader', 'lag']);
  const down = [nodes[0], nodes[1], { ...nodes[2], reachable: false }];
  assert.deepEqual(transferChoices(healthy, down).map((c) => c.state), ['ok', 'leader', 'down']);
  assert.equal(transferBlocker(behind, nodes), null);
  assert.match(transferBlocker(behind, nodes, true), /przed chwilą/);
  assert.match(transferBlocker({ partition: 0, leaderNodeId: 'n-rig', replicas: ['n-rig'], isr: ['n-rig'], lagging: [] }, nodes), /jedną kopię/);
  assert.match(transferBlocker({ ...behind, isr: ['n-main'], lagging: [] }, nodes), /Żaden inny node/);
  assert.match(transferBlocker({ ...healthy, leaderNodeId: null }, nodes), /nie ma teraz nodu prowadzącego/);
});

test('a topic\'s rows join its partitions (numbers, size) with their copies', () => {
  const rows = partitionRows({
    detailPartitions: [
      { partition: 0, earliestOffset: 100, highWatermark: 2000, sizeBytes: 6 * MIB },
      { partition: 1, earliestOffset: 0, highWatermark: 0, sizeBytes: 0 },
    ],
    replicaPartitions: [healthy, behind],
    nodes,
  });
  assert.equal(rows[0].leader, 'mainpc');
  assert.deepEqual(rows[0].range, { from: 100, to: 1999 });
  assert.equal(norm(rangeText(rows[0].range)), 'od 100 do 1 999');
  assert.equal(rangeText(rows[1].range), 'brak wiadomości');
  assert.equal(rows[0].copies.length, 3);
  assert.equal(rows[1].replica, healthy);
});

test('partitions needing attention across topics, and how far each copy trails', () => {
  const rows = attentionRows([
    { topic: 'odczyty-urzadzen', partitions: [healthy, behind] },
    { topic: 'faktury', partitions: [{ ...healthy, partition: 3, unavailableReason: 'NoIsr' }] },
  ], nodes);
  assert.deepEqual(rows.map((r) => r.key), ['faktury:3', 'odczyty-urzadzen:0']);
  assert.equal(norm(behindText(rows[1].behind)), 'mac-studio: 87 MB · 4 s');
  assert.equal(unavailableText(rows[0].unavailable), 'brak zgodnej kopii');
  assert.equal(unavailableText('no_assignment'), 'bez nodu prowadzącego');
  assert.equal(unavailableText('Mystery'), 'partycja niedostępna');
});

function open(overrides = {}) {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const calls = { sent: [], done: [] };
  const win = openLeaderTransfer({
    topic: 'odczyty-urzadzen',
    partition: 0,
    choices: transferChoices(behind, nodes),
    transfer: async (id) => { calls.sent.push(id); },
    describeError: () => 'Większość nodów nie potwierdziła zmiany.',
    onDone: (r) => calls.done.push(r),
    ...overrides,
  });
  return { win, calls };
}

test('the window offers the in-sync copy, disables the others with the reason, and says what happens', async () => {
  const { win, calls } = open();
  assert.equal(win.getAttribute('modal'), '');
  assert.match(win._titleEl.textContent, /Przenieś prowadzenie — odczyty-urzadzen$/);
  assert.match(win.querySelector('.tb-explain-box').textContent, /z całym topikiem odczyty-urzadzen.*wszystkie jego partycje razem z nieprzetworzonymi wiadomościami/);
  const cards = [...win.querySelectorAll('tf-choice-card')];
  assert.deepEqual(cards.map((c) => [c.getAttribute('heading'), c.hasAttribute('disabled')]), [['rig26', false], ['mainpc', true], ['mac-studio', true]]);
  assert.match(norm(cards[2].getAttribute('description')), /w tyle o 87 MB według nodu prowadzącego mainpc — nie może przejąć/);
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /Topik odczyty-urzadzen razem z nieprzetworzonymi wiadomościami będzie prowadził node rig26\..*najwyżej 5 s.*jeszcze raz/);
  assert.match(win.textContent, /dzienniku audytu/);
  win.querySelector('[data-act="move"]').click();
  await tick();
  assert.deepEqual(calls.sent, ['n-rig']);
  assert.deepEqual(calls.done, [{ nodeId: 'n-rig', label: 'rig26' }]);
});

test('a refused move stays in the window with the reason', async () => {
  const { win, calls } = open({ transfer: async () => { throw new Error('bus.replication'); } });
  win.querySelector('[data-act="move"]').click();
  await tick();
  await tick();
  assert.equal(calls.done.length, 0);
  assert.equal(win.isConnected, true);
  assert.match(win.querySelector('[data-role="error"]').textContent, /nie potwierdziła/);
});

test('with no node to choose the move cannot be sent', () => {
  const { win } = open({ choices: transferChoices({ ...behind, isr: ['n-main'], lagging: [] }, nodes) });
  assert.ok(win.querySelector('[data-act="move"]').hasAttribute('disabled'));
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Wybierz node ze zgodną kopią/);
});

test('the preferred node\'s copy carries a pin, and only where there is a choice of nodes', () => {
  const row = { ...healthy, preferredLeaderNodeId: 'n-mac' };
  assert.deepEqual(copyStates(row, nodes).map((c) => c.preferred), [false, false, true]);
  assert.deepEqual(copyStates({ ...row, replicas: ['n-mac'], isr: ['n-mac'] }, nodes).map((c) => c.preferred), [false], 'one copy: nothing to prefer');
  assert.deepEqual(copyStates(healthy, nodes).map((c) => c.preferred), [false, false, false], 'an older server names none');
  const chip = copyChipHtml(copyStates(row, nodes)[2]);
  assert.match(chip, /<use href="#i-pin"\/>/);
  assert.match(chip, /title="kopia zgodna · preferowany node"/);
  assert.doesNotMatch(copyChipHtml(copyStates(row, nodes)[0]), /i-pin/);
  assert.match(preferredLegendHtml(), /preferowany<\/span> node, na który topik wraca sam, gdy jego kopia jest zgodna/);
});

test('a node of an older version cannot take over; a node that is down or not yet asked is not one', () => {
  const old = [nodes[0], nodes[1], { ...nodes[2], forwarding: 'lacking', forwardCapable: false }];
  assert.deepEqual(transferChoices(healthy, old).map((c) => c.state), ['ok', 'leader', 'old']);
  assert.deepEqual(transferChoices(healthy, nodes.map((n) => ({ ...n, forwarding: 'capable', forwardCapable: true }))).map((c) => c.state), ['ok', 'leader', 'ok']);
  // A server that sends only `forwardCapable`: false from a node that answers is an older build ...
  assert.deepEqual(transferChoices(healthy, [nodes[0], nodes[1], { ...nodes[2], forwardCapable: false }]).map((c) => c.state), ['ok', 'leader', 'old']);
  // ... and a node that does not answer is down, whatever it reports about forwarding.
  assert.deepEqual(transferChoices(healthy, [nodes[0], nodes[1], { ...nodes[2], reachable: false, forwardCapable: false }]).map((c) => c.state), ['ok', 'leader', 'down']);
  assert.deepEqual(transferChoices(healthy, [nodes[0], nodes[1], { ...nodes[2], forwarding: 'unknown', forwardCapable: false }]).map((c) => c.state), ['ok', 'leader', 'ok'], 'unknown is not "older version"');
});

test('seen from a node that does not lead, the leader\'s own row says how far a copy trails', () => {
  // The page is open on rig26; mainpc leads. The leader's replica list carries the lag rig26 cannot see.
  const own = { ...healthy, leaderNodeId: 'n-main' };
  assert.deepEqual(transferChoices(own, nodes).map((c) => c.state), ['ok', 'leader', 'ok'], 'rig26\'s own row knows nothing of the lag');
  const leadersRow = { ...own, isr: ['n-rig', 'n-main'], lagging: [{ nodeId: 'n-mac', lagBytes: 12.4 * MIB, lagMs: 4000 }], preferredLeaderNodeId: 'n-mac' };
  const choices = transferChoices(leadersRow, nodes);
  assert.deepEqual(choices.map((c) => c.state), ['ok', 'leader', 'lag']);
  assert.equal(choices[2].byLeader, true);
  const { win } = open({ choices });
  const cards = [...win.querySelectorAll('tf-choice-card')];
  assert.equal(cards[2].getAttribute('heading'), 'mac-studio · preferowany');
  assert.equal(cards[2].hasAttribute('disabled'), true);
  assert.match(norm(cards[2].getAttribute('description')), /^kopia w tyle o 12,4 MB według nodu prowadzącego mainpc — nie może przejąć, dopóki nie dogoni$/);
});

test('only a paused consumer (or a pause that cannot be read) blocks the whole topic', () => {
  const blockers = [
    { topic: 'odczyty-urzadzen', reason: 'leader_unavailable', nodeId: 'n-main' },
    { topic: 'odczyty-urzadzen', reason: 'would_not_improve' },
    { topic: 'odczyty-urzadzen', reason: 'target_behind', nodeId: 'n-mac' },
    { topic: 'faktury', reason: 'consumer_group_paused' },
  ];
  assert.equal(topicWideBlocker(blockers, 'odczyty-urzadzen'), null, 'none of these says "do not move the topic"');
  assert.equal(topicWideBlocker(blockers, 'faktury').reason, 'consumer_group_paused');
  assert.equal(topicWideBlocker([{ topic: 'x', reason: 'pause_state_unreadable' }], 'x').reason, 'pause_state_unreadable');
  assert.equal(topicWideBlocker(undefined, 'x'), null);
});

test('a topic-wide refusal (a paused consumer) offers no node at all', () => {
  const { win } = open({ choices: transferChoices(healthy, nodes), blockedNote: 'Odbiorca jest wstrzymany.' });
  assert.ok(win.querySelector('[data-act="move"]').hasAttribute('disabled'));
  assert.deepEqual([...win.querySelectorAll('tf-choice-card')].map((c) => c.hasAttribute('disabled')), [true, true, true]);
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Odbiorca jest wstrzymany/);
});

// ---------------------------------------------------------------------------
// The context of the window when the page is open on a node that does not lead
// the partition: the leader's own row and the plan's word on the whole topic.
// ---------------------------------------------------------------------------

const { transferContext, exclusive } = await import('./partitions.js');

const followerView = { ...healthy, leaderNodeId: 'n-main' };
const leaderAnswer = { partitions: [{ ...healthy, partition: 1, leaderNodeId: 'n-main', isr: ['n-rig', 'n-main'], lagging: [{ nodeId: 'n-mac', lagBytes: 12.4 * MIB, lagMs: 4000 }] }] };
const ctxArgs = (over = {}) => ({
  topic: 'odczyty-urzadzen', partition: 1, replica: followerView, nodes,
  readLeader: async () => leaderAnswer, readPlan: async () => ({ blockers: [] }), describeBlocker: (b) => `blokada: ${b.reason}`,
  ...over,
});

test('on a node that does not lead, the leader\'s own row replaces what this node can see', async () => {
  const { row, blockedNote } = await transferContext(ctxArgs());
  assert.deepEqual(transferChoices(row, nodes).map((c) => c.state), ['ok', 'leader', 'lag']);
  assert.equal(blockedNote, '');
  // On the leader itself nothing is asked.
  let asked = 0;
  const onLeader = await transferContext(ctxArgs({ replica: { ...healthy, leaderNodeId: 'n-rig' }, readLeader: async () => { asked += 1; return leaderAnswer; }, readPlan: async () => { asked += 1; return {}; } }));
  assert.equal(asked, 0);
  assert.equal(onLeader.row.leaderNodeId, 'n-rig');
});

test('a plan that says the leader is unavailable or the move would not help does not block the topic; a paused consumer does', async () => {
  for (const reason of ['leader_unavailable', 'would_not_improve', 'target_behind', 'no_admin_right']) {
    const { blockedNote } = await transferContext(ctxArgs({ readPlan: async () => ({ blockers: [{ topic: 'odczyty-urzadzen', reason, nodeId: 'n-mac' }] }) }));
    assert.equal(blockedNote, '', reason);
  }
  for (const reason of ['consumer_group_paused', 'pause_state_unreadable']) {
    const { blockedNote } = await transferContext(ctxArgs({ readPlan: async () => ({ blockers: [{ topic: 'odczyty-urzadzen', reason }] }) }));
    assert.equal(blockedNote, `blokada: ${reason}`);
  }
});

test('when the leader cannot be asked the window still opens, with this node\'s own row and no verdict', async () => {
  const failing = async () => { throw new Error('bus.not_leader'); };
  const { row, blockedNote } = await transferContext(ctxArgs({ readLeader: failing, readPlan: failing }));
  assert.equal(row, followerView);
  assert.equal(blockedNote, '');
  const empty = await transferContext(ctxArgs({ readLeader: async () => ({ partitions: [] }), readPlan: async () => null }));
  assert.equal(empty.row, followerView, 'a leader answer without this partition is no answer');
});

test('a leader that does not answer within the cap lets the window open with the note, and an answering one does not', async () => {
  const never = () => new Promise(() => {});
  const started = Date.now();
  const silent = await transferContext(ctxArgs({ readLeader: never, readPlan: never, waitMs: 40 }));
  assert.ok(Date.now() - started < 1000, 'the wait is bounded by the cap');
  assert.equal(silent.row, followerView);
  assert.equal(silent.leaderSilent, true);
  assert.equal((await transferContext(ctxArgs())).leaderSilent, false);
  assert.equal((await transferContext(ctxArgs({ replica: { ...healthy, leaderNodeId: 'n-rig' } }))).leaderSilent, false, 'nothing was asked on the leader');
  const { win } = open({ choices: transferChoices(healthy, nodes), leaderSilent: true });
  assert.match(win.querySelector('[data-role="leader-silent"]').textContent, /nie odpowiedział w ciągu 2 s/);
  assert.equal(open({ choices: transferChoices(healthy, nodes) }).win.querySelector('[data-role="leader-silent"]'), null);
});

test('a second click while the leader is being asked does nothing', async () => {
  const gate = exclusive();
  let release;
  let started = 0;
  const first = gate(() => { started += 1; return new Promise((r) => { release = r; }); });
  const second = await gate(() => { started += 1; });
  assert.equal(second, undefined);
  assert.equal(started, 1);
  release('opened');
  assert.equal(await first, 'opened');
  assert.equal(await gate(() => 'again'), 'again', 'the gate opens again once the first call finished');
});

// ---------------------------------------------------------------------------
// TB-04 K10 (D4): on a node that does not lead, the leader's answer decides.
// ---------------------------------------------------------------------------

// This node (n-rig) follows n-main. Its own view says only n-main is in sync; the leader sees everyone in sync.
const staleHere = { partition: 1, leaderNodeId: 'n-main', replicas: ['n-rig', 'n-main'], isr: ['n-main'], lagging: [] };
const trueOnLeader = { partition: 1, leaderNodeId: 'n-main', replicas: ['n-rig', 'n-main'], isr: ['n-rig', 'n-main'], lagging: [] };
const pairNodes = [nodes[0], nodes[1]];

test('the leader\'s own rows replace this node\'s trailing view of the partitions it leads', () => {
  const rows = overlayLeaderRows([staleHere, { ...staleHere, partition: 2, leaderNodeId: 'n-rig' }], new Map([['n-main', [trueOnLeader]]]), 'n-rig');
  assert.deepEqual(rows[0].isr, ['n-rig', 'n-main']);
  assert.equal(rows[0].awaitsLeader, undefined);
  assert.equal(rows[1].isr.length, 1, 'a partition this node leads is its own truth');
  assert.equal(transferBlocker(rows[0], pairNodes), null, 'the copy here is in sync on the leader, so the move is open');
  // The pin falls back to this node\'s when the leader\'s list does not carry it.
  const pinned = overlayLeaderRows([{ ...staleHere, preferredLeaderNodeId: 'n-rig' }], new Map([['n-main', [trueOnLeader]]]), 'n-rig');
  assert.equal(pinned[0].preferredLeaderNodeId, 'n-rig');
});

test('until the leader has answered the move says "sprawdzam", never "no node has an in-sync copy"', () => {
  const [row] = overlayLeaderRows([staleHere], new Map(), 'n-rig');
  assert.equal(row.awaitsLeader, true);
  const text = transferBlocker(row, pairNodes);
  assert.match(text, /Sprawdzam/);
  assert.doesNotMatch(text, /Żaden inny node nie ma teraz zgodnej kopii/);
  // The same view without waiting is judged from what it holds (here: the stale answer, which is why the leader is asked first).
  assert.match(transferBlocker(staleHere, pairNodes), /Żaden inny node nie ma teraz zgodnej kopii/);
});

test('a leader that did not answer, or does not hold the partition, leaves this node\'s row in place and not waiting', () => {
  assert.equal(overlayLeaderRows([staleHere], new Map([['n-main', null]]), 'n-rig')[0].awaitsLeader, undefined);
  assert.equal(overlayLeaderRows([staleHere], new Map([['n-main', [{ ...trueOnLeader, partition: 9 }]]]), 'n-rig')[0], staleHere);
  // A row the leader reports as led by another node (the leader changed meanwhile) is not taken for the leader\'s own.
  assert.equal(overlayLeaderRows([staleHere], new Map([['n-main', [{ ...trueOnLeader, leaderNodeId: 'n-mac' }]]]), 'n-rig')[0], staleHere);
});

test('every partition of a topic gets the same verdict from the leader, so no button stands alone', () => {
  const stale = (partition) => ({ ...staleHere, partition, isr: partition === 1 ? ['n-rig', 'n-main'] : ['n-main'] });
  const onLeader = (partition) => ({ ...trueOnLeader, partition });
  const parts = [0, 1, 2, 3].map(stale);
  assert.deepEqual(parts.map((p) => transferBlocker(p, pairNodes) === null), [false, true, false, false], 'this node\'s own view: one enabled, three disabled');
  const decided = overlayLeaderRows(parts, new Map([['n-main', [0, 1, 2, 3].map(onLeader)]]), 'n-rig');
  assert.deepEqual(decided.map((p) => transferBlocker(p, pairNodes)), [null, null, null, null]);
});

test('the leaders are asked once per topic and leader, never about what this node leads, and a failure is a null answer', async () => {
  const asked = [];
  const lists = [
    { key: 'wyniki', partitions: [staleHere, { ...staleHere, partition: 2, leaderNodeId: 'n-mac' }] },
    { key: '__dlq.wyniki', partitions: [{ ...staleHere, leaderNodeId: 'n-rig' }] },
    { key: 'faktury', partitions: [] },
  ];
  const answers = await askLeaders({
    lists,
    localId: 'n-rig',
    read: async (key, leaderId) => {
      asked.push(`${key}@${leaderId}`);
      if (leaderId === 'n-mac') throw new Error('timed out');
      return { partitions: [trueOnLeader] };
    },
  });
  assert.deepEqual(asked.sort(), ['wyniki@n-mac', 'wyniki@n-main']);
  assert.deepEqual([...answers.keys()], ['wyniki']);
  assert.equal(answers.get('wyniki').get('n-main')[0].isr.length, 2);
  assert.equal(answers.get('wyniki').get('n-mac'), null);
});

test('a node shown by its short id carries the full id as a tooltip in the leader cell and on the copy chip', () => {
  const hex = '41291e2758d793a2d740f5db7d7c99141f32f02199c1427803d4373636e7ca57';
  const bare = [{ nodeId: hex, label: '', isLocal: true }, { nodeId: 'b'.repeat(64), label: '' }];
  const p = { partition: 0, leaderNodeId: hex, replicas: [hex, 'b'.repeat(64)], isr: [hex], lagging: [] };
  assert.equal(copyStates(p, bare)[0].label, '41291e27');
  assert.match(copyChipHtml(copyStates(p, bare)[0]), new RegExp(`title="[^"]*${hex}`));
  assert.match(nodeNameHtml('41291e27', hex), new RegExp(`title="${hex}"[^>]*>41291e27<`));
  assert.doesNotMatch(nodeNameHtml('rig26'), /title=/);
});

test('a healthy topic costs no call to another node, a doubtful one costs one per list and leader', async () => {
  const calls = [];
  const read = async (key, leaderId) => { calls.push(`${key}@${leaderId}`); return { partitions: [] }; };
  const healthyRow = (partition) => ({ ...healthy, partition, leaderNodeId: 'n-main' });
  const lists = [
    // 20 healthy topics led by another node: nothing is asked.
    ...Array.from({ length: 20 }, (_, i) => ({ key: `topik-${i}`, partitions: [healthyRow(0), healthyRow(1)] })),
    // One topic with a copy behind, four partitions: one call, not four.
    { key: 'wyniki', partitions: [0, 1, 2, 3].map((partition) => ({ ...behind, partition })) },
  ];
  assert.equal(needsLeaderVerdict(healthyRow(0), nodes), false);
  assert.equal(needsLeaderVerdict(behind, nodes), true);
  assert.equal(needsLeaderVerdict({ ...healthyRow(0), unavailableReason: 'NoIsr' }, nodes), true);
  await askLeaders({ lists, localId: 'n-rig', nodes, read });
  assert.deepEqual(calls, ['wyniki@n-main']);
  // A healthy row is never swapped for an older answer, and is not marked as waiting.
  assert.equal(overlayLeaderRows([healthyRow(0)], new Map(), 'n-rig', nodes)[0].awaitsLeader, undefined);
});
