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

const { pickPartition, previewStart, buildBrowseRequest, rangeText, pageNote, scannedUpTo, searchRanOut, openMessagePreview } = await import('./message-preview.js');

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

// ----- Correlation (TB-02.4) -----

const CORR = '0193c7e2-5b1a-7c3e-9a40-2f6d8e1b7a55';
const stamped = (partition, offset, corr) => ({ ...record(partition, offset, `{"n":${offset}}`), headers: corr ? [{ key: 'tf.correlation_id', value: enc(corr) }] : [] });
const PART0 = [{ partition: 0, earliestOffset: 0, highWatermark: 30000 }];

test('the browse request carries the correlation only when one is given', () => {
  assert.equal(buildBrowseRequest('i', 't', 0, 5, 50, CORR).correlationId, CORR);
  assert.equal('correlationId' in buildBrowseRequest('i', 't', 0, 5), false);
  assert.equal('correlationId' in buildBrowseRequest('i', 't', 0, 5, 50, null), false);
});

test('the scan state: nothing found while the partition has more is a search that ran out, not "no results"', () => {
  const more = { nextOffset: 18403, hasMore: true };
  assert.equal(scannedUpTo(more), 18402);
  assert.equal(searchRanOut({ correlation: CORR, records: [], info: more }), true);
  assert.equal(searchRanOut({ correlation: CORR, records: [], info: { nextOffset: 30000, hasMore: false } }), false);
  assert.equal(searchRanOut({ correlation: CORR, records: [{}], info: more }), false);
  assert.equal(searchRanOut({ correlation: null, records: [], info: more }), false);
  assert.match(norm(pageNote(more, 3)), /Pasujące wiadomości: 3\. Przeszukano do numeru 18 402\./);
});

test('the correlation column shows the id shortened with the whole id in its title, and a dash without one', async () => {
  const { win } = open({
    parts: PART0,
    browse: () => ({ records: [stamped(0, 1, CORR), stamped(0, 2, null)], partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 30000, nextOffset: 3, hasMore: true }] }),
  });
  await settle();
  const rows = win.querySelector('[data-role="table"]').rows;
  assert.match(rows[0].correlation, /0193c7e2…7a55/);
  assert.ok(rows[0].correlation.includes(`title="${CORR}"`));
  assert.equal(rows[1].correlation, '—');
});

test('the detail of a stamped message links to its chain in Events', async () => {
  const { win } = open({
    parts: PART0,
    browse: () => ({ records: [stamped(0, 1, CORR)], partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 30000, nextOffset: 2, hasMore: false }] }),
  });
  await settle();
  const link = win.querySelector('[data-role="corr-link"]');
  assert.equal(link.getAttribute('href'), `#/events?correlation=${CORR}`);
  assert.match(link.textContent, /Korelacja 0193c7e2…7a55 → Zdarzenia/);
});

test('the filter reads from the chosen number with the correlation, and clearing it reads without', async () => {
  const { win, requests } = open({ parts: PART0, browse: () => ({ records: [], partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 30000, nextOffset: 30000, hasMore: false }] }) });
  await settle();
  const from = win.querySelector('[data-role="from"]');
  from.value = '18000';
  from.dispatchEvent(new CustomEvent('change', { detail: { value: '18000' } }));
  await settle();
  const corr = win.querySelector('[data-role="corr"]');
  corr.value = ` ${CORR} `;
  corr.dispatchEvent(new CustomEvent('change', { detail: { value: corr.value } }));
  await settle();
  assert.equal(requests.at(-1).correlationId, CORR);
  assert.deepEqual(requests.at(-1).fromOffsets, [{ partition: 0, offset: 18000 }]);
  assert.match(norm(win.querySelector('[data-role="state"]').textContent), /nie ma wiadomości z tą korelacją od numeru 18 000/);
  corr.value = '';
  corr.dispatchEvent(new CustomEvent('change', { detail: { value: '' } }));
  await settle();
  assert.equal('correlationId' in requests.at(-1), false);
});

test('when the search budget runs out the window says how far it got and goes on from the next number', async () => {
  const { win, requests } = open({
    parts: PART0,
    browse: (req) => (req.fromOffsets?.[0]?.offset === 18403
      ? { records: [stamped(0, 18500, CORR)], partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 30000, nextOffset: 18501, hasMore: true }] }
      : { records: [], partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 30000, nextOffset: 18403, hasMore: true }] }),
  });
  await settle();
  const corr = win.querySelector('[data-role="corr"]');
  corr.value = CORR;
  corr.dispatchEvent(new CustomEvent('change', { detail: { value: CORR } }));
  await settle();
  const state = win.querySelector('[data-role="state"]');
  assert.match(norm(state.textContent), /Przeszukano do numeru 18 402/);
  assert.doesNotMatch(state.textContent, /Brak wyników/i);
  const button = state.querySelector('[data-role="scan-on"]');
  assert.match(norm(button.textContent), /Szukaj dalej od 18 403/);
  button.click();
  await settle();
  assert.deepEqual(requests.at(-1).fromOffsets, [{ partition: 0, offset: 18403 }]);
  assert.equal(requests.at(-1).correlationId, CORR);
  assert.equal(win.querySelector('[data-role="from"]').value, '18403');
  assert.equal(win.querySelector('[data-role="table"]').rows.length, 1);
  assert.equal(win.querySelector('[data-role="state"]').querySelector('[data-role="scan-on"]'), null);
});

test('a window opened by the Events deep link reads that place with that correlation at once', async () => {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const requests = [];
  const win = openMessagePreview({
    instanceId: 'tentabus-1',
    topic: 'zgloszenia.nowe',
    partitionCount: 1,
    initial: { correlation: CORR, partition: 2, from: 18402 },
    loadPartitions: async () => [{ partition: 2, earliestOffset: 0, highWatermark: 20000 }],
    browse: async (req) => { requests.push(req); return { records: [stamped(2, 18402, CORR)], partitions: [{ partition: 2, earliestOffset: 0, highWatermark: 20000, nextOffset: 18403, hasMore: false }] }; },
  });
  await settle();
  assert.equal(requests.length, 1, 'the partition list does not move a chosen place');
  assert.equal(requests[0].partition, 2);
  assert.deepEqual(requests[0].fromOffsets, [{ partition: 2, offset: 18402 }]);
  assert.equal(requests[0].correlationId, CORR);
  assert.equal(win.querySelector('[data-role="corr"]').value, CORR);
  assert.equal(win.querySelector('[data-role="from"]').value, '18402');
});

test('the partition picker grows to the topic list when a deep link opened it with fewer', async () => {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const win = openMessagePreview({
    instanceId: 'tentabus-1',
    topic: 'zgloszenia.nowe',
    partitionCount: 1,
    initial: { correlation: CORR, partition: 2, from: 5 },
    loadPartitions: async () => Array.from({ length: 6 }, (_, i) => ({ partition: i, earliestOffset: 0, highWatermark: 100 })),
    browse: async () => ({ records: [], partitions: [{ partition: 2, earliestOffset: 0, highWatermark: 100, nextOffset: 100, hasMore: false }] }),
  });
  await settle();
  const select = win.querySelector('[data-role="partition"]');
  assert.equal(select.value, '2', 'the choice is kept');
  assert.equal(select.querySelectorAll('select option').length, 6);
});

test('while the search budget ran out only "Szukaj dalej" offers to go on, not "Wczytaj więcej"', async () => {
  const { win } = open({
    parts: PART0,
    browse: () => ({ records: [], partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 30000, nextOffset: 18403, hasMore: true }] }),
  });
  await settle();
  const corr = win.querySelector('[data-role="corr"]');
  corr.value = CORR;
  corr.dispatchEvent(new CustomEvent('change', { detail: { value: CORR } }));
  await settle();
  assert.ok(win.querySelector('[data-role="scan-on"]'));
  assert.equal(win.querySelector('[data-role="more"]').hidden, true);
});

test('a deep link to a number retention already removed starts from the oldest kept message, with a note', async () => {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const requests = [];
  const win = openMessagePreview({
    instanceId: 'tentabus-1',
    topic: 'zgloszenia.nowe',
    partitionCount: 1,
    initial: { correlation: CORR, partition: 0, from: 10 },
    loadPartitions: async () => [{ partition: 0, earliestOffset: 500, highWatermark: 900 }],
    browse: async (req) => { requests.push(req); return { records: [], partitions: [{ partition: 0, earliestOffset: 500, highWatermark: 900, nextOffset: 900, hasMore: false }] }; },
  });
  await settle();
  assert.equal(requests.length, 1);
  assert.deepEqual(requests[0].fromOffsets, [{ partition: 0, offset: 500 }]);
  assert.equal(win.querySelector('[data-role="from"]').value, '500');
  assert.match(win.querySelector('[data-role="note"]').textContent, /nie jest już dostępny/);
});

test('a refused read of a deep-linked number is retried once from the oldest message', async () => {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const requests = [];
  const win = openMessagePreview({
    instanceId: 'tentabus-1',
    topic: 'zgloszenia.nowe',
    partitionCount: 1,
    initial: { partition: 0, from: 10 },
    loadPartitions: async () => { throw new Error('unknown'); },
    browse: async (req) => {
      requests.push(req);
      if (req.fromOffsets) throw new Error('offset out of range');
      return { records: [stamped(0, 500, null)], partitions: [{ partition: 0, earliestOffset: 500, highWatermark: 900, nextOffset: 501, hasMore: false }] };
    },
  });
  await settle();
  assert.equal(requests.length, 2);
  assert.equal('fromOffsets' in requests[1], false);
  assert.equal(win.querySelector('[data-role="table"]').rows.length, 1);
  assert.match(win.querySelector('[data-role="note"]').textContent, /nie jest już dostępny/);
});
