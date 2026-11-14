// =============================================================================
// File: modules/flows-builder/bus-consumer.test.js
// Description: Wording of a bus_consume flow's consumer status, shared by the
//       flow list chip, the canvas badge and the inspector banner.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { busConsumerText, busConsumerChipStatus } from './bus-consumer.js';
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
  assert.equal(busConsumerChipStatus('refused'), 'warning');
  assert.equal(busConsumerChipStatus('halted'), 'danger');
  assert.equal(busConsumerChipStatus('running'), 'success');
  assert.equal(busConsumerChipStatus('nonsense'), 'neutral');
});
