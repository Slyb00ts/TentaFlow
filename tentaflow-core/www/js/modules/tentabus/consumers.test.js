// =============================================================================
// File: modules/tentabus/consumers.test.js
// Description: The Odbiorcy tab (U3): rows from the server's list with the
// snapshot's newer waiting count and paused state, the "Opóźnieni" and
// "Wstrzymani" filters counted the way the overview counts, search by consumer
// or topic, the footer, the commit-mode words, pause/resume offered only where
// the reader may change the consumer, and the empty, loading and failed tab.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const { consumerRows, filterConsumerRows, consumersFooter, commitModeLabel, commitModeHint, drawConsumers } = await import('./consumers.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ');
const tick = () => new Promise((r) => setTimeout(r, 20));
const MIN = 60_000;
const NOW = new Date(2026, 8, 23, 15, 0, 0).getTime();

const groups = [
  { group: 'aplikacja-lekarza', topic: 'wyniki-badan', commitMode: 'auto_after_success', paused: false, updatedAtMs: new Date(2026, 8, 18, 9, 0).getTime(), lagTotal: 100, canAdmin: true },
  { group: 'raporty-laboratorium', topic: 'wyniki-badan', commitMode: 'explicit', paused: false, updatedAtMs: new Date(2026, 8, 2, 9, 0).getTime(), lagTotal: 0, canAdmin: true },
  { group: 'system-rozliczen', topic: 'faktury', commitMode: 'explicit', paused: true, updatedAtMs: new Date(2026, 8, 23, 13, 55).getTime(), lagTotal: 400, canAdmin: true },
  { group: 'panel', topic: 'odczyty', commitMode: 'at_most_once', paused: false, updatedAtMs: null, lagTotal: null, canAdmin: false },
];
const stats = {
  groups: [
    { group: 'aplikacja-lekarza', topic: 'wyniki-badan', lagTotal: 18_420, paused: false, lagRisingSinceMs: NOW - 25 * MIN },
    { group: 'system-rozliczen', topic: 'faktury', lagTotal: 10_800, paused: true },
  ],
};

test('rows take the snapshot\'s newer waiting count and paused state; an unmeasured lag stays unknown', () => {
  const rows = consumerRows({ groups, stats, nowMs: NOW });
  const by = Object.fromEntries(rows.map((r) => [r.group, r]));
  assert.equal(by['aplikacja-lekarza'].waiting, 18_420);
  assert.equal(by['aplikacja-lekarza'].lagging, true);
  assert.equal(by['raporty-laboratorium'].waiting, 0, 'the list\'s own figure when the snapshot does not list it');
  assert.equal(by['system-rozliczen'].paused, true);
  assert.equal(by.panel.waiting, null);
  assert.equal(by.panel.canAdmin, false);
});

test('filters count like the overview: behind = something waits; paused; search by consumer or topic', () => {
  const rows = consumerRows({ groups, stats, nowMs: NOW });
  const all = filterConsumerRows(rows);
  assert.deepEqual(all.counts, { all: 4, delayed: 2, paused: 1 });
  assert.deepEqual(filterConsumerRows(rows, { filter: 'delayed' }).rows.map((r) => r.group), ['aplikacja-lekarza', 'system-rozliczen']);
  assert.deepEqual(filterConsumerRows(rows, { filter: 'paused' }).rows.map((r) => r.group), ['system-rozliczen']);
  assert.deepEqual(filterConsumerRows(rows, { query: 'FAKTURY' }).rows.map((r) => r.group), ['system-rozliczen']);
  assert.deepEqual(consumersFooter(all.rows), { consumers: 4, waiting: 29_220, paused: 1 });
});

test('the ways of confirming, in plain words', () => {
  assert.equal(commitModeLabel('auto_after_success'), 'po udanym przetworzeniu');
  assert.equal(commitModeLabel('explicit'), 'program sam daje znać, że skończył');
  assert.equal(commitModeLabel('at_most_once'), 'od razu przy odczycie');
  assert.equal(commitModeLabel('something'), 'nieznany sposób');
  assert.match(commitModeHint('at_most_once'), /Bez ponownych prób/);
});

function mount(view) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  const full = { groups, error: null, errorKind: null, stats, instanceLabel: 'Produkcja', notice: null, nowMs: NOW, ...view };
  const ctx = { view: () => full, go: (a) => moves.push(a) };
  drawConsumers(body, ctx);
  return { body, ctx, moves, view: full };
}

test('the list: filters with counts, a row per consumer, footer and the three ways explained', async () => {
  const { body } = mount({});
  await tick();
  const labels = [...body.querySelectorAll('[data-role="filter"] .tf-seg-opt')].map((b) => norm(b.textContent));
  assert.deepEqual(labels, ['Wszyscy 4', 'Opóźnieni 2', 'Wstrzymani 1']);
  const table = body.querySelector('[data-role="table"]');
  assert.equal(table.rows.length, 4);
  const first = table.rows[0];
  assert.match(first.group, /aplikacja-lekarza/);
  assert.match(norm(first.waiting), /18 420/);
  assert.equal(first.commit, 'po udanym przetworzeniu');
  assert.equal(first.changed, '18.09.2026');
  assert.equal(table.rows.find((r) => r._group === 'system-rozliczen').changed, 'dziś 13:55');
  assert.match(norm(body.querySelector('[data-role="footer"]').textContent), /4 odbiorcy.*razem czeka 29 220 wiadomości.*wstrzymanych: 1/);
  assert.equal(body.querySelectorAll('.tb-commit-legend .legend-item').length, 3);
  assert.equal(body.querySelector('[data-role="admin-note"]').hidden, true, 'the reader may change some consumers');
});

test('row buttons: pause or resume only where the reader may, and they do not open the row', async () => {
  const { body, moves } = mount({});
  await tick();
  const table = body.querySelector('[data-role="table"]');
  const actionsOf = (group) => {
    const idx = table.rows.findIndex((r) => r._group === group);
    return table.rowActions(table.rows[idx], idx);
  };
  const pause = actionsOf('aplikacja-lekarza').querySelector('[data-act="pause"]');
  assert.equal(pause.textContent, 'Wstrzymaj');
  pause.click();
  assert.deepEqual(moves.at(-1), { kind: 'pause', group: 'aplikacja-lekarza', topic: 'wyniki-badan' });
  const resume = actionsOf('system-rozliczen').querySelector('[data-act="resume"]');
  assert.equal(resume.textContent, 'Wznów');
  resume.click();
  assert.deepEqual(moves.at(-1), { kind: 'resume', group: 'system-rozliczen', topic: 'faktury' });
  assert.equal(actionsOf('panel').querySelector('[data-act="pause"]'), null, 'no rights on that topic: no button');
  table.dispatchEvent(new window.CustomEvent('row-click', { detail: { row: table.rows[0] } }));
  assert.deepEqual(moves.at(-1), { kind: 'open', group: 'aplikacja-lekarza', topic: 'wyniki-badan' });
});

test('a reader who may change none of the consumers is told who can', async () => {
  const { body } = mount({ groups: groups.map((g) => ({ ...g, canAdmin: false })) });
  await tick();
  const note = body.querySelector('[data-role="admin-note"]');
  assert.equal(note.hidden, false);
  assert.match(note.textContent, /administrator topiku/);
});

test('empty, loading and failed tab', () => {
  assert.match(mount({ groups: [] }).body.querySelector('tf-empty-state').getAttribute('title'), /Nikt jeszcze nie czyta/);
  assert.ok(mount({ groups: null }).body.querySelector('tf-spinner'));
  const failed = mount({ groups: null, error: new Error('x'), errorKind: 'lost' });
  failed.body.querySelector('[data-go="retry"]').click();
  assert.deepEqual(failed.moves, [{ kind: 'retry' }]);
});
