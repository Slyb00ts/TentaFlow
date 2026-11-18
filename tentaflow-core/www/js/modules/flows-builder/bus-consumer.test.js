// =============================================================================
// File: modules/flows-builder/bus-consumer.test.js
// Description: Wording of a bus_consume flow's consumer status, shared by the
//       flow list chip, the canvas badge and the inspector banner.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { busConsumerText, busConsumerChipStatus, explainServiceAccountError, busGroupProblem } from './bus-consumer.js';
import { I18n } from '/js/i18n.js';

test('no status, or an unknown state, says nothing', () => {
  assert.equal(busConsumerText(null), null);
  assert.equal(busConsumerText({ state: 'exploded' }), null);
});

test('a refused consumer names its reason and the node it was read on', () => {
  const original = I18n.t;
  // A missing translation comes back as the key itself, so a translated key must differ from it.
  I18n.t = (key, params) => (params ? `T:${key}:${params.node}` : `T:${key}`);
  try {
    const info = busConsumerText({ state: 'refused', reason: 'service_account_missing', nodeId: 'n1' });
    assert.equal(info.state, 'refused');
    assert.equal(info.text, 'T:flows_config.consumer_state_refused');
    assert.equal(
      info.detail,
      'T:flows_config.consumer_state_refused: T:flows_config.consumer_reason_service_account_missing, T:flows_config.consumer_on_node:n1',
    );
  } finally {
    I18n.t = original;
  }
});

test('only a state that needs attention is not neutral', () => {
  // The names <tf-chip> knows; anything else would render as the default pill.
  assert.equal(busConsumerChipStatus('refused'), 'warn');
  assert.equal(busConsumerChipStatus('halted'), 'err');
  assert.equal(busConsumerChipStatus('running'), 'ok');
  assert.equal(busConsumerChipStatus('waiting'), 'info');
  assert.equal(busConsumerChipStatus('nonsense'), 'info');
});

test('a halted consumer shows why it stopped and how to resume, and a long node id is shortened', () => {
  const original = I18n.t;
  I18n.t = (key, params) => (params ? `T:${key}:${params.node}` : `T:${key}`);
  try {
    const info = busConsumerText({
      state: 'halted', detail: 'No such key: nie', nodeId: 'fb2b1369c424e67e9c8063331cb7cb319c88b097e8d4f18a2275df7ef2996217',
    });
    assert.equal(info.reason, 'No such key: nie — T:flows_config.consumer_halted_hint');
    assert.match(info.detail, /T:flows_config\.consumer_on_node:fb2b1369$/);
    assert.ok(!info.detail.includes('fb2b1369c4'), 'the full node id must not reach the text');
  } finally {
    I18n.t = original;
  }
});

test('a service-account refusal is localised with the node label and the key name', () => {
  const original = I18n.t;
  I18n.t = (key, params) => (key === 'flows_config.sa_error.key_no_read' ? `PL ${params.node}/${params.key}/${params.topic}` : key);
  try {
    const message = "protocol error BadRequest: bus_consume node 'Odbior': the service account 'Konto' has no read access to topic 'zrodlo' [sa:key_no_read {\"node\":\"Odbior\",\"key\":\"Konto\",\"topic\":\"zrodlo\"}]";
    assert.equal(explainServiceAccountError(message), 'PL Odbior/Konto/zrodlo');
    // No translation for the kind: the English sentence, without prefix or tag.
    const unknown = "protocol error BadRequest: boom [sa:other {\"a\":1}]";
    assert.equal(explainServiceAccountError(unknown), 'boom');
    // Not a service-account message: only the transport prefix goes.
    assert.equal(explainServiceAccountError('protocol error Internal: db down'), 'db down');
  } finally {
    I18n.t = original;
  }
});

test('the group check follows the broker: 2-121 of lowercase, digits, dot and dash', () => {
  assert.equal(busGroupProblem(''), '');
  assert.equal(busGroupProblem('orders-1.v2'), '');
  for (const bad of ['a', 'zła grupa', 'Orders', '-lead', '__x', 'a'.repeat(122)]) {
    assert.equal(busGroupProblem(bad), 'flows_config.group_invalid', bad);
  }
});
