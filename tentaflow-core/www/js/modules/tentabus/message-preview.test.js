// =============================================================================
// File: modules/tentabus/message-preview.test.js
// Description: "Podgląd wiadomości" (T02c): the window opens on the busiest
// partition at its newest page, asks the server for exactly one partition
// from a number, shows the range the partition keeps, pages on with
// "Wczytaj więcej", selects the newest message for reading, and words an
// empty page and a refused read.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { pickPartition, previewStart, buildBrowseRequest, rangeText, pageNote, openMessagePreview } = await import('./message-preview.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ');
const enc = (s) => new TextEncoder().encode(s);

test('opens on the partition that holds the most, at its newest page', () => {
  const parts = [
    { partition: 0, earliestOffset: 0, highWatermark: 10 },
    { partition: 1, earliestOffset: 100, highWatermark: 400 },
    { partition: 2, earliestOffset: 0, highWatermark: 300 },
  ];
  assert.equal(pickPartition(parts), 1);
  assert.equal(pickPartition([]), 0);
  assert.equal(previewStart(parts[1], 50), 350);
  assert.equal(previewStart(parts[0], 50), 0, 'never before the oldest kept message');
  assert.equal(previewStart({ partition: 0, earliestOffset: 0, logEndOffset: 80 }, 50), 30);
  assert.equal(previewStart(null), null);
});

test('one partition per request, from a number or from the oldest', () => {
  assert.deepEqual(buildBrowseRequest('tentabus-1', 'wizyty', 2, 70), {
    instanceId: 'tentabus-1', topic: 'wizyty', partition: 2, limit: 50, fromOffsets: [{ partition: 2, offset: 70 }],
  });
  assert.equal('fromOffsets' in buildBrowseRequest('tentabus-1', 'wizyty', 0, null), false);
  assert.equal(buildBrowseRequest('tentabus-1', 'wizyty', 0, 0).fromOffsets[0].offset, 0, 'number 0 is a real start');
});

test('range and page notes in words', () => {
  assert.equal(norm(rangeText({ earliestOffset: 30433233, highWatermark: 72405460 })), 'od 30 433 233 do 72 405 459');
  assert.equal(rangeText({ earliestOffset: 5, highWatermark: 5 }), 'partycja jest pusta');
  assert.equal(norm(pageNote({ hasMore: false, highWatermark: 72405460 })), 'Od najstarszej do najnowszej. Następna wiadomość dostanie numer 72 405 460.');
  assert.match(norm(pageNote({ hasMore: true, nextOffset: 120 })), /Dalej są wiadomości od numeru 120\./);
});

const tick = () => new Promise((r) => setTimeout(r, 0));
const settle = async () => { for (let i = 0; i < 5; i += 1) await tick(); };

function record(partition, offset, text) {
  return { partition, offset, timestampMs: Date.now(), key: enc(`K-${offset}`), payloadPreview: enc(text), headers: [], truncated: false, isBlobRef: false };
}

function open({ browse, parts = [{ partition: 0, earliestOffset: 0, highWatermark: 3 }, { partition: 1, earliestOffset: 0, highWatermark: 60 }] } = {}) {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const requests = [];
  const win = openMessagePreview({
    instanceId: 'tentabus-1',
    topic: 'wyniki-badan',
    partitionCount: parts.length,
    loadPartitions: async () => parts,
    browse: async (req) => { requests.push(req); return browse(req); },
    describeError: () => 'Nie masz prawa czytania topiku wyniki-badan.',
  });
  return { win, requests };
}

test('the window reads the busiest partition from its newest page and shows the newest message', async () => {
  const { win, requests } = open({
    browse: (req) => ({
      records: [record(1, 10, 'MSH|a'), record(1, 11, 'MSH|b')],
      partitions: [{ partition: 1, earliestOffset: 0, highWatermark: 60, nextOffset: 12, hasMore: true }],
    }),
  });
  assert.equal(win.getAttribute('modal'), '');
  await settle();
  assert.equal(requests[0].partition, 1);
  assert.deepEqual(requests[0].fromOffsets, [{ partition: 1, offset: 10 }]);
  assert.equal(win.querySelector('[data-role="from"]').value, '10');
  assert.equal(norm(win.querySelector('[data-role="range"]').textContent), 'od 0 do 59');
  const table = win.querySelector('[data-role="table"]');
  assert.equal(table.rows.length, 2);
  assert.equal(table.rows[1]._class, 'selected');
  assert.equal(win.querySelector('.tb-payload').textContent, 'MSH|b');
  assert.match(win.querySelector('.tb-preview-record-head').textContent, /Wiadomość 11 · partycja 1/);
  assert.equal(win.querySelector('[data-role="more"]').hidden, false);
  win.querySelector('[data-role="more"]').click();
  await settle();
  assert.deepEqual(requests[1].fromOffsets, [{ partition: 1, offset: 12 }]);
  assert.equal(table.rows.length, 4, 'the next page joins the list');
  table.dispatchEvent(new CustomEvent('row-click', { detail: { row: table.rows[0], index: 0 } }));
  assert.equal(win.querySelector('.tb-payload').textContent, 'MSH|a');
});

test('another partition and another start number read again', async () => {
  const { win, requests } = open({ browse: () => ({ records: [], partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 3, nextOffset: 3, hasMore: false }] }) });
  await settle();
  win.querySelector('[data-role="partition"]').dispatchEvent(new CustomEvent('change', { detail: { value: '0' } }));
  await settle();
  assert.equal(requests.at(-1).partition, 0);
  assert.deepEqual(requests.at(-1).fromOffsets, [{ partition: 0, offset: 0 }]);
  const from = win.querySelector('[data-role="from"]');
  from.value = '2';
  from.dispatchEvent(new CustomEvent('change', { detail: { value: '2' } }));
  await settle();
  assert.deepEqual(requests.at(-1).fromOffsets, [{ partition: 0, offset: 2 }]);
  assert.match(win.querySelector('[data-role="state"]').textContent, /nie ma wiadomości od numeru 2/);
});

test('a refused read is said in words', async () => {
  const { win } = open({ browse: () => { throw new Error('bus.permission_denied'); } });
  await settle();
  assert.match(win.querySelector('[data-role="state"]').textContent, /Nie masz prawa czytania/);
  assert.equal(win.querySelector('[data-role="table"]').hidden, true);
});

test('a partition picked before the partition list arrives stays picked', async () => {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  let release;
  const listed = new Promise((r) => { release = r; });
  const requests = [];
  const win = openMessagePreview({
    instanceId: 'tentabus-1',
    topic: 'wyniki-badan',
    partitionCount: 2,
    loadPartitions: () => listed,
    browse: async (req) => { requests.push(req); return { records: [], partitions: [] }; },
  });
  win.querySelector('[data-role="partition"]').dispatchEvent(new CustomEvent('change', { detail: { value: '0' } }));
  await settle();
  release([{ partition: 0, earliestOffset: 0, highWatermark: 3 }, { partition: 1, earliestOffset: 0, highWatermark: 60 }]);
  await settle();
  assert.deepEqual(requests.map((r) => r.partition), [0], 'no read of the busiest partition behind the reader\'s back');
  assert.equal(win.querySelector('[data-role="partition"]').value, '0');
  assert.equal(norm(win.querySelector('[data-role="range"]').textContent), 'od 0 do 2', 'the range line learns partition 0\'s bounds');
});
