// =============================================================================
// File: modules/tentabus/schemas.test.js
// Description: The Wzory wiadomości list: filter counts and membership (a
// withdrawn pattern is "wycofany" even while a topic still validates with it),
// search over names and the topics using a pattern, format names in plain
// words, and the rendered table/empty/error states.
// =============================================================================

import './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { filterSchemas, schemaState, schemaFormatLabel, drawSchemas } = await import('./schemas.js');

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
  assert.deepEqual(all.counts, { all: 4, used: 1, deprecated: 2 });
  assert.deepEqual(all.rows.map((s) => s.subject), ['faktura', 'powiadomienie', 'wizyta', 'wizyta-2025']);
  assert.deepEqual(filterSchemas(subjects, { filter: 'used' }).rows.map((s) => s.subject), ['wizyta']);
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
  drawSchemas(body, { view: () => ({ instanceLabel: 'Produkcja', error: null, errorKind: null, ...view }), go: (a) => moves.push(a) });
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
  assert.deepEqual(labels, ['Wszystkie 4', 'W użyciu 1', 'Wycofane 2']);
});

test('empty (T11) and error (T12) states', () => {
  const empty = mount({ subjects: [] });
  assert.equal(empty.body.querySelector('tf-empty-state').getAttribute('title'), 'Nie ma jeszcze wzorów wiadomości');
  const failed = mount({ subjects: null, error: new Error('t'), errorKind: 'timeout' });
  assert.equal(failed.body.querySelector('tf-empty-state').getAttribute('title'), 'Nie udało się wczytać wzorów wiadomości');
  failed.body.querySelector('[data-go="retry"]').click();
  assert.deepEqual(failed.moves, [{ kind: 'retry' }]);
});
