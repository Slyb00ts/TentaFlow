// =============================================================================
// File: modules/tentabus/schemas.test.js
// Description: The Wzory wiadomości list: filter counts and membership (a
// withdrawn pattern is "wycofany" even while a topic still validates with it),
// search over names and the topics using a pattern, format names in plain
// words, the rendered table/empty/error states, and what an administrator and
// a reader can do from it: a row opens the pattern, "Dodaj wzór", and a bin
// that works only for a pattern no topic uses (disabled with the topics that
// hold it otherwise).
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { filterSchemas, schemaState, schemaFormatLabel, drawSchemas, deleteBlocker, schemaKind } = await import('./schemas.js');

const subjects = [
  { subject: 'wizyta', schemaType: 'json_schema', compatibility: 'backward', latestVersion: 5, deprecatedAtMs: null, usedByTopics: ['wizyty'] },
  { subject: 'powiadomienie', schemaType: 'protobuf', compatibility: 'none', latestVersion: 1, deprecatedAtMs: null, usedByTopics: [] },
  { subject: 'wizyta-2025', schemaType: 'json_schema', compatibility: 'backward', latestVersion: 7, deprecatedAtMs: 123, usedByTopics: [] },
  { subject: 'faktura', schemaType: 'xsd', compatibility: 'full', latestVersion: 2, deprecatedAtMs: 456, usedByTopics: ['faktury'] },
];

test('state: withdrawn first, then in use, else unused', () => {
  assert.deepEqual(subjects.map(schemaState), ['used', 'unused', 'deprecated', 'deprecated']);
});

test('filters count and select; search matches names and using topics', () => {
  const all = filterSchemas(subjects);
  assert.deepEqual(all.counts, { all: 4, used: 2, deprecated: 2 });
  assert.deepEqual(all.rows.map((s) => s.subject), ['faktura', 'powiadomienie', 'wizyta', 'wizyta-2025']);
  assert.deepEqual(filterSchemas(subjects, { filter: 'used' }).rows.map((s) => s.subject), ['faktura', 'wizyta'], 'a withdrawn pattern a topic still checks with is in use');
  assert.deepEqual(filterSchemas(subjects, { filter: 'deprecated' }).rows.map((s) => s.subject), ['faktura', 'wizyta-2025']);
  assert.deepEqual(filterSchemas(subjects, { query: 'FAKTURY' }).rows.map((s) => s.subject), ['faktura']);
});

test('formats in plain words; an unnamed type prints as sent', () => {
  assert.equal(schemaFormatLabel('json_schema'), 'JSON Schema');
  assert.equal(schemaFormatLabel('hl7v2_profile'), 'profil HL7 v2');
  assert.equal(schemaFormatLabel('cbor'), 'cbor');
});

function mount(view) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  drawSchemas(body, { view: () => ({ instanceLabel: 'Produkcja', error: null, errorKind: null, canAdmin: false, notice: null, ...view }), go: (a) => moves.push(a) });
  return { body, moves };
}

test('list: one row per pattern, filter options with counts', () => {
  const { body } = mount({ subjects });
  const table = body.querySelector('[data-role="table"]');
  assert.equal(table.rows.length, 4);
  assert.match(table.rows[0].state, /wycofany/);
  assert.match(table.rows[2].used, /wizyty/);
  assert.match(table.rows[1].used, /żaden topik/);
  const labels = [...body.querySelectorAll('[data-role="filter"] .tf-seg-opt')].map((b) => b.textContent);
  assert.deepEqual(labels, ['Wszystkie 4', 'W użyciu 2', 'Wycofane 2']);
});

test('empty (T11) and error (T12) states', () => {
  const empty = mount({ subjects: [] });
  assert.equal(empty.body.querySelector('tf-empty-state').getAttribute('title'), 'Nie ma jeszcze wzorów wiadomości');
  const failed = mount({ subjects: null, error: new Error('t'), errorKind: 'timeout' });
  assert.equal(failed.body.querySelector('tf-empty-state').getAttribute('title'), 'Nie udało się wczytać wzorów wiadomości');
  failed.body.querySelector('[data-go="retry"]').click();
  assert.deepEqual(failed.moves, [{ kind: 'retry' }]);
});

test('why a pattern cannot be deleted names the topics holding it', () => {
  assert.equal(deleteBlocker(subjects[1]), null);
  assert.equal(deleteBlocker(subjects[0]), 'Nie można usunąć: używa go topik wizyty. Usuniesz go, gdy w ustawieniach tego topiku wybierzesz inny wzór.');
  assert.equal(deleteBlocker({ usedByTopics: ['b', 'a'] }), 'Nie można usunąć: używają go topiki a i b. Usuniesz go, gdy w ustawieniach tych topików wybierzesz inny wzór.');
  assert.doesNotMatch(deleteBlocker(subjects[0]), /wycofaj/, 'withdrawing never makes a used pattern deletable');
  assert.deepEqual(['json_schema', 'xsd', 'hl7v2_profile', 'thrift', 'cbor'].map(schemaKind), ['json', 'xml', 'hl7v2', 'binary', '']);
});

test('an administrator: "Dodaj wzór", a row opens its pattern, the bin only for an unused one, a lock that answers for a used one', () => {
  const { body, moves } = mount({ subjects, canAdmin: true });
  assert.equal(body.querySelector('.tb-admin-note'), null);
  body.querySelector('[data-go="add"]').click();
  const table = body.querySelector('[data-role="table"]');
  table.dispatchEvent(new CustomEvent('row-click', { detail: { row: table.rows[2] } }));
  const used = table.rowActions(table.rows[2], 2);
  assert.equal(used.querySelector('[data-act="delete"]'), null);
  const lock = used.querySelector('[data-act="delete-blocked"]');
  assert.equal(lock.hasAttribute('disabled'), false, 'a disabled button would keep the reason from a tap and the keyboard');
  assert.equal(lock.getAttribute('icon'), 'lock');
  assert.equal(lock.getAttribute('aria-label'), 'Nie można usunąć: używa go topik wizyty. Usuniesz go, gdy w ustawieniach tego topiku wybierzesz inny wzór.');
  lock.click();
  table.rowActions(table.rows[1], 1).querySelector('[data-act="delete"]').click();
  table.rowActions(table.rows[3], 3).querySelector('[data-act="open"]').click();
  assert.deepEqual(moves, [
    { kind: 'add' },
    { kind: 'open', subject: 'wizyta' },
    { kind: 'delete-blocked', subject: 'wizyta', reason: 'Nie można usunąć: używa go topik wizyty. Usuniesz go, gdy w ustawieniach tego topiku wybierzesz inny wzór.' },
    { kind: 'delete', subject: 'powiadomienie' },
    { kind: 'open', subject: 'wizyta-2025' },
  ]);
});

test('a reader: the list, no "Dodaj wzór" and no bin, one line saying who changes patterns', () => {
  const { body } = mount({ subjects, canAdmin: false });
  assert.equal(body.querySelector('[data-go="add"]'), null);
  const table = body.querySelector('[data-role="table"]');
  assert.equal(table.rowActions(table.rows[1], 1).querySelector('[data-act="delete"]'), null);
  assert.match(body.querySelector('.tb-admin-note').textContent, /administrator instancji/);
  const empty = mount({ subjects: [], canAdmin: false });
  assert.equal(empty.body.querySelector('[data-go="add"]'), null);
});

test('the empty instance leads an administrator to "Dodaj wzór"; a note stays over the list', () => {
  const { body, moves } = mount({ subjects: [], canAdmin: true });
  assert.equal(body.querySelector('tf-empty-state').getAttribute('message'), 'Wzór wiadomości opisuje, jak ma wyglądać wiadomość. Dodaj wzór tutaj albo wydaj systemowi zewnętrznemu klucz z prawem dodawania wzorów.');
  body.querySelector('tf-empty-state [data-go="add"]').click();
  assert.deepEqual(moves, [{ kind: 'add' }]);
  const noted = mount({ subjects, canAdmin: true, notice: { tone: 'success', title: 'Dodano wzór skierowanie', text: 'JSON Schema, wersja 1.' } });
  assert.equal(noted.body.querySelector('[data-role="notice"] tf-alert').getAttribute('title'), 'Dodano wzór skierowanie');
});
