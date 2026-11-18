import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const s = await import('./consumer-sla.js');

const config = (over = {}) => ({ instance_id: 'bus-1', topic: 'orders', group: 'g', service_account: 'u1', ...over });
const graph = (over) => JSON.stringify({ nodes: [{ type: 'bus_consume', config: config(over) }, { type: 'log', config: {} }] });
const ASK = { group: 'k:u1.g', topic: 'orders', instanceId: 'bus-1' };

test('a flow reads as the consumer whose instance, effective group and topic all match', () => {
  assert.ok(s.readsAs(graph(), ASK));
  assert.ok(!s.readsAs(graph(), { ...ASK, group: 'g' }));
  assert.ok(!s.readsAs(graph(), { ...ASK, topic: 'other' }));
  assert.ok(!s.readsAs(graph({ instance_id: 'bus-2' }), ASK), 'the same topic and group of another bus instance');
  assert.ok(!s.readsAs(graph({ instance_id: '' }), ASK));
  assert.deepEqual(s.consumeConfigs('not json'), []);
});

const baseCall = (extra = {}) => async (kind, payload, options) => {
  if (kind === 'flowListRequest') {
    const node = options?.targetNodeId ?? 'n1';
    return { flows: [{ id: 'f1', name: 'Orders', slaPolicyId: 'p1', busConsumer: extra.consumers?.[node] ?? { state: 'standby', inFlight: 0, maxConcurrency: 4 }, slaBrief: extra.brief ?? null }] };
  }
  if (kind === 'flowDetailRequest') return { graphJson: graph() };
  if (kind === 'slaPolicyGetRequest') {
    if (extra.policyFails) throw new Error('boom');
    return { policy: { id: 'p1', spec: { name: 'Std', maxConcurrency: 4, ordering: 'partition' } } };
  }
  return { statuses: [] };
};

test('loadConsumerSla finds the flow, reads each graph once and answers null without a match', async () => {
  const calls = [];
  const call = (kind, ...rest) => { calls.push(kind); return baseCall()(kind, ...rest); };
  const cache = new Map();
  const found = await s.loadConsumerSla({ ...ASK, call, cache });
  assert.equal(found.flow.name, 'Orders');
  assert.equal(found.policy.spec.name, 'Std');
  await s.loadConsumerSla({ ...ASK, call, cache });
  assert.equal(calls.filter((k) => k === 'flowDetailRequest').length, 1);
  assert.equal(await s.loadConsumerSla({ ...ASK, group: 'k:zz.g', call, cache: new Map() }), null);
  assert.equal(await s.loadConsumerSla({ ...ASK, instanceId: 'bus-2', call, cache: new Map() }), null);
});

test('the consumer status comes from the node whose consumer runs, not from the connected one', async () => {
  const nodes = async () => [{ nodeId: 'n1', isLocal: true }, { nodeId: 'n2', isLocal: false }];
  const call = baseCall({ consumers: { n1: { state: 'standby', inFlight: 0, maxConcurrency: 4 }, n2: { state: 'running', inFlight: 3, maxConcurrency: 4, throttledUntil: 9 } } });
  const found = await s.loadConsumerSla({ ...ASK, call, cache: new Map(), nodes });
  assert.equal(found.consumer.inFlight, 3);
  assert.equal(found.consumer.throttledUntil, 9);
  const hung = async (kind, payload, options) => (options?.targetNodeId === 'n2' ? new Promise(() => {}) : call(kind, payload, options));
  const fallback = await s.loadConsumerSla({ ...ASK, call: hung, cache: new Map(), nodes, deadlineMs: 20 });
  assert.equal(fallback.consumer.state, 'standby', 'a node that does not answer falls back to the connected one');
});

test('the tile, the held alert and the settings card say what the policy does', () => {
  const sla = { flow: { name: 'Orders' }, elsewhere: false, policyError: false, consumer: { inFlight: 3, maxConcurrency: 4, throttledUntil: 2_000_000 }, status: null,
    policy: { spec: { name: 'Std', maxConcurrency: 4, ordering: 'partition', callLimit: 24000, callWindowSecs: 60, warnAtPct: 80 } } };
  assert.equal(s.concurrencyTile(sla).value, '3/4');
  assert.equal(s.concurrencyTile({ consumer: { inFlight: 1 } }), null);
  assert.equal(s.throttledUntilMs(sla, 3_000_000), null);
  assert.equal(s.throttledUntilMs(sla, 1_000_000), 2_000_000);
  const texts = s.throttledTexts(sla, 1_000_000);
  assert.match(texts.title, /^Wstrzymano limitem do \d\d:\d\d:\d\d$/);
  assert.match(texts.m1, /„Std”/);
  const html = s.slaSettingsHtml(sla);
  assert.match(html, /Std/);
  assert.match(html, /24\s?000 na minutę/);
  assert.match(html, /data-go="sla-policies"/);
  assert.equal(s.slaSettingsHtml(null), '');
});

test('without a readable policy nothing says "Brak polityki" and no empty name is quoted', () => {
  const base = { flow: { name: 'Orders' }, consumer: { inFlight: 1, maxConcurrency: 4, throttledUntil: 2_000_000 }, status: null, policy: null };
  const elsewhere = s.slaSettingsHtml({ ...base, elsewhere: true, policyError: false });
  assert.match(elsewhere, /Politykę nadała inna organizacja/);
  assert.doesNotMatch(elsewhere, /Brak polityki/);
  const failed = s.slaSettingsHtml({ ...base, elsewhere: false, policyError: true });
  assert.match(failed, /Nie udało się wczytać polityki/);
  assert.doesNotMatch(failed, /Brak polityki/);
  assert.match(s.slaSettingsHtml({ ...base, elsewhere: false, policyError: false }), /Brak polityki/);
  for (const sla of [{ ...base, elsewhere: true }, { ...base, policyError: true }]) {
    const texts = s.throttledTexts(sla, 1_000_000);
    assert.doesNotMatch(texts.m1, /„”/);
    assert.doesNotMatch(texts.m1, /polityka „/);
  }
  assert.equal(s.concurrencyTile({ ...base, elsewhere: true }).delta, '');
});

test('a policy fetch that fails is reported as a load error, not as no policy', async () => {
  const found = await s.loadConsumerSla({ ...ASK, call: baseCall({ policyFails: true }), cache: new Map(), deadlineMs: 50 });
  assert.equal(found.policyError, true);
  const elsewhere = await s.loadConsumerSla({ ...ASK, call: baseCall({ brief: { governedElsewhere: true } }), cache: new Map() });
  assert.equal(elsewhere.elsewhere, true);
  assert.equal(elsewhere.policyError, false);
});
