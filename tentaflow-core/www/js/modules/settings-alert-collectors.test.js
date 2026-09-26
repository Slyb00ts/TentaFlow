// =============================================================================
// File: modules/settings-alert-collectors.test.js
// Description: The platform admin's list of internal alert collectors: the
// card renders the stored list, sends the edited list as the platform setting
// `tentanas.forward_allowlist`, and words the node's refusal of a bad entry.
// =============================================================================

import './tentanas/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { renderAlertCollectorsCard, bindAlertCollectorsCard, parseAllowlist, ALLOWLIST_SETTING } = await import('./settings-alert-collectors.js');

const flush = () => new Promise((r) => setTimeout(r, 0));
const click = (el) => el.dispatchEvent(new window.MouseEvent('click', { bubbles: true, composed: true }));

function mount(value, answer) {
  const sent = [];
  ApiBinary.action = (kind, payload) => { sent.push({ kind, payload }); return answer(payload); };
  document.body.innerHTML = `<div id="host">${renderAlertCollectorsCard(value)}</div>`;
  const host = document.getElementById('host');
  bindAlertCollectorsCard(host, () => {});
  return { host, sent };
}

test('the stored list is shown, edited and saved as the platform setting', async () => {
  const { host, sent } = mount('[{"entry":"siem.lan","allow_http":true}]', () => ({ applied: 1 }));
  const rows = host.querySelectorAll('.alert-collector-row');
  assert.equal(rows.length, 1);
  assert.equal(rows[0].querySelector('[data-role="entry"]').getAttribute('value'), 'siem.lan');
  assert.ok(rows[0].querySelector('[data-role="http"]').checked);
  click(host.querySelector('#alert-collectors-add'));
  const added = host.querySelectorAll('.alert-collector-row')[1];
  added.querySelector('[data-role="entry"]').value = '10.0.5.0/24';
  added.querySelector('[data-role="port"]').value = '6514';
  click(host.querySelector('#alert-collectors-save'));
  await flush();
  assert.equal(sent[0].kind, 'settingsUpdateRequest');
  const entry = sent[0].payload.entries[0];
  assert.equal(entry.key, ALLOWLIST_SETTING);
  assert.deepEqual(JSON.parse(entry.value), [{ entry: 'siem.lan', allow_http: true }, { entry: '10.0.5.0/24', allow_http: false, port: 6514 }]);
  assert.match(host.textContent, /169\.254\.169\.254/, 'the card says what can never be allowed');
});

test('the node\'s refusal of an entry is worded, not shown raw', async () => {
  const { host } = mount('', () => Promise.reject(new Error('refusal:forward_allowlist_refused')));
  click(host.querySelector('#alert-collectors-add'));
  host.querySelector('[data-role="entry"]').value = '127.0.0.1';
  click(host.querySelector('#alert-collectors-save'));
  await flush(); await flush();
  const err = host.querySelector('#alert-collectors-error');
  assert.ok(!err.hidden);
  assert.match(err.textContent, /loopback, link-local albo adres metadanych chmury/);
  assert.doesNotMatch(err.textContent, /refusal:/);
  assert.deepEqual(parseAllowlist('not json'), [], 'an unreadable setting reads as an empty list');
});
