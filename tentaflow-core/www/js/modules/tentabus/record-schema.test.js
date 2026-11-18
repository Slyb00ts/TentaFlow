// =============================================================================
// File: modules/tentabus/record-schema.test.js
// Description: F4 B6 — the schema a record of a binary topic was encoded under
// (`schemaRefId`): the block under a record (the id, "Pokaż wzór", the note for
// a withheld record), the schema text on demand with its title (a sub-schema of
// the reader's rule or the pattern itself), a schema the server no longer
// serves, and the three places that show it: the message browser, an
// unprocessed message and the "jak widzi…" preview (body as bytes, the schema
// of what the subject would be sent).
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { recordSchemaHtml, wireRecordSchema, schemaBodyHtml, hasSchemaRef } = await import('./record-schema.js');
const { openMessagePreview } = await import('./message-preview.js');
const { openUnprocessedView } = await import('./unprocessed-windows.js');
const { unprocessedRecord } = await import('./unprocessed.js');
const { previewResultHtml, openHidingPreview } = await import('./hiding-preview.js');
const { fieldSource } = await import('./topic-hiding.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const tick = (ms = 0) => new Promise((r) => setTimeout(r, ms));
const settle = async () => { for (let i = 0; i < 6; i += 1) await tick(); };
const closeAll = () => document.querySelectorAll('tf-window').forEach((w) => w.remove());

const SUB = { subject: 'zamowienie', schemaType: 'avro', version: 3, schemaRefId: 4242, derived: true, schemaText: '{"type":"record","name":"Z","fields":[{"name":"numer","type":"string"}]}' };
const FULL = { ...SUB, derived: false, schemaText: '{"type":"record","name":"Z","fields":[]}' };

test('a record with a schema id shows it with "Pokaż wzór"; id 0 is withheld only with an empty body on a binary topic; no field, no block', () => {
  assert.equal(hasSchemaRef({ schemaRefId: 4242 }), true);
  assert.equal(hasSchemaRef({ schemaRefId: 0 }), false);
  assert.equal(hasSchemaRef({}), false);
  const block = document.createElement('div');
  block.innerHTML = recordSchemaHtml({ schemaRefId: 4242 });
  assert.equal(norm(block.querySelector('.tb-record-schema-head span').textContent), 'Wzór wiadomości: numer 4242');
  assert.equal(block.querySelector('[data-act="show-schema"]').textContent, 'Pokaż wzór');
  assert.match(block.textContent, /zakodowana pod-schematem, czyli wzorem bez ukrytych pól/);
  block.innerHTML = recordSchemaHtml({ schemaRefId: 4242 }, { empty: true });
  assert.match(block.textContent, /Zasada nie pokazuje żadnego pola wiadomości/);
  block.innerHTML = recordSchemaHtml({ schemaRefId: 0 }, { empty: true, binary: true });
  assert.match(block.querySelector('[data-role="record-schema-withheld"]').textContent, /Treść wstrzymana/);
  assert.equal(recordSchemaHtml({ schemaRefId: 0 }, { empty: true }), '', 'an empty body of a text topic is just empty');
  assert.equal(recordSchemaHtml({ schemaRefId: 0 }, { empty: false, binary: true }), '', 'a body that is there is not withheld');
  assert.equal(recordSchemaHtml({}, { empty: true, binary: true }), '', 'a server that does not send the id says nothing');
});

test('"Pokaż wzór" asks for the id of the block, titles a sub-schema and the pattern differently, and folds away on the second click', async () => {
  const asked = [];
  const root = document.createElement('div');
  document.body.appendChild(root);
  root.innerHTML = recordSchemaHtml({ schemaRefId: 4242 });
  wireRecordSchema(root, { load: async (id) => { asked.push(id); return asked.length === 1 ? SUB : FULL; }, describeError: String });
  const button = () => root.querySelector('[data-act="show-schema"]');
  button().click();
  await settle();
  assert.deepEqual(asked, [4242]);
  assert.equal(norm(root.querySelector('.tb-record-schema-title').textContent), 'Pod-schemat wzoru zamowienie (Avro, wersja 3) — tylko pola, które zasada pokazuje');
  assert.equal(root.querySelector('[data-role="record-schema-text"]').textContent, SUB.schemaText);
  assert.equal(button().textContent, 'Ukryj wzór');
  button().click();
  await settle();
  assert.equal(root.querySelector('[data-role="record-schema-body"]').innerHTML, '');
  assert.equal(button().textContent, 'Pokaż wzór');
  button().click();
  await settle();
  assert.equal(norm(root.querySelector('.tb-record-schema-title').textContent), 'Wzór zamowienie (Avro, wersja 3)');
  assert.match(schemaBodyHtml(SUB), /Pod-schemat wzoru/);
});

test('a schema the server no longer serves is said in words; any other failure is worded by the screen', async () => {
  const root = document.createElement('div');
  document.body.appendChild(root);
  root.innerHTML = recordSchemaHtml({ schemaRefId: 7 });
  let failure = new Error('protocol error BadRequest: bus.schema_not_found: schema not found');
  wireRecordSchema(root, { load: async () => { throw failure; }, describeError: () => 'Brak uprawnień do tej operacji.' });
  root.querySelector('[data-act="show-schema"]').click();
  await settle();
  assert.match(root.querySelector('[data-role="record-schema-error"]').textContent, /Tego wzoru nie da się już pobrać.*Odczytaj wiadomość jeszcze raz\./);
  assert.equal(root.querySelector('[data-act="show-schema"]').hasAttribute('disabled'), false, 'it can be asked again');
  failure = new Error('bus.permission_denied');
  root.querySelector('[data-act="show-schema"]').click();
  await settle();
  assert.equal(root.querySelector('[data-role="record-schema-error"]').textContent, 'Brak uprawnień do tej operacji.');
});

const enc = (t) => new TextEncoder().encode(t);

test('the message browser shows the schema of a binary record for a reader who may ask for it, and nothing for a server without the id', async () => {
  closeAll();
  const loads = [];
  const win = openMessagePreview({
    instanceId: 'tentabus-1',
    topic: 'zamowienia',
    partitionCount: 1,
    binaryTopic: true,
    loadPartitions: async () => [{ partition: 0, earliestOffset: 0, highWatermark: 3 }],
    loadRecordSchema: async (topic, id) => { loads.push([topic, id]); return SUB; },
    browse: async () => ({
      records: [{ partition: 0, offset: 2, timestampMs: Date.now(), key: enc('k'), payloadPreview: new Uint8Array([0x0a, 0x00, 0xff, 0x01]), headers: [], truncated: false, isBlobRef: false, schemaRefId: 4242 }],
      partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 3, nextOffset: 3, hasMore: false }],
    }),
  });
  await settle();
  assert.ok(win.querySelector('[data-role="payload-binary"]'), 'the body is bytes');
  win.querySelector('[data-act="show-schema"]').click();
  await settle();
  assert.deepEqual(loads, [['zamowienia', 4242]]);
  assert.match(win.querySelector('.tb-record-schema-title').textContent, /^Pod-schemat wzoru zamowienie/);
  closeAll();
  const old = openMessagePreview({
    instanceId: 'tentabus-1', topic: 'wizyty', partitionCount: 1, loadPartitions: async () => [{ partition: 0, earliestOffset: 0, highWatermark: 3 }],
    browse: async () => ({ records: [{ partition: 0, offset: 2, timestampMs: Date.now(), key: enc('k'), payloadPreview: enc('{}'), headers: [], truncated: false, isBlobRef: false }], partitions: [{ partition: 0, earliestOffset: 0, highWatermark: 3, nextOffset: 3, hasMore: false }] }),
  });
  await settle();
  assert.equal(old.querySelector('[data-role="record-schema"]'), null);
});

function dlqRecord({ payload, schemaRefId }) {
  const h = (k, v) => ({ name: k, value: enc(String(v)) });
  return unprocessedRecord('zamowienia', {
    partition: 0,
    offset: 4,
    timestampMs: Date.now(),
    headers: [h('dlq.source_topic', 'zamowienia'), h('dlq.source_partition', 0), h('dlq.source_offset', 9), h('dlq.group_id', 'magazyn'), h('dlq.reason', 'consumer_error'), h('dlq.attempts', 5)],
    payloadPreview: payload,
    isBlobRef: false,
    truncated: false,
    schemaRefId,
  });
}

test('an unprocessed binary record shows its schema through the __dlq topic; a withheld one says so; the other buttons still close the window', async () => {
  closeAll();
  const loads = [];
  const rec = dlqRecord({ payload: new Uint8Array([0x0a, 0x00, 0xff]), schemaRefId: 4242 });
  assert.equal(rec.schemaRefId, 4242);
  const win = openUnprocessedView({ rec, maxAttempts: 5, binaryTopic: true, loadRecordSchema: async (topic, id) => { loads.push([topic, id]); return SUB; }, describeError: String });
  const closed = [];
  win.close = (force) => closed.push(force);
  win.querySelector('[data-act="show-schema"]').click();
  await settle();
  assert.deepEqual(loads, [['__dlq.zamowienia', 4242]]);
  assert.deepEqual(closed, [], 'asking for the schema does not close the window');
  assert.ok(win.querySelector('[data-role="record-schema-text"]'));
  win.querySelector('[data-act="close"]').click();
  assert.deepEqual(closed, [true]);
  const withheld = openUnprocessedView({ rec: dlqRecord({ payload: new Uint8Array(), schemaRefId: 0 }), maxAttempts: 5, binaryTopic: true, loadRecordSchema: async () => SUB });
  assert.match(withheld.querySelector('[data-role="record-schema-withheld"]').textContent, /Treść wstrzymana/);
  const text = openUnprocessedView({ rec: dlqRecord({ payload: new Uint8Array(), schemaRefId: 0 }), maxAttempts: 5, loadRecordSchema: async () => SUB });
  assert.equal(text.querySelector('[data-role="record-schema-withheld"]'), null, 'an empty body of a text topic is not withheld');
});

const ORDER = fieldSource({ format: 'json', schema: { subject: 'zamowienie', schemaType: 'avro', version: 3, fields: ['numer', 'klient'], types: [] }, binaryActions: ['hide'] });
const BINARY_RESPONSE = {
  record: { partition: 0, offset: 5, timestampMs: Date.now(), key: new Uint8Array(), headers: [], payloadPreview: new Uint8Array([0x0a, 0x00, 0xff]), isBlobRef: false, truncated: false, schemaRefId: 4242 },
  applied: [{ field: 'numer', action: 'show' }, { field: 'klient', action: 'hide' }],
  limitedByCaller: false,
};

test('the "jak widzi…" preview of a binary topic shows bytes, a note about the sub-schema and "Pokaż wzór" only where the schema can be asked for', () => {
  const subject = { subjectType: 'group', label: 'Magazyn' };
  const host = document.createElement('div');
  host.innerHTML = previewResultHtml({ resp: BINARY_RESPONSE, subject, source: ORDER, format: 'binary', withSchema: true });
  assert.ok(host.querySelector('[data-role="payload-binary"]'));
  assert.match(host.querySelector('[data-role="binary-note"]').textContent, /Treść binarna jest pokazana jako bajty\. Odbiorca z zasadą dostaje ją zakodowaną pod-schematem/);
  assert.ok(host.querySelector('[data-act="show-schema"]'));
  assert.match(host.querySelector('[data-role="summary"]').textContent, /Zasady ukryły 1 pole: klient\./);
  host.innerHTML = previewResultHtml({ resp: BINARY_RESPONSE, subject, source: ORDER, format: 'binary' });
  assert.equal(host.querySelector('[data-act="show-schema"]'), null);
  const withheld = { ...BINARY_RESPONSE, record: { ...BINARY_RESPONSE.record, payloadPreview: new Uint8Array(), schemaRefId: 0 } };
  host.innerHTML = previewResultHtml({ resp: withheld, subject, source: ORDER, format: 'binary', withSchema: true });
  assert.match(host.querySelector('[data-role="record-schema-withheld"]').textContent, /Treść wstrzymana/);
  host.innerHTML = previewResultHtml({ resp: BINARY_RESPONSE, subject, source: ORDER, format: 'json' });
  assert.equal(host.querySelector('[data-role="binary-note"]'), null, 'a text topic is unchanged');
});

test('in the preview window "Pokaż wzór" asks the topic for the id of the record it shows', async () => {
  closeAll();
  const loads = [];
  const win = openHidingPreview({
    instanceId: 'tentabus-1',
    topic: 'zamowienia',
    partitionCount: 1,
    source: ORDER,
    format: 'binary',
    directory: async () => ({ entries: [{ subjectType: 'group', subjectId: 'g-mag', label: 'Magazyn', memberCount: 2 }] }),
    loadPartitions: async () => [{ partition: 0, earliestOffset: 0, highWatermark: 6 }],
    preview: async () => BINARY_RESPONSE,
    loadRecordSchema: async (topic, id) => { loads.push([topic, id]); return SUB; },
    describeError: String,
  });
  await settle();
  const select = win.querySelector('[data-role="subject"]');
  select.value = 'group:g-mag';
  select.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: 'group:g-mag' } }));
  win.querySelector('[data-act="show"]').click();
  await settle();
  win.querySelector('[data-act="show-schema"]').click();
  await settle();
  assert.deepEqual(loads, [['zamowienia', 4242]]);
  assert.ok(win.querySelector('[data-role="record-schema-text"]'));
});
