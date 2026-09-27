// =============================================================================
// File: modules/tentabus/topic-delete.test.js
// Description: "Usuń topik" (T02d): the lines saying what goes with the
// topic (size, unprocessed messages, rules and access entries in one line
// when both exist, the consumers that stop receiving) and what stays; counts
// that could not be read are left out; the danger button unlocks only on the
// exact name, a refusal keeps the window open, success closes it.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { deleteImpact, openTopicDelete } = await import('./topic-delete.js');

const GIB = 1024 ** 3;
const topic = { name: 'wyniki-badan', partitions: 6, schemaId: 'wynik-badania' };
const stats = { topic: 'wyniki-badan', totalBytesOnDisk: 38.2 * GIB, dlqDepth: 14 };
const norm = (s) => String(s).replace(/[  ]/g, ' ');

test('what goes with the topic, in the mockup\'s words', () => {
  const { lost, kept } = deleteImpact({ topic, stats, consumers: ['raporty-laboratorium', 'aplikacja-lekarza'], aclCount: 7, policyCount: 4 });
  assert.deepEqual(lost.map(norm), [
    '38,2 GB wiadomości w 6 partycjach',
    '14 nieprzetworzonych wiadomości',
    'Nieprzetworzone wiadomości są usuwane jako pierwsze — znikną nawet wtedy, gdy samego topiku nie uda się usunąć.',
    '4 zasady ukrywania danych i 7 wpisów dostępu osób, grup i addonów',
    'odbiorcy aplikacja-lekarza i raporty-laboratorium przestaną dostawać wiadomości',
  ]);
  assert.match(kept, /wzór wiadomości wynik-badania oraz klucze API/);
});

test('one consumer, no rules, no pattern; unknown counts are left out', () => {
  const { lost, kept } = deleteImpact({ topic: { name: 'faktury', partitions: 2 }, stats: { dlqDepth: 0, totalBytesOnDisk: 0 }, consumers: ['system-rozliczen'], aclCount: 2, policyCount: null });
  assert.deepEqual(lost.map(norm), [
    '2 puste partycje',
    '2 wpisy dostępu osób, grup i addonów',
    'odbiorca system-rozliczen przestanie dostawać wiadomości',
  ]);
  assert.match(kept, /^Zostają: klucze API/);
  assert.equal(deleteImpact({ topic, stats: null, aclCount: null, policyCount: null }).lost.length, 1);
});

const tick = () => new Promise((r) => setTimeout(r, 0));

function open(overrides = {}) {
  document.querySelectorAll('tf-window').forEach((w) => w.remove());
  const calls = { removed: 0, deleted: 0 };
  const win = openTopicDelete({
    topic,
    stats,
    consumers: ['aplikacja-lekarza'],
    loadCounts: async () => ({ aclCount: 3, policyCount: 0 }),
    remove: async () => { calls.removed += 1; },
    onDeleted: () => { calls.deleted += 1; },
    ...overrides,
  });
  return { win, calls };
}

const retype = (win, value) => {
  const input = win.querySelector('#retype-input');
  input.value = value;
  input.dispatchEvent(new CustomEvent('input', { detail: { value } }));
};
const confirm = (win) => win.dispatchEvent(new CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));

test('the window: counts join after they load, the button waits for the exact name', async () => {
  const { win, calls } = open();
  assert.equal(win.getAttribute('modal'), '');
  assert.match(win.querySelector('[data-action="confirm"]').textContent, /Usuń topik wyniki-badan/);
  await tick();
  assert.match(win.querySelector('[data-role="impact"]').textContent, /3 wpisy dostępu/);
  const btn = win.querySelector('[data-action="confirm"]');
  assert.ok(btn.hasAttribute('disabled'));
  retype(win, 'wyniki');
  assert.ok(btn.hasAttribute('disabled'));
  retype(win, 'wyniki-badan');
  assert.equal(btn.hasAttribute('disabled'), false);
  confirm(win);
  await tick();
  await tick();
  assert.equal(calls.removed, 1);
  assert.equal(calls.deleted, 1);
});

test('a refusal stays in the window with its reason', async () => {
  const { win, calls } = open({ remove: async () => { throw new Error('nope'); }, describeError: () => 'Brak uprawnień do usunięcia topiku.' });
  retype(win, 'wyniki-badan');
  confirm(win);
  await tick();
  await tick();
  assert.equal(calls.deleted, 0);
  assert.equal(win.querySelector('#retype-error').hidden, false);
  assert.match(win.querySelector('#retype-error').textContent, /Brak uprawnień/);
});

test('while the delete is out the window cannot be closed, so its answer is seen', async () => {
  let fail;
  const pending = new Promise((_, reject) => { fail = reject; });
  const { win } = open({ remove: () => pending, describeError: () => 'Node nie odpowiedział.' });
  retype(win, 'wyniki-badan');
  confirm(win);
  await tick();
  win.close();
  win.dispatchEvent(new CustomEvent('action', { detail: { action: 'cancel' } }));
  await new Promise((r) => setTimeout(r, 400));
  assert.equal(win.isConnected, true, 'Escape, the close button and Anuluj wait for the answer');
  assert.ok(win.querySelector('[data-action="cancel"]').hasAttribute('disabled'));
  fail(new Error('timeout'));
  await tick();
  await tick();
  assert.match(win.querySelector('#retype-error').textContent, /Node nie odpowiedział/);
  assert.equal(win.querySelector('[data-action="cancel"]').hasAttribute('disabled'), false);
  win.close();
  await new Promise((r) => setTimeout(r, 400));
  assert.equal(win.isConnected, false, 'once answered it closes again');
});
