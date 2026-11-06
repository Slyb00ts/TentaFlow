// ============ File: flows-builder/process-token-overlay.test.js — token view to overlay model, scope trail, polling feed and the canvas overlay ============

import { window } from '../../sdk-runtime/_dom-test-harness.js';
import test, { after } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

for (const name of ['MutationObserver', 'ResizeObserver', 'Document', 'CSS', 'navigator']) {
  if (globalThis[name] === undefined) globalThis[name] = window[name];
}
Object.defineProperty(globalThis, 'localStorage', { value: window.localStorage, configurable: true });
window.HTMLCanvasElement.prototype.getContext = () => ({ measureText: (value) => ({ width: String(value).length * 7 }) });
const hostFetch = globalThis.fetch;
globalThis.fetch = (url, options) => {
  const path = String(url);
  if (/^\/(i18n|css)\//.test(path)) return Promise.resolve(new Response(readFileSync(new URL(`../../..${path}`, import.meta.url)), { headers: { 'Content-Type': path.endsWith('.json') ? 'application/json' : 'text/css' } }));
  return hostFetch(url, options);
};
const { I18n } = await import('../../i18n.js');
const { emptyProcessModel, processTemplates } = await import('./bpmn.js');
const { FlowCanvas } = await import('./canvas.js');
const overlay = await import('./process-token-overlay.js');
const { ascendTo, buildTokenOverlay, canvasOverlay, createTokenFeed, drillInto, instanceIsTerminal, isCoordinatorToken, repeatingNodeIds, scopeHasEnded, tokenWaitKind, trailPath, unknownElementIds } = overlay;
localStorage.setItem('tentaflow_lang', 'en');
await I18n.init();
after(() => window.happyDOM?.close?.());

let seq = 0;
function token(nodeId, fields = {}) {
  seq += 1;
  return { tokenId: `t${seq}`, scopeId: 'i1', nodeId, status: 'Waiting', sinceMs: 1000 + seq, incidentIds: [], ...fields };
}
const answer = (tokens, fields = {}) => ({ instanceRevision: 5, notModified: false, version: 2, processId: 'P',
  tokens, takenEdgeIds: [], visitedNodeIds: [], truncated: false, ...fields });

test('tokens are counted per element with the kind of wait and the strongest tone', () => {
  const view = buildTokenOverlay(answer([
    token('Review', { wait: { UserTask: { userTaskId: 'u1' } } }),
    token('Review', { wait: { UserTask: { userTaskId: 'u2' } } }),
    token('Call', { wait: { Job: { jobId: 'j1' } }, incidentIds: ['inc-1'] }),
    token('Pause', { wait: { Timer: { timerId: 'm1', dueAtMs: 9 } } }),
    token('Go', { status: 'Ready' }),
    token('Join', { status: 'Joining', wait: 'Gateway' }),
  ], { takenEdgeIds: ['E1'], visitedNodeIds: ['Start', 'Review'] }));
  const review = view.nodes.get('Review');
  assert.equal(review.count, 2);
  assert.deepEqual(review.kinds, ['UserTask']);
  assert.equal(review.tone, 'waiting');
  assert.equal(view.nodes.get('Call').tone, 'incident');
  assert.deepEqual(view.nodes.get('Call').incidentIds, ['inc-1']);
  assert.equal(view.nodes.get('Go').tone, 'active');
  assert.equal(view.nodes.get('Go').primaryKind, 'Ready');
  assert.equal(view.nodes.get('Join').primaryKind, 'Gateway');
  assert.equal(view.nodes.get('Pause').tokens[0].wait.Timer.dueAtMs, 9);
  assert.deepEqual([...view.takenEdgeIds], ['E1']);
  assert.deepEqual([...view.visitedNodeIds], ['Start', 'Review']);
  assert.equal(view.revision, 5);
});

test('a repetition coordinator token is not counted beside its occurrences', () => {
  const coordinator = token('Task', { wait: undefined });
  const live = [0, 1].map((ordinal) => token('Task', { wait: { Job: { jobId: `j${ordinal}` } }, occurrence: { groupId: 'g', ordinal } }));
  const view = buildTokenOverlay(answer([coordinator, ...live]), { repeatingNodeIds: new Set(['Task']) });
  assert.equal(view.nodes.get('Task').count, 2);
  assert.equal(view.nodes.get('Task').occurrenceCount, 2);
  assert.ok(!view.nodes.get('Task').tokens.some((entry) => entry.tokenId === coordinator.tokenId));

  // Without the model the sibling occurrences alone identify the coordinator.
  const bare = buildTokenOverlay(answer([coordinator, ...live]));
  assert.equal(bare.nodes.get('Task').count, 2);
  assert.equal(isCoordinatorToken(coordinator, [coordinator, ...live], new Set()), true);
  assert.equal(isCoordinatorToken(live[0], [coordinator, ...live], new Set()), false);

  // A coordinator alone on a repeating element between two occurrences is still no token.
  const between = buildTokenOverlay(answer([coordinator]), { repeatingNodeIds: new Set(['Task']) });
  assert.equal(between.nodes.has('Task'), false);
  // On an element that does not repeat a token without a wait is a real token.
  assert.equal(buildTokenOverlay(answer([token('Plain', { status: 'Ready' })])).nodes.get('Plain').count, 1);
});

test('an incident held by the coordinator marks the element without adding a token', () => {
  const coordinator = token('Task', { incidentIds: ['inc-9'] });
  const view = buildTokenOverlay(answer([coordinator]), { repeatingNodeIds: new Set(['Task']) });
  const entry = view.nodes.get('Task');
  assert.equal(entry.count, 0);
  assert.equal(entry.tone, 'incident');
  assert.deepEqual(entry.incidentIds, ['inc-9']);
  const badge = canvasOverlay(view, (key) => key).nodes.get('Task');
  assert.equal(badge.count, 0);
  assert.equal(badge.icon, 'alert');
});

test('a sub-process token counts the work inside it and keeps its scope for the drill-down', () => {
  const view = buildTokenOverlay(answer([
    token('Sub', { wait: { Scope: { scopeId: 'child-1', activeTokens: 3 } } }),
    token('Sub', { wait: { Scope: { scopeId: 'child-2', activeTokens: 1 } }, occurrence: { groupId: 'g', ordinal: 1 } }),
  ]));
  const sub = view.nodes.get('Sub');
  assert.equal(sub.count, 2);
  assert.equal(sub.inside, 4);
  assert.equal(sub.displayCount, 4);
  assert.deepEqual(sub.tokens.map((entry) => entry.wait.Scope.scopeId), ['child-1', 'child-2']);
  assert.equal(tokenWaitKind(sub.tokens[0]), 'Scope');
});

test('the canvas model carries count, tone, icon and a sentence made from the translated parts', () => {
  const view = buildTokenOverlay(answer([
    token('Review', { wait: { UserTask: { userTaskId: 'u1' } }, incidentIds: ['inc-1'] }),
    token('Review', { wait: { UserTask: { userTaskId: 'u2' } } }),
    token('Pause', { wait: { Timer: { timerId: 'm', dueAtMs: null } } }),
    token('Join', { status: 'Joining', wait: 'Gateway' }),
  ], { takenEdgeIds: ['E1'] }));
  const translate = (key, values) => `${key}${values ? JSON.stringify(values) : ''}`;
  const model = canvasOverlay(view, translate);
  const review = model.nodes.get('Review');
  assert.equal(review.count, 2);
  assert.equal(review.tone, 'incident');
  assert.equal(review.icon, 'alert');
  assert.match(review.label, /diagram_token_count\{"count":2\}/);
  assert.match(review.label, /diagram_wait_usertask/);
  assert.match(review.label, /diagram_incident_count\{"count":1\}/);
  assert.equal(model.nodes.get('Pause').icon, 'clock');
  assert.equal(model.nodes.get('Join').icon, 'pause', 'a held token is not drawn with the plus of a parallel gateway');
  assert.deepEqual([...model.takenEdgeIds], ['E1']);
});

test('truncated answers say so and an unknown wait is read as ready', () => {
  assert.equal(buildTokenOverlay(answer([], { truncated: true })).truncated, true);
  assert.equal(buildTokenOverlay(answer([])).truncated, false);
  assert.equal(tokenWaitKind({ status: 'Waiting', wait: { Surprise: {} } }), 'Ready');
  assert.equal(tokenWaitKind({ status: 'Joining' }), 'Gateway');
  assert.deepEqual([...repeatingNodeIds({ nodes: [{ id: 'A', repeat: { StructuredLoop: {} } }, { id: 'B', repeat: null }, { id: 'C' }] })], ['A']);
});

test('the scope trail drills into a sub-process, ignores a repeated open and climbs back by depth', () => {
  let trail = [];
  trail = drillInto(trail, { scopeId: 's1', nodeId: 'Sub', name: 'Sub' });
  trail = drillInto(trail, { scopeId: 's1', nodeId: 'Sub', name: 'Sub' });
  assert.equal(trail.length, 1);
  trail = drillInto(trail, { scopeId: 's2', nodeId: 'Inner', name: 'Inner' });
  assert.deepEqual(trailPath(trail), ['Sub', 'Inner']);
  assert.deepEqual(trailPath(ascendTo(trail, 1)), ['Sub']);
  assert.deepEqual(ascendTo(trail, 0), []);
  assert.equal(ascendTo(trail, 5), trail);
});

test('terminal statuses end the run for the feed and nothing else does', () => {
  for (const status of ['Completed', 'Cancelled', 'Error']) assert.equal(instanceIsTerminal(status), true);
  for (const status of ['Running', 'Waiting', 'Incident', undefined]) assert.equal(instanceIsTerminal(status), false);
});

// A manual clock: timers fire when the test says so.
function clock() {
  const timers = new Map();
  let next = 1;
  return {
    setTimer: (callback, delay) => { timers.set(next, { callback, delay }); return next++; },
    clearTimer: (id) => { timers.delete(id); },
    get pending() { return timers.size; },
    async fire() {
      const [id, timer] = [...timers].at(0) ?? [];
      assert.ok(timer, 'a timer is waiting');
      timers.delete(id);
      timer.callback();
      await settle();
    },
    delays: () => [...timers.values()].map((timer) => timer.delay),
  };
}
const settle = async () => { for (let turn = 0; turn < 10; turn += 1) await new Promise((resolve) => setImmediate(resolve)); };

function feedWith(responses, { terminal = () => false, ...extra } = {}) {
  const time = clock();
  const requests = [];
  const shown = [];
  const errors = [];
  const feed = createTokenFeed({
    instanceId: 'i1',
    request: async (request) => {
      requests.push(request);
      const reply = responses(request, requests.length);
      if (reply instanceof Error) throw reply;
      return reply;
    },
    isTerminal: terminal,
    onOverlay: (response, info) => shown.push({ response, ...info }),
    onError: (error) => errors.push(error),
    setTimer: time.setTimer, clearTimer: time.clearTimer, ...extra,
  });
  return { feed, time, requests, shown, errors };
}

test('the feed reads at once, repeats every three seconds and asks with the revision it holds', async () => {
  const { feed, time, requests, shown } = feedWith((request) => (request.ifRevision === 5
    ? answer([], { notModified: true, instanceRevision: 5 }) : answer([token('A')])));
  feed.start();
  await settle();
  assert.equal(requests.length, 1);
  assert.equal(requests[0].ifRevision, null);
  assert.equal(shown.length, 1);
  assert.deepEqual(time.delays(), [3000]);
  await time.fire();
  assert.equal(requests[1].ifRevision, 5);
  // A not-modified answer re-reports the cached answer, flagged as not fresh.
  assert.equal(shown.at(-1).fresh, false);
  assert.equal(shown.at(-1).response.tokens.length, 1);
  feed.dispose();
});

test('the feed never overlaps requests and runs one more read after a poke made meanwhile', async () => {
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  const { feed, requests } = feedWith(() => gate.then(() => answer([token('A')])));
  feed.start();
  await settle();
  feed.poke();
  feed.poke();
  await settle();
  assert.equal(requests.length, 1);
  release();
  await settle();
  assert.equal(requests.length, 2, 'the pokes during the flight collapse into one more read');
  feed.dispose();
});

test('stopping the feed cancels the clock and starting it again reads at once', async () => {
  const { feed, time, requests } = feedWith(() => answer([]));
  feed.start();
  await settle();
  assert.equal(time.pending, 1);
  feed.stop();
  assert.equal(time.pending, 0);
  feed.poke();
  await settle();
  assert.equal(requests.length, 1, 'a stopped feed does not read');
  feed.start();
  await settle();
  assert.equal(requests.length, 2);
  feed.dispose();
  assert.equal(time.pending, 0);
  feed.start();
  await settle();
  assert.equal(requests.length, 2, 'a disposed feed stays quiet');
});

test('a read that starts once the run is terminal is the last one', async () => {
  let terminal = false;
  const { feed, time, requests } = feedWith(() => answer([]), { terminal: () => terminal });
  feed.start();
  await settle();
  assert.equal(time.pending, 1);
  terminal = true;
  feed.poke();
  await settle();
  assert.equal(requests.length, 2);
  assert.equal(time.pending, 0);
  assert.equal(feed.running, false);
  feed.poke();
  await settle();
  assert.equal(requests.length, 2);
  feed.dispose();
});

test('an answer is kept per scope: going back shows the cached one and asks conditionally', async () => {
  const { feed, requests, shown } = feedWith((request) => {
    if (request.scopeId === 'child') return answer([token('Inner', { scopeId: 'child' })], { instanceRevision: 8 });
    return request.ifRevision === 5 ? answer([], { notModified: true }) : answer([token('Outer')]);
  });
  feed.start();
  await settle();
  feed.setScope('child');
  await settle();
  assert.equal(requests.at(-1).scopeId, 'child');
  assert.equal(requests.at(-1).ifRevision, null);
  assert.equal(shown.at(-1).response.tokens[0].nodeId, 'Inner');
  const before = requests.length;
  feed.setScope(null);
  assert.equal(shown.at(-1).response.tokens[0].nodeId, 'Outer', 'the cached root answer is shown before any read returns');
  await settle();
  assert.equal(requests.length, before + 1);
  assert.equal(requests.at(-1).ifRevision, 5);
  assert.equal(feed.cached('child').tokens[0].nodeId, 'Inner');
  feed.dispose();
});

test('a late answer of a scope that is no longer shown only fills the cache', async () => {
  const gates = [];
  const { feed, shown } = feedWith((request) => new Promise((resolve) => gates.push(() => resolve(
    answer([token(request.scopeId ?? 'root', { scopeId: request.scopeId ?? 'i1' })])))).then((value) => value));
  feed.setScope('slow');
  feed.start();
  await settle();
  feed.setScope(null);
  gates.shift()();
  await settle();
  assert.equal(shown.length, 0, 'nothing is shown for the scope that was left');
  assert.ok(feed.cached('slow'));
  feed.dispose();
});

test('a failed read is reported once and the clock keeps running', async () => {
  let fail = true;
  const { feed, time, errors, shown } = feedWith(() => (fail ? new Error('offline') : answer([token('A')])));
  feed.start();
  await settle();
  assert.equal(errors.length, 1);
  assert.equal(time.pending, 1);
  fail = false;
  await time.fire();
  assert.equal(shown.length, 1);
  feed.dispose();
});

test('the canvas draws counts, tones and taken edges without rebuilding the element', async () => {
  const model = emptyProcessModel();
  const body = model;
  const root = document.createElement('div');
  document.body.append(root);
  const canvas = new FlowCanvas(root, { mode: 'bpmn', readOnly: true });
  try {
    canvas.setTemplates(processTemplates());
    canvas.setData(structuredClone(model));
    const first = body.nodes[0].id;
    const element = () => canvas.nodesLayer.querySelector(`[data-node-id="${first}"]`);
    const before = element();
    const base = before.getAttribute('aria-label');
    canvas.setRuntimeOverlay({
      nodes: new Map([[first, { count: 3, tone: 'waiting', icon: 'user', label: '3 tokens' }]]),
      takenEdgeIds: new Set(body.sequenceFlows.map((flow) => flow.id)), visitedNodeIds: new Set(),
    });
    assert.ok(element() === before, 'the element is not replaced');
    assert.ok(element().classList.contains('fb-node--token-waiting'));
    assert.equal(element().querySelector('.fb-runtime-badge b').textContent, '3');
    assert.match(element().getAttribute('aria-label'), /3 tokens/);
    assert.ok(root.classList.contains('fb-runtime'));
    const taken = () => [...canvas.svg.querySelectorAll('.fb-edge-path.fb-edge--taken')].map((path) => path.dataset.edgeId);
    assert.deepEqual(taken(), ['Sequence_start_end']);
    canvas._renderEdges();
    assert.deepEqual(taken(), ['Sequence_start_end'], 'a redrawn edge keeps its mark');
    const badge = element().querySelector('.fb-runtime-badge');
    canvas.setRuntimeOverlay({ nodes: new Map([[first, { count: 3, tone: 'waiting', icon: 'user', label: '3 tokens' }]]), takenEdgeIds: new Set(), visitedNodeIds: new Set() });
    assert.deepEqual(taken(), [], 'the mark follows the overlay');
    assert.ok(element().querySelector('.fb-runtime-badge') === badge, 'an unchanged badge is kept');
    canvas.setRuntimeOverlay(null);
    assert.equal(element().querySelector('.fb-runtime-badge'), null);
    assert.equal(element().getAttribute('aria-label'), base);
    assert.ok(!root.classList.contains('fb-runtime'));
  } finally {
    canvas.destroy();
  }
});

test('occurrences of one element are listed in the order they were issued', () => {
  const view = buildTokenOverlay(answer([
    token('Review', { occurrence: { groupId: 'g', ordinal: 2 }, wait: { Job: { jobId: 'a' } } }),
    token('Review', { occurrence: { groupId: 'g', ordinal: 0 }, wait: { Job: { jobId: 'b' } } }),
    token('Review', { occurrence: { groupId: 'g', ordinal: 1 }, wait: { Job: { jobId: 'c' } } }),
  ]));
  assert.deepEqual(view.nodes.get('Review').tokens.map((item) => item.occurrence.ordinal), [0, 1, 2]);
});

test('tokens on elements the shown body lacks are reported by id', () => {
  const view = buildTokenOverlay(answer([token('Review', { wait: 'Gateway' }), token('Ghost', { wait: 'Gateway' })]));
  assert.deepEqual(unknownElementIds(view, { nodes: [{ id: 'Review' }] }), ['Ghost']);
  assert.deepEqual(unknownElementIds(view, { nodes: [{ id: 'Review' }, { id: 'Ghost' }] }), []);
});

test('a scope with no live token ends, unless the answer was cut', () => {
  assert.equal(scopeHasEnded(answer([])), true);
  assert.equal(scopeHasEnded(answer([token('A')])), false);
  assert.equal(scopeHasEnded(answer([], { truncated: true })), false);
});

test('a retired scope is never read again, by the clock, a poke or a restart', async () => {
  const { feed, time, requests } = feedWith(() => answer([]));
  feed.setScope('dead');
  feed.start();
  await settle();
  assert.equal(requests.length, 1);
  feed.retire('dead');
  assert.equal(time.pending, 0);
  feed.poke();
  feed.start();
  await settle();
  assert.equal(requests.length, 1, 'retired');
  feed.setScope(null);
  await settle();
  assert.equal(requests.length, 2, 'another scope reads normally');
  assert.equal(time.pending, 1);
  feed.dispose();
});

test('the canvas names every flow tab stop as taken or not taken and trims a closing full stop', async () => {
  const model = emptyProcessModel();
  const root = document.createElement('div');
  document.body.append(root);
  const canvas = new FlowCanvas(root, { mode: 'bpmn', readOnly: true });
  try {
    canvas.setTemplates(processTemplates());
    const data = structuredClone(model);
    data.nodes[0].name = 'Begin.';
    canvas.setData(data);
    const first = data.nodes[0].id;
    const edgeId = data.sequenceFlows[0].id;
    const hit = () => canvas.svg.querySelector(`.fb-edge-hit[data-edge-id="${edgeId}"]`);
    const base = hit().getAttribute('aria-label');
    const draw = (taken) => canvas.setRuntimeOverlay({
      nodes: new Map([[first, { count: 1, tone: 'active', icon: 'play', label: '1 token' }]]),
      takenEdgeIds: new Set(taken), visitedNodeIds: new Set(), edgeTakenLabel: 'Flow taken', edgeNotTakenLabel: 'Flow not taken',
    });
    draw([]);
    assert.equal(hit().getAttribute('aria-label'), `${base}. Flow not taken`);
    draw([edgeId]);
    assert.equal(hit().getAttribute('aria-label'), `${base}. Flow taken`);
    canvas._renderEdges();
    assert.equal(hit().getAttribute('aria-label'), `${base}. Flow taken`, 'a redrawn edge keeps its state');
    assert.doesNotMatch(canvas.nodesLayer.querySelector(`[data-node-id="${first}"]`).getAttribute('aria-label'), /\.\./);
    canvas.setRuntimeOverlay(null);
    assert.equal(hit().getAttribute('aria-label'), base);
  } finally {
    canvas.destroy();
  }
});

test('every diagram key exists in all five languages', () => {
  const names = ['pl', 'en', 'de', 'es', 'fr'];
  const bundles = names.map((name) => JSON.parse(readFileSync(new URL(`../../../i18n/${name}.json`, import.meta.url), 'utf8')).bpmn);
  const wanted = Object.keys(bundles[1]).filter((key) => /^diagram_|^redacted_|^variable_types$|^event_redacted$/.test(key));
  assert.ok(wanted.length >= 40);
  for (const [index, bundle] of bundles.entries()) {
    for (const key of wanted) assert.equal(typeof bundle[key], 'string', `${names[index]} lacks bpmn.${key}`);
  }
  for (const key of ['diagram_token_count', 'diagram_incident_count', 'diagram_inside', 'diagram_repeat_active']) {
    assert.match(bundles[0][key], /\{\w+\|[^|}]+\|[^|}]+\|[^}]+\}/, `pl bpmn.${key} needs three plural forms`);
  }
});
