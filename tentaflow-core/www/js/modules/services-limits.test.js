import './tentabus/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const l = await import('./services-limits.js');

const dom = (html) => { const root = document.createElement('div'); root.innerHTML = html; return root; };

test('the section is prefilled from the stored config and carries the mockup texts', () => {
  const root = dom(l.renderLimitsSection(l.limitsFromConfig({ max_concurrent: 4, timeout_ms: 120000 })));
  assert.equal(root.querySelector('[data-limit="max_concurrent"]').value, '4');
  assert.equal(root.querySelector('[data-limit="timeout_ms"]').value, '120000');
  assert.match(root.textContent, /Maks\. liczba wątków \(równoległych wykonań\)/);
  assert.match(root.textContent, /Timeout \(ms\)/);
  assert.match(root.textContent, /potem dostają odpowiedź 503 z nagłówkiem Retry-After\. Puste pole = bez limitu\./);
  assert.match(root.textContent, /Limity działają od razu po zapisie, bez restartu serwisu\./);
  const empty = dom(l.renderLimitsSection(l.limitsFromConfig({ max_concurrent: 0 })));
  assert.equal(empty.querySelector('[data-limit="max_concurrent"]').value, '', '0 in the config means no limit');
  assert.equal(empty.querySelector('[data-limit="max_concurrent"]').placeholder, 'bez limitu');
  assert.deepEqual(l.limitsFromConfig(undefined), { maxConcurrent: null, timeoutMs: null });
});

test('only changed limits are sent; an emptied thread limit clears with 0, an empty one stays unsent', () => {
  const initial = { maxConcurrent: 4, timeoutMs: 120000 };
  assert.deepEqual(l.limitsDelta(initial, { max_concurrent: '4', timeout_ms: '120000' }), {});
  assert.deepEqual(l.limitsDelta(initial, { max_concurrent: '8', timeout_ms: '120000' }), { maxConcurrent: 8 });
  assert.deepEqual(l.limitsDelta(initial, { max_concurrent: '', timeout_ms: '120000' }), { maxConcurrent: 0 });
  assert.deepEqual(l.limitsDelta(initial, { max_concurrent: '4', timeout_ms: '5000' }), { timeoutMs: 5000 });
  assert.deepEqual(l.limitsDelta({ maxConcurrent: null, timeoutMs: null }, { max_concurrent: '', timeout_ms: '' }), {});
});

test('the same ranges as the server are checked on the field, with localized text', () => {
  const initial = { maxConcurrent: null, timeoutMs: 120000 };
  assert.deepEqual(l.validateLimits({ max_concurrent: '', timeout_ms: '120000' }, initial), {});
  assert.match(l.validateLimits({ max_concurrent: '0', timeout_ms: '120000' }, initial).max_concurrent, /od 1 do 1024/);
  assert.ok(l.validateLimits({ max_concurrent: '1025', timeout_ms: '120000' }, initial).max_concurrent);
  assert.ok(l.validateLimits({ max_concurrent: '2.5', timeout_ms: '120000' }, initial).max_concurrent);
  assert.deepEqual(Object.keys(l.validateLimits({ max_concurrent: '1', timeout_ms: '99' }, initial)), ['timeout_ms']);
  assert.ok(l.validateLimits({ max_concurrent: '1', timeout_ms: '86400001' }, initial).timeout_ms);
  assert.ok(l.validateLimits({ max_concurrent: '1', timeout_ms: '' }, initial).timeout_ms, 'a timeout that had a value cannot be emptied');
  assert.deepEqual(l.validateLimits({ max_concurrent: '1', timeout_ms: '' }, { maxConcurrent: null, timeoutMs: null }), {});
  const root = dom(l.renderLimitsSection(initial));
  const first = l.showLimitProblems(root, { timeout_ms: 'Błąd' });
  assert.equal(first.dataset.limit, 'timeout_ms');
  assert.equal(first.getAttribute('aria-invalid'), 'true');
  assert.equal(root.querySelector('[data-limit-error="timeout_ms"]').hidden, false);
  l.showLimitProblems(root, {});
  assert.equal(root.querySelector('[data-limit-error="timeout_ms"]').hidden, true);
});

const full = (over = {}) => ({ serviceId: 7, nodeId: 'n', modelRepo: null, modelPresetId: 'p', gpuMemoryUtilization: 0.9, maxModelLen: null, maxNumSeqs: null,
  maxNumBatchedTokens: null, kvCacheDtype: null, chunkedPrefill: true, vllmArgsOverride: null, pinned: null, paused: null, restartAfterSave: true, ...over });

test('a save that changes only limits carries only limits and never asks for a restart', () => {
  const plan = l.savePlan({ full: full(), baseline: full(), delta: { maxConcurrent: 0 } });
  assert.equal(plan.limitsOnly, true);
  assert.equal(plan.payload.maxConcurrent, 0);
  assert.equal(plan.payload.restartAfterSave, false);
  assert.equal(plan.payload.modelPresetId, null);
  assert.equal(plan.payload.gpuMemoryUtilization, null);
  assert.equal('timeoutMs' in plan.payload, false);
  assert.equal(plan.payload.serviceId, 7);
});

test('with another field changed the whole save goes out with the changed limits', () => {
  const plan = l.savePlan({ full: full({ gpuMemoryUtilization: 0.8 }), baseline: full(), delta: { timeoutMs: 5000 } });
  assert.equal(plan.limitsOnly, false);
  assert.equal(plan.payload.timeoutMs, 5000);
  assert.equal(plan.payload.gpuMemoryUtilization, 0.8);
  assert.equal(plan.payload.restartAfterSave, true);
  const none = l.savePlan({ full: full(), baseline: full(), delta: {} });
  assert.equal(none.limitsOnly, false);
  assert.equal('maxConcurrent' in none.payload, false);
});

test('a bad_request is shown against its field in words of ours, never the server text', () => {
  const server = (message) => Object.assign(new Error(message), { code: 'bad_request' });
  const threads = l.limitRefusal(server('max_concurrent must be 0..=1024'));
  assert.equal(threads.field, 'max_concurrent');
  assert.doesNotMatch(threads.message, /must be/);
  assert.equal(l.limitRefusal(server('timeout_ms out of range')).field, 'timeout_ms');
  assert.equal(l.limitRefusal(Object.assign(new Error('x'), { code: 10 })).field, null);
  assert.match(l.limitRefusal(server('weird')).message, /Serwer odrzucił limity/);
  assert.equal(l.limitRefusal(Object.assign(new Error('x'), { code: 8 })), null);
});

test('a service on a node without limit support gets a disabled section with the reason and sends no limits', () => {
  assert.equal(l.limitsEditable({ limitsEditable: false }), false);
  assert.equal(l.limitsEditable({ limitsEditable: true }), true);
  assert.equal(l.limitsEditable({}), true, 'a row that does not carry the flag is left to the server');
  const initial = l.limitsFromConfig({ max_concurrent: 4, timeout_ms: 5000 });
  const root = dom(l.renderLimitsSection(initial, { editable: false }));
  for (const input of root.querySelectorAll('[data-limit]')) assert.ok(input.disabled);
  assert.match(root.querySelector('[data-limits-old-node]').textContent, /ma starszą wersję — zaktualizuj go, aby zmieniać limity\./);
  assert.equal(root.querySelector('[data-limit="max_concurrent"]').value, '4', 'current values stay visible');
  const editable = dom(l.renderLimitsSection(initial));
  assert.equal(editable.querySelector('[data-limits-old-node]'), null);
  assert.ok(!editable.querySelector('[data-limit="max_concurrent"]').disabled);
  assert.deepEqual(l.limitsDelta(initial, l.initialValues(initial)), {}, 'untouched values produce no limits to send');
  assert.deepEqual(l.validateLimits(l.initialValues(initial), initial), {});
});

test('the server\'s "too old" refusal reads as the localized note, not as a field error', () => {
  const refusal = l.limitRefusal(Object.assign(new Error('the hosting node is too old to change limits; upgrade it'), { code: 'bad_request' }));
  assert.equal(refusal.field, null);
  assert.match(refusal.message, /starszą wersję/);
  assert.doesNotMatch(refusal.message, /upgrade it/);
});
