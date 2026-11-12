// ============ File: flows-builder/process-retry-policy.test.js — retry policy editing rules and the words about attempts ============

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
const { ApiBinary } = await import('../../protocol/api-binary-shim.js');
const { processRetryChange, processRetryProblems, processRetryMode, PROCESS_RETRY_LIMITS } = await import('./bpmn.js');
const { retryAttemptsText } = await import('./process-retry.js');
const { simulationServiceSteps, simulationWaitSteps } = await import('./simulation-service.js');
// I18n.init asks the server for the language preference; no connection is made.
ApiBinary.one = async () => ({});
ApiBinary.action = ApiBinary.one;
localStorage.setItem('tentaflow_lang', 'en');
await I18n.init();
after(async () => { await window.happyDOM.close(); });

test('zero attempts removes the policy and a first attempt starts at values the section shows', () => {
  assert.equal(processRetryChange(null, 'retryAttempts', '0'), null);
  assert.equal(processRetryChange({ maxAttempts: 3, delaySeconds: 30, backoff: 'Fixed' }, 'retryAttempts', '0'), null);
  assert.equal(processRetryChange(null, 'retryAttempts', ''), null);
  assert.deepEqual(processRetryChange(null, 'retryAttempts', '3'), { maxAttempts: 3, delaySeconds: 30, backoff: 'Fixed' });
  assert.deepEqual(processRetryChange({ maxAttempts: 3, delaySeconds: 90, backoff: 'Fixed' }, 'retryAttempts', '4'),
    { maxAttempts: 4, delaySeconds: 90, backoff: 'Fixed' }, 'a change of attempts keeps the other fields');
  assert.equal(processRetryChange(null, 'retryDelaySeconds', '10'), null, 'no policy to change the delay of');
});

test('switching to growing waits adds a factor and a cap and switching back drops them', () => {
  const fixed = { maxAttempts: 3, delaySeconds: 30, backoff: 'Fixed' };
  const grown = processRetryChange(fixed, 'retryBackoff', 'Exponential');
  assert.deepEqual(grown, { maxAttempts: 3, delaySeconds: 30, backoff: { Exponential: { factorPercent: 200, maxDelaySeconds: 480 } } });
  assert.equal(processRetryMode(grown), 'Exponential');
  assert.equal(processRetryChange(grown, 'retryBackoff', 'Exponential'), grown, 'choosing the same pattern changes nothing');
  assert.deepEqual(processRetryChange(grown, 'retryFactorPercent', '150').backoff, { Exponential: { factorPercent: 150, maxDelaySeconds: 480 } });
  assert.deepEqual(processRetryChange(grown, 'retryMaxDelaySeconds', '900').backoff, { Exponential: { factorPercent: 200, maxDelaySeconds: 900 } });
  assert.deepEqual(processRetryChange(grown, 'retryBackoff', 'Fixed'), fixed);
  assert.equal(processRetryChange(fixed, 'retryFactorPercent', '150'), fixed, 'a fixed policy has no factor');
  const long = processRetryChange({ maxAttempts: 2, delaySeconds: 80_000, backoff: 'Fixed' }, 'retryBackoff', 'Exponential');
  assert.equal(long.backoff.Exponential.maxDelaySeconds, PROCESS_RETRY_LIMITS.maxDelay, 'the default cap never leaves the limit');
});

test('every field that breaks a limit is named with the limit it breaks', () => {
  assert.deepEqual(processRetryProblems(null), {});
  assert.deepEqual(processRetryProblems({ maxAttempts: 10, delaySeconds: 86400, backoff: 'Fixed' }), {});
  assert.deepEqual(Object.keys(processRetryProblems({ maxAttempts: 11, delaySeconds: 0, backoff: 'Fixed' })).sort(), ['retryAttempts', 'retryDelaySeconds']);
  const grown = (factorPercent, maxDelaySeconds) => ({ maxAttempts: 2, delaySeconds: 30, backoff: { Exponential: { factorPercent, maxDelaySeconds } } });
  assert.deepEqual(processRetryProblems(grown(200, 600)), {});
  assert.deepEqual(Object.keys(processRetryProblems(grown(100, 600))), ['retryFactorPercent']);
  assert.deepEqual(Object.keys(processRetryProblems(grown(1001, 600))), ['retryFactorPercent']);
  assert.deepEqual(processRetryProblems(grown(200, 29)), { retryMaxDelaySeconds: { min: 30, max: 86400 } });
  assert.deepEqual(Object.keys(processRetryProblems({ maxAttempts: 2, delaySeconds: NaN, backoff: 'Fixed' })), ['retryDelaySeconds']);
});

test('attempts are put into words with the plural forms of the language', async () => {
  const incident = (attempts) => ({ attempts });
  assert.equal(retryAttemptsText({}), null, 'a step without a policy says nothing');
  assert.equal(retryAttemptsText(incident({ made: 1, limit: 5, nextAttemptAtMs: 130_000 }), 100_000),
    'Automatic retry 2 of 5 starts in 30 s.');
  assert.equal(retryAttemptsText(incident({ made: 0, limit: 3, nextAttemptAtMs: 100_000 + 5 * 60_000 }), 100_000),
    'Automatic retry 1 of 3 starts in 5 min.');
  assert.match(retryAttemptsText(incident({ made: 1, limit: 3, nextAttemptAtMs: 50_000 }), 100_000), /starts in 0 s/, 'a past deadline never goes negative');
  assert.equal(retryAttemptsText(incident({ made: 1, limit: 3, nextAttemptAtMs: null })), '1 retry made, up to 3 allowed. Nothing more starts by itself.');
  assert.equal(retryAttemptsText(incident({ made: 3, limit: 3 })), '3 retries made, up to 3 allowed. Nothing more starts by itself.');
  await I18n.setLanguage('pl');
  try {
    const ended = (made) => retryAttemptsText(incident({ made, limit: 10 }));
    assert.match(ended(1), /^Wykonano 1 ponowienie,/);
    assert.match(ended(3), /^Wykonano 3 ponowienia,/);
    assert.match(ended(5), /^Wykonano 5 ponowień,/);
    assert.match(ended(12), /^Wykonano 12 ponowień,/);
    assert.match(ended(22), /^Wykonano 22 ponowienia,/);
  } finally {
    await I18n.setLanguage('en');
  }
});

test('the five languages carry every key of the retry policy', async () => {
  const keys = ['tab_time', 'tab_time_title', 'timeout_hint', 'attempts_pending', 'attempts_ended', 'retry_policy_title', 'retry_attempts',
    'retry_attempts_invalid', 'retry_delay', 'retry_delay_invalid', 'retry_backoff', 'retry_backoff_fixed', 'retry_backoff_exponential',
    'retry_factor', 'retry_factor_invalid', 'retry_max_delay', 'retry_max_delay_invalid', 'retry_policy_hint', 'retry_agent_cost_hint', 'simulation_retry_skipped',
    'event_job_retry_scheduled', 'event_job_retries_exhausted', 'event_job_retry_stopped', 'retry_stopped_initiator_unavailable',
    'retry_stopped_retry_unavailable', 'retry_stopped_not_retryable', 'retry_stopped_engine_error', 'diag_CAMUNDA_RETRY_CYCLE_APPROXIMATED'];
  for (const language of ['de', 'en', 'es', 'fr', 'pl']) {
    const table = JSON.parse(readFileSync(new URL(`../../../i18n/${language}.json`, import.meta.url), 'utf8')).bpmn;
    for (const key of keys) assert.equal(typeof table[key], 'string', `${language} ${key}`);
    assert.ok(!/węzeł|węzł/i.test(JSON.stringify(Object.fromEntries(keys.map((key) => [key, table[key]])))), `${language} says node, not węzeł`);
  }
});

test('a simulation form is told which steps have a policy that the run will not repeat', () => {
  const model = { nodes: [
    { id: 'Plain', kind: { ServiceTask: { flowId: 'f', verification: 'Human' } } },
    { id: 'Policy', kind: { ServiceTask: { flowId: 'f', verification: 'Human', retry: { maxAttempts: 2, delaySeconds: 5, backoff: 'Fixed' } } } },
  ] };
  const steps = simulationServiceSteps(model);
  assert.deepEqual(steps.map((step) => step.hasRetryPolicy ?? false), [false, true]);
  const [{ step }] = simulationWaitSteps([{ jobId: 'j', nodeId: 'Policy', verdictApplicable: false }], model);
  assert.equal(step.hasRetryPolicy, true);
});
