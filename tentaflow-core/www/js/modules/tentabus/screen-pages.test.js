// The two detail pages of the TentaBus screen drawn from the screen's own contexts, so a reference
// that does not exist in tentabus.js (a ReferenceError inside view() or the loader context) fails here.

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { pageInternals } = await import('../tentabus.js');
const { drawTopicDetail } = await import('./topic-detail.js');
const { drawConsumerDetail } = await import('./consumer-detail.js');
const { state, detailContext, consumerContext, consumerLoaderContext } = pageInternals;

const INSTANCE = 'tentabus-a1b2c3d4';
const topicDetail = {
  topic: { name: 'orders', partitions: 2, retentionMs: 86_400_000, retentionBytesPerPartition: 1024 ** 3, replicationFactor: 1, acks: 'leader', durabilityClass: 'standard', compression: 'lz4', maxDeliveryAttempts: 5, retryBackoffMs: 1000, contentType: 'application/json', schemaId: null, validation: 'off' },
  partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 10, sizeBytes: 1, earliestTimestampMs: 1 }],
  groups: [],
  access: { canRead: true, canWrite: true, canAdmin: true },
  adminLabels: [],
};

test('the topic page draws from the screen context with the instance in it', () => {
  state.instanceId = INSTANCE;
  state.instanceLabel = 'Produkcja';
  state.view = { kind: 'topic-detail', name: 'orders', section: 'state' };
  state.detail = topicDetail;
  const view = detailContext.view();
  assert.equal(view.instanceId, INSTANCE);
  const body = document.createElement('div');
  document.body.append(body);
  drawTopicDetail(body, { view: () => view, go() {} });
  assert.match(body.textContent, /orders/);
});

test('the consumer page and its loader context carry the instance, and the page draws', () => {
  state.instanceId = INSTANCE;
  state.view = { kind: 'consumer-detail', group: 'g', topic: 'orders', section: 'state' };
  state.consumer = {
    detail: { group: 'g', topic: 'orders', commitMode: 'auto_after_success', paused: false, partitions: [{ partition: 0, committedOffset: 1, lag: 2 }] },
    topicDetail,
    dlq: null,
    samples: [],
    sla: null,
  };
  assert.equal(consumerLoaderContext().instanceId, INSTANCE);
  assert.match(consumerLoaderContext().name, /orders/);
  const view = consumerContext.view();
  assert.equal(view.group, 'g');
  const body = document.createElement('div');
  document.body.append(body);
  drawConsumerDetail(body, { view: () => view, go() {} });
  assert.match(body.textContent, /orders/);
});
