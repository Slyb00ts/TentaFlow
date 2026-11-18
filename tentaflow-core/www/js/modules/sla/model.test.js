import '../tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const m = await import('./model.js');

test('every coded error maps to its own localized message', () => {
  const text = (error) => m.describeSlaError(error);
  assert.equal(text({ code: 'name_taken' }).field, 'name');
  assert.match(text({ code: 'name_taken' }).message, /już istnieje/);
  assert.match(text({ code: 'system_flow' }).message, /systemowych/);
  assert.match(text({ code: 'governed_elsewhere' }).message, /inna organizacja/);
  assert.match(text({ code: 'policy_in_use', assignments: 3 }).message, /3 przepływów/);
  assert.equal(text({ code: 'storage_error', detail: 'sqlite exploded' }).message, 'Nie udało się wykonać operacji. Spróbuj ponownie.');
  assert.equal(text({ code: 'something_new' }).field, null);
});

test('invalid_value is shown on its own field, with the range for numbers', () => {
  const window = m.describeSlaError({ code: 'invalid_value', field: 'call_window_secs' });
  assert.equal(window.field, 'call_window');
  const threads = m.describeSlaError({ code: 'invalid_value', field: 'max_concurrency' });
  assert.equal(threads.field, 'max_concurrency');
  assert.match(threads.message, /od 1 do 1\s?024/);
  assert.match(m.describeSlaError({ code: 'invalid_value', field: 'name' }).message, /nazw/);
});

test('unlessRefused throws a refusal that failureText explains in words', () => {
  assert.throws(() => m.unlessRefused({ error: { code: 'name_taken' } }), m.SlaRefusal);
  assert.equal(m.unlessRefused({ ok: 1 }).ok, 1);
  assert.match(m.failureText(new m.SlaRefusal({ code: 'name_taken' })), /już istnieje/);
});

test('delete is blocked with the number of flows in the text', () => {
  assert.match(m.deleteBlockedText(1), /1 przepływu/);
  assert.match(m.deleteBlockedText(5), /5 przepływów/);
});

const brief = (over = {}) => ({ state: 'ok', inFlight: 3, maxConcurrency: 4, windowUsed: null, windowLimit: null, governedElsewhere: false, ...over });
const own = new Map([['p1', { id: 'p1', spec: { name: 'Standard' } }]]);

test('slaView: none, system, own and governed elsewhere', () => {
  assert.equal(m.slaView({ id: 'a' }, own).kind, 'none');
  assert.equal(m.slaView({ id: 'a', isSystem: true, slaBrief: brief() }, own).kind, 'system');
  const mine = m.slaView({ id: 'a', slaPolicyId: 'p1', slaBrief: brief() }, own);
  assert.deepEqual([mine.kind, mine.name, mine.chipStatus, mine.detail], ['own', 'Standard', 'ok', 'równoległość 3/4']);
  const other = m.slaView({ id: 'a', slaPolicyId: 'p9', slaBrief: brief({ governedElsewhere: true, state: 'warn' }) }, own);
  assert.equal(other.kind, 'elsewhere');
  assert.equal(other.name, 'Inna organizacja');
  assert.equal(other.policyId, '');
  assert.equal(other.chipStatus, 'warn');
  assert.equal(other.detail, 'równoległość 3/4');
});

test('slaView: a breach replaces the occupancy line', () => {
  const status = { openBreaches: [{ kind: 'latency', peakValue: 1200, threshold: 800 }] };
  const view = m.slaView({ id: 'a', slaPolicyId: 'p1', slaBrief: brief({ state: 'breach' }) }, own, status);
  assert.equal(view.chipStatus, 'err');
  assert.match(view.detail, /p95 1\s?200 ms > 800 ms/);
});

test('protocol failures get a localized message of their own, never the server text', () => {
  const secret = 'internal: sqlite error at /var/lib/x';
  const text = (code, message = secret) => m.failureText(Object.assign(new Error(message), { code }));
  assert.match(text(2), /uprawnień/);
  assert.match(text('PolicyDenied'), /uprawnień/);
  assert.match(text(3), /Zaloguj/);
  assert.match(text('AuthRequired'), /Zaloguj/);
  assert.match(text(9), /Nie znaleziono/);
  assert.match(text(undefined, 'request slaAssignRequest timed out after 5000ms'), /na czas/);
  assert.equal(text(8), 'Nie udało się wykonać operacji. Spróbuj ponownie.');
  for (const code of [2, 3, 4, 6, 8, 9, undefined]) assert.ok(!text(code).includes('sqlite'));
});

test('a throttled bus node says "wstrzymany limitem" in its row and in the header, not "ostrzeżenie"', () => {
  const spec = { name: 'S', maxConcurrency: 4, ordering: 'partition' };
  const status = { state: 'ok', inFlight: 3, maxConcurrency: 4 };
  const consumer = { state: 'running', throttledUntil: 2_000_000 };
  const cells = m.nodeCells({ status, spec, consumer, nowMs: 1_000_000, bus: true });
  assert.equal(cells.state, 'throttled');
  assert.equal(cells.label, 'wstrzymany limitem');
  assert.equal(cells.nodeSub, 'prowadzi odbiór');
  assert.equal(cells.inFlightSub, 'po kolei w partycji');
  assert.equal(m.stateLabel(m.overallState([cells.state])), 'wstrzymany limitem');
  assert.equal(m.overallState(['ok', 'throttled']), 'throttled');
  assert.equal(m.overallState(['throttled', 'warn']), 'warn');
  assert.equal(m.nodeCells({ status: { ...status, state: 'breach' }, spec, consumer, nowMs: 1_000_000, bus: true }).state, 'breach');
  assert.equal(m.nodeCells({ status, spec, consumer, nowMs: 3_000_000, bus: true }).state, 'ok');
});

test('call limits read "na minutę": breach rows and the window names', () => {
  assert.equal(m.windowPer(60), 'minutę');
  assert.equal(m.windowPer(86400), 'dobę');
  const cells = m.breachCells({ kind: 'calls_limit', peakValue: 60, threshold: 60 }, { callWindowSecs: 60 });
  assert.equal(cells.threshold, '60 na minutę');
});

test('the delete tooltip agrees in number: one assignment, several assignments', () => {
  assert.match(m.deleteBlockedText(1), /przypisana do 1 przepływu\. Najpierw zdejmij przypisanie\.$/);
  assert.match(m.deleteBlockedText(3), /do 3 przepływów\. Najpierw zdejmij przypisania\.$/);
});

test('the assign action is off, with its reason, for a system flow, another organization and a missing edit right', () => {
  const flow = { id: 'f', isSystem: false };
  const base = { flow, kind: 'own', canManage: true, canEditFlows: true };
  assert.deepEqual(m.assignAction({ ...base, canManage: false }).show, false);
  assert.equal(m.assignAction(base).disabled, false);
  const system = m.assignAction({ ...base, flow: { ...flow, isSystem: true } });
  assert.deepEqual([system.show, system.disabled], [true, true]);
  assert.match(system.title, /systemowych/);
  const elsewhere = m.assignAction({ ...base, kind: 'elsewhere' });
  assert.deepEqual([elsewhere.disabled, elsewhere.title], [true, 'Politykę nadała inna organizacja']);
  const noRight = m.assignAction({ ...base, canEditFlows: false });
  assert.equal(noRight.disabled, true);
  assert.match(noRight.title, /prawa do edycji przepływów/);
});

test('bounded answers null for a call that fails or is too slow, and the value otherwise', async () => {
  assert.equal(await m.bounded(Promise.resolve(5), 50), 5);
  assert.equal(await m.bounded(Promise.reject(new Error('x')), 50), null);
  assert.equal(await m.bounded(new Promise(() => {}), 10), null);
});

test('only the newest request may draw: a late answer for an earlier one is stale', () => {
  const requests = m.newestRequest();
  const first = requests.begin();
  const second = requests.begin();
  assert.equal(first(), false, 'an answer for flow A after flow B was asked for');
  assert.equal(second(), true);
  requests.cancel();
  assert.equal(second(), false, 'an answer after the tab changed');
});
