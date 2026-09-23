// =============================================================================
// File: modules/agent-accounts-ack.test.js
// Description: What an acknowledgement renders as.
//       `AccountOpAck` carries a message key with BOTH outcomes, and
//       `reportWriteOutcome` — the one reporter every account write goes
//       through (`agent-accounts-window.js`, `my-accounts.js`) — has to prefer
//       it either way. `agent_accounts.purge_incomplete` arrives with
//       `ok: false` and says a plaintext credential could not be removed from
//       the node; `credential_unchanged` and `credential_absent` arrive with
//       `ok: true` and say the write was a NO-OP. Printing the caller's success
//       sentence over any of the three would report something the node did not
//       say: that a token is gone while it is still readable, or that a key was
//       removed from an account that never had one. A refusal that carries no
//       key at all is the fourth combination, and it is the one the node's own
//       contract does not forbid — so it is rendered as a refusal too, rather
//       than throwing on it. The function IS exported, so the shipped code is
//       imported rather than cut out.
// =============================================================================

import '/js/sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';

const WWW_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

// The harness serves `file:` URLs (the wasm codec) from disk; the locale files
// are served the same way so the assertions read the strings the app ships.
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
// lives. The layer under test never touches it, so the one call i18n makes
// answers here.
const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
ApiBinary.action = () => Promise.resolve({});

const { I18n } = await import('/js/i18n.js');
const { reportWriteOutcome } = await import('/js/modules/agent-accounts.js');

// Reading in Polish so the expected strings come from the dictionary the app
// ships rather than from a copy hand-typed into this file; a missing key would
// then surface as `undefined` instead of passing against its own key path.
await I18n.setLanguage('pl');
const DICT = JSON.parse(readFileSync(join(WWW_ROOT, 'i18n', 'pl.json'), 'utf8')).agent_accounts;

/// The toasts the call under test added, in order.
///
/// By COUNT rather than by clearing the container: `utils.js` holds the
/// container element in a module variable, so emptying the body would leave the
/// next toast in a detached node this reader could not see.
function toastsAddedSince(count) {
  return [...document.querySelectorAll('.toast-container .toast')].slice(count);
}

const toastCount = () => document.querySelectorAll('.toast-container .toast').length;

test('a partial purge renders its key as a failure, never the caller’s sentence', () => {
  const before = toastCount();
  reportWriteOutcome(
    { ok: false, message_key: 'agent_accounts.purge_incomplete' },
    'CALLER-SENTENCE-DISCONNECTED',
  );
  const [toast] = toastsAddedSince(before);
  assert.ok(toast, 'the partial purge raised no toast at all');
  assert.equal(toast.textContent, DICT.purge_incomplete);
  assert.ok(
    toast.classList.contains('toast-error'),
    'a credential left on the node was reported as a success',
  );
});

test('a no-op write renders its key too, not the caller’s success sentence', () => {
  for (const [key, sentence] of [
    ['agent_accounts.credential_unchanged', 'CALLER-SENTENCE-KEY-SAVED'],
    ['agent_accounts.credential_absent', 'CALLER-SENTENCE-KEY-CLEARED'],
  ]) {
    const before = toastCount();
    reportWriteOutcome({ ok: true, message_key: key }, sentence);
    const [toast] = toastsAddedSince(before);
    assert.ok(toast, `${key} raised no toast at all`);
    assert.equal(toast.textContent, DICT[key.replace('agent_accounts.', '')], `${key}: the key was not rendered`);
    assert.ok(
      toast.classList.contains('toast-success'),
      `${key}: a no-op was reported as a failure`,
    );
  }
});

test('an ack with no key renders the caller’s own sentence', () => {
  const before = toastCount();
  reportWriteOutcome({ ok: true }, 'CALLER-SENTENCE-APPLIED');
  const [toast] = toastsAddedSince(before);
  assert.ok(toast, 'a write with no key raised no toast at all');
  assert.equal(toast.textContent, 'CALLER-SENTENCE-APPLIED');
  assert.ok(toast.classList.contains('toast-success'));
});

test('a refusal with no key is still a refusal, and says no reason', () => {
  // `ok: false` is the node saying the change landed but something did not
  // complete; the key is the only part of that a screen can render, so an ack
  // without one must not fall through to the caller's success sentence. It must
  // not throw either — `I18n.t(undefined)` does (`i18n.js::lookup` splits the
  // path), and a throw here loses the refusal AND the sentence.
  const before = toastCount();
  reportWriteOutcome({ ok: false }, 'CALLER-SENTENCE-DISCONNECTED');
  const [toast] = toastsAddedSince(before);
  assert.ok(toast, 'a refusal with no key raised no toast at all');
  assert.equal(toast.textContent, DICT.error_unknown);
  assert.ok(
    toast.classList.contains('toast-error'),
    'a refusal with no key was reported as a success',
  );
});
