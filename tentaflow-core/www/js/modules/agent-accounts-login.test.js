// =============================================================================
// File: modules/agent-accounts-login.test.js
// Description: The A02 sign-in state machine, driven with a fake request layer.
//       The flow lives on a NODE, not in the browser: this file pins what the
//       window may do to it — start once, type a code once, stop polling the
//       moment the node reports a terminal state, and cancel exactly the flows
//       that are still running. It also pins the two translations of a server
//       answer the screens depend on: a refusal sentence turned into the
//       operator's language, and an address from a terminal transcript that is
//       only ever linked when it is http(s).
// =============================================================================

import '/js/sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';

const WWW_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

// The harness serves `file:` URLs (the wasm codec) from disk; the locale files
// are served the same way so the flow reports the strings the app ships.
const harnessFetch = globalThis.fetch;
globalThis.fetch = (url, init) => {
  const match = /^\/i18n\/(\w+)\.json$/.exec(String(url));
  if (!match) return harnessFetch(url, init);
  const text = readFileSync(join(WWW_ROOT, 'i18n', `${match[1]}.json`), 'utf8');
  return Promise.resolve({ ok: true, status: 200, json: () => Promise.resolve(JSON.parse(text)) });
};
if (typeof globalThis.localStorage === 'undefined') {
  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
  };
}

// Switching the language tells Core about the preference, which would open the
// dashboard's WebSocket and leave it reconnecting for as long as the process
// lives. The flow under test never touches this layer — it is handed a fake
// request layer — so the one call i18n makes answers here.
const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
ApiBinary.action = () => Promise.resolve({});

const { I18n } = await import('/js/i18n.js');
await I18n.setLanguage('pl');
const { LoginFlow, safeHttpUrl } = await import('/js/modules/agent-accounts-login.js');
const { describeError } = await import('/js/modules/agent-accounts.js');

const pl = JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', 'pl.json'), 'utf8')).agent_accounts;

/**
 * A stand-in for `AgentAccounts`. Every call is recorded; `status` answers the
 * queued snapshots in order and repeats the last one, and any entry that is an
 * `Error` is thrown the way the binary shim rejects.
 */
function fakeApi({ start = {}, statuses = [], input = null, cancel = null } = {}) {
  const calls = [];
  const queue = [...statuses];
  const answer = (value) => (value instanceof Error ? Promise.reject(value) : Promise.resolve(value));
  return {
    calls,
    loginStart(payload) {
      calls.push({ kind: 'start', payload });
      return answer(start);
    },
    loginInput(loginId, value) {
      calls.push({ kind: 'input', loginId, value });
      return answer(input ?? { ok: true });
    },
    loginStatus(loginId) {
      calls.push({ kind: 'status', loginId });
      return answer(queue.length > 1 ? queue.shift() : queue[0]);
    },
    loginCancel(loginId) {
      calls.push({ kind: 'cancel', loginId });
      return answer(cancel ?? { ok: true });
    },
  };
}

const STARTED = {
  login_id: 'node-a:11111111-2222-3333-4444-555555555555',
  node_id: 'node-a',
  verification_url: 'https://provider.example/device',
  instruction_key: 'agent_accounts.login.instruction.code',
  expires_at: '2026-09-18T10:00:00Z',
};

/** A flow that polls only when the test asks it to. */
async function startedFlow(api, options = {}) {
  const flow = new LoginFlow(api, { pollMs: 60_000 });
  await flow.start({ accountId: 'acc-1', ...options });
  flow.stopPolling();
  return flow;
}

test('a started sign-in carries the address, the instruction and the node it runs on', async () => {
  const api = fakeApi({ start: STARTED });
  const seen = [];
  const flow = new LoginFlow(api, { pollMs: 60_000 });
  flow.onChange((f) => seen.push(f.state));
  await flow.start({ accountId: 'acc-1', nodeId: 'node-a' });
  flow.stopPolling();

  assert.deepEqual(api.calls[0], { kind: 'start', payload: { accountId: 'acc-1', nodeId: 'node-a' } });
  assert.equal(flow.state, 'awaiting_input');
  assert.equal(flow.loginId, STARTED.login_id);
  assert.equal(flow.nodeId, 'node-a');
  assert.equal(flow.verificationUrl, STARTED.verification_url);
  assert.equal(flow.instructionKey, STARTED.instruction_key);
  assert.deepEqual(seen, ['starting', 'awaiting_input']);
  assert.equal(flow.finished, false);
  flow.dispose();
});

test('a second start is ignored while one sign-in is running', async () => {
  const api = fakeApi({ start: STARTED });
  const flow = await startedFlow(api);
  await flow.start({ accountId: 'acc-1' });
  assert.equal(api.calls.filter((c) => c.kind === 'start').length, 1);
  flow.dispose();
});

test('a node that does not receive accounts refuses the start in the operator language', async () => {
  const refusal = new Error(
    'protocol error PolicyDenied: this node is not configured to receive agent accounts',
  );
  const flow = new LoginFlow(fakeApi({ start: refusal }), { pollMs: 60_000 });
  await flow.start({ accountId: 'acc-1' });
  assert.equal(flow.state, 'failed');
  assert.equal(flow.message, pl.login.node_not_receiving);
  assert.equal(flow.finished, true);
  flow.dispose();
});

test('the code is typed once and the flow waits for the node to verify it', async () => {
  const api = fakeApi({ start: STARTED });
  const flow = await startedFlow(api);
  await flow.submit('  ABCD-1234 ');
  assert.deepEqual(api.calls.at(-1), { kind: 'input', loginId: STARTED.login_id, value: 'ABCD-1234' });
  assert.equal(flow.state, 'verifying');

  // Nothing is sent for an empty field, and nothing more once the flow ended.
  await flow.submit('   ');
  flow.state = 'succeeded';
  await flow.submit('ZZZZ');
  assert.equal(api.calls.filter((c) => c.kind === 'input').length, 1);
  flow.dispose();
});

test('a code the terminal refuses keeps the sign-in open with the server message', async () => {
  const api = fakeApi({
    start: STARTED,
    input: new Error('protocol error InvalidRequest: the sign-in is not waiting for input'),
  });
  const flow = await startedFlow(api);
  await flow.submit('1234');
  assert.equal(flow.state, 'awaiting_input');
  assert.equal(flow.message, 'the sign-in is not waiting for input');
  assert.equal(flow.finished, false);
  flow.dispose();
});

test('polling follows the node to the outcome and stops there', async () => {
  const api = fakeApi({
    start: STARTED,
    statuses: [
      { state: 'verifying' },
      { state: 'succeeded', provider_subject: 'ops@example.com', plan_label: 'Max' },
    ],
  });
  const flow = await startedFlow(api);

  await flow.poll();
  assert.equal(flow.state, 'verifying');
  flow.stopPolling();

  await flow.poll();
  assert.equal(flow.state, 'succeeded');
  assert.equal(flow.providerSubject, 'ops@example.com');
  assert.equal(flow.planLabel, 'Max');
  assert.equal(flow.finished, true);

  const polls = api.calls.filter((c) => c.kind === 'status').length;
  await flow.poll();
  assert.equal(api.calls.filter((c) => c.kind === 'status').length, polls, 'a finished flow is not polled again');
  flow.dispose();
});

test('a failed sign-in shows the message key the node sent, translated', async () => {
  const api = fakeApi({
    start: STARTED,
    statuses: [{ state: 'failed', message_key: 'agent_accounts.login.expired' }],
  });
  const flow = await startedFlow(api);
  await flow.poll();
  assert.equal(flow.state, 'failed');
  assert.equal(flow.message, pl.login.expired);
  flow.dispose();
});

test('a sign-in the node no longer knows ends instead of polling forever', async () => {
  const api = fakeApi({ start: STARTED, statuses: [new Error('protocol error NotFound: unknown login')] });
  const flow = await startedFlow(api);
  await flow.poll();
  assert.equal(flow.state, 'failed');
  assert.equal(flow.message, pl.login.lost);
  const polls = api.calls.filter((c) => c.kind === 'status').length;
  await flow.poll();
  assert.equal(api.calls.filter((c) => c.kind === 'status').length, polls);
  flow.dispose();
});

test('a broken transport is retried a few times before the sign-in is given up', async () => {
  const api = fakeApi({ start: STARTED, statuses: [new Error('protocol error Internal: socket closed')] });
  const flow = await startedFlow(api);
  for (let attempt = 0; attempt < 4; attempt += 1) {
    await flow.poll();
    flow.stopPolling();
    assert.equal(flow.state, 'awaiting_input', `attempt ${attempt + 1} keeps the sign-in open`);
  }
  await flow.poll();
  assert.equal(flow.state, 'failed');
  assert.equal(flow.message, 'socket closed');
  flow.dispose();
});

test('cancelling a running sign-in tells the node, and an unstarted one tells nobody', async () => {
  const api = fakeApi({ start: STARTED });
  const flow = await startedFlow(api);
  await flow.cancel();
  assert.deepEqual(api.calls.at(-1), { kind: 'cancel', loginId: STARTED.login_id });
  assert.equal(flow.state, 'cancelled');
  assert.equal(flow.finished, true);

  // A second cancel (the window closing after the button) sends nothing more.
  await flow.cancel();
  assert.equal(api.calls.filter((c) => c.kind === 'cancel').length, 1);
  flow.dispose();

  const untouched = fakeApi({ start: STARTED });
  const fresh = new LoginFlow(untouched, { pollMs: 60_000 });
  await fresh.cancel();
  assert.equal(fresh.state, 'cancelled');
  assert.deepEqual(untouched.calls, []);
  fresh.dispose();
});

test('a disposed flow neither polls nor reports', async () => {
  const api = fakeApi({ start: STARTED, statuses: [{ state: 'succeeded' }] });
  const flow = await startedFlow(api);
  let notified = 0;
  flow.onChange(() => { notified += 1; });
  flow.dispose();
  await flow.poll();
  assert.equal(api.calls.filter((c) => c.kind === 'status').length, 0);
  assert.equal(notified, 0);
});

test('a transport rejection without a message key falls back to the generic error', () => {
  assert.equal(describeError(new Error('')).message, pl.error_unknown);
  assert.deepEqual(describeError(new Error('protocol error PolicyDenied: not your account')), {
    code: 'PolicyDenied',
    message: 'not your account',
  });
  assert.equal(describeError(new Error('the socket is gone')).code, '');
  assert.equal(describeError(new Error('the socket is gone')).message, 'the socket is gone');
});

test('only an http(s) address from the terminal becomes a link', () => {
  assert.equal(safeHttpUrl('https://provider.example/device?code=A1'), 'https://provider.example/device?code=A1');
  assert.equal(safeHttpUrl('http://127.0.0.1:1455/auth'), 'http://127.0.0.1:1455/auth');
  assert.equal(safeHttpUrl('javascript:alert(1)'), '');
  assert.equal(safeHttpUrl('data:text/html,<script>'), '');
  assert.equal(safeHttpUrl('file:///etc/passwd'), '');
  assert.equal(safeHttpUrl('Open https://provider.example/device in a browser'), '');
  assert.equal(safeHttpUrl(''), '');
  assert.equal(safeHttpUrl(null), '');
});
