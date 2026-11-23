// ===== File: modules/tentabus/replica-note.test.js — the note under a view served from a copy =====

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { fromReplica, replicaNoteHtml } = await import('./replica-note.js');

test('only an answer marked `fromReplica` carries the note, worded for values or for messages', () => {
  assert.equal(fromReplica({ fromReplica: false }, undefined, null), false);
  assert.equal(fromReplica({}, { fromReplica: true }), true);
  assert.equal(replicaNoteHtml('values', false), '');
  assert.match(replicaNoteHtml('values', true), /Wartości z kopii na tym nodzie — mogą się spóźniać do ok\. 1 s\./);
  assert.match(replicaNoteHtml('messages', true), /Wiadomości z kopii na tym nodzie — najnowsze mogą się jeszcze nie pokazać/);
});
