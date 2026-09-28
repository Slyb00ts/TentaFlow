// =============================================================================
// File: modules/tentabus/unprocessed.test.js
// Description: Nieprzetworzone wiadomości (U4): a DLQ record read as the
// screen says it (where it sits, its consumer, reason, attempts, a message
// rejected at write); the list of several topics merged newest first without
// placing a record above one not loaded yet; the most common reason; what
// "Ponów wszystkie" would republish; the instance tab (tiles with the count,
// reason and last arrival, "Ponów wszystkie" only for the topic's
// administrator, the merged list and "Wczytaj więcej") and a topic's section
// (row buttons by rights, none of "Ponów" for a message rejected at write,
// who can for a reader, the empty state); the windows say what the server
// does and keep a refusal in the window.
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  unprocessedRecord, mergeNewest, commonReason, retryAllPlan, attemptsText, sourceText, whoCanRetry, receiversText,
  unprocessedTopics, drawUnprocessed, paintUnprocessedSection, LIST_STEP,
} = await import('./unprocessed.js');
const { openUnprocessedView, openRetryOne, openDiscardOne, openRetryAll, retryImpact, retryAllImpact } = await import('./unprocessed-windows.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ').replace(/\s+/g, ' ').trim();
const tick = () => new Promise((r) => setTimeout(r, 20));
const MIN = 60_000;
const NOW = new Date(2026, 8, 23, 15, 0, 0).getTime();
const enc = (s) => Array.from(new TextEncoder().encode(String(s)));
const headers = (o) => Object.entries(o).map(([key, value]) => ({ key, value: enc(value) }));

function consumerFailure({ offset, atMs, group = 'aplikacja-lekarza', reason = 'consumer_error', source = [2, 72373099], payload = 'MSH|x' }) {
  return {
    partition: 0,
    offset,
    timestampMs: atMs - 2 * MIN,
    headers: headers({
      'dlq.source_topic': 'wyniki-badan',
      'dlq.source_partition': source[0],
      'dlq.source_offset': source[1],
      'dlq.group_id': group,
      'dlq.attempts': 5,
      'dlq.first_failed_at_ms': atMs - MIN,
      'dlq.last_failed_at_ms': atMs,
      'dlq.reason': reason,
      'dlq.error_message': 'brak odpowiedzi w ciągu 30 s',
    }),
    payloadPreview: enc(payload),
    isBlobRef: false,
    truncated: false,
  };
}

function writeRejection({ offset, atMs }) {
  return {
    partition: 1,
    offset,
    timestampMs: atMs,
    headers: headers({ 'dlq.source_topic': 'wizyty', 'dlq.reason': 'schema_violation', 'dlq.error_message': '/pacjent: required', 'dlq.rejected_at_ms': atMs }),
    payloadPreview: enc('{"termin":"x"}'),
    isBlobRef: false,
    truncated: false,
  };
}

test('a consumer failure reads as where it sits, who gave up, why and after how many attempts', () => {
  const rec = unprocessedRecord('wyniki-badan', consumerFailure({ offset: 7, atMs: NOW - 5 * MIN }));
  assert.equal(rec.key, 'wyniki-badan\u00000:7');
  assert.equal(rec.group, 'aplikacja-lekarza');
  assert.equal(rec.reason, 'consumer_error');
  assert.equal(rec.atWrite, false);
  assert.equal(rec.arrivalMs, NOW - 5 * MIN, 'arrival is the last failure, as the server orders it');
  assert.equal(norm(sourceText(rec)), 'partycja 2 · numer 72 373 099');
  assert.equal(norm(attemptsText(rec, 5)), '5 z 5');
});

test('a message rejected at write has no consumer and never reached the topic', () => {
  const rec = unprocessedRecord('wizyty', writeRejection({ offset: 3, atMs: NOW - MIN }));
  assert.equal(rec.atWrite, true);
  assert.equal(rec.group, null);
  assert.equal(rec.reason, 'schema_violation');
  assert.equal(rec.arrivalMs, NOW - MIN);
  assert.equal(sourceText(rec), 'nie trafiła do topiku');
  assert.equal(attemptsText(rec, 5), '1 z 1');
});

test('an unknown reason is said as such, never as the raw code', () => {
  const r = consumerFailure({ offset: 1, atMs: NOW, reason: 'something_new' });
  assert.equal(unprocessedRecord('t', r).reason, 'unknown');
});

test('several topics merge newest first; nothing is placed below a record a topic has not loaded yet', () => {
  const a = [NOW - 1 * MIN, NOW - 10 * MIN, NOW - 30 * MIN].map((atMs, i) => unprocessedRecord('a', consumerFailure({ offset: 10 - i, atMs })));
  const b = [NOW - 5 * MIN, NOW - 20 * MIN].map((atMs, i) => unprocessedRecord('b', consumerFailure({ offset: 5 - i, atMs })));
  const all = mergeNewest([{ topic: 'a', records: a, hasMore: false }, { topic: 'b', records: b, hasMore: false }]);
  assert.deepEqual(all.rows.map((r) => `${r.topic}${r.dlqOffset}`), ['a10', 'b5', 'a9', 'b4', 'a8']);
  assert.equal(all.pending, null);
  const held = mergeNewest([{ topic: 'a', records: a, hasMore: false }, { topic: 'b', records: b, hasMore: true }]);
  assert.deepEqual(held.rows.map((r) => `${r.topic}${r.dlqOffset}`), ['a10', 'b5', 'a9', 'b4'], 'a8 is older than b\'s next page could be');
  assert.equal(held.pending, 'b');
});

test('the most common reason, a tie going to the newer', () => {
  const recs = ['consumer_timeout', 'consumer_error', 'consumer_error', 'consumer_timeout'].map((reason, i) => unprocessedRecord('t', consumerFailure({ offset: i, atMs: NOW - i * MIN, reason })));
  assert.equal(commonReason(recs), 'consumer_timeout');
  assert.equal(commonReason([]), null);
});

test('"Ponów wszystkie" leaves messages rejected at write and never counts past what one call does', () => {
  const recs = [
    unprocessedRecord('wizyty', writeRejection({ offset: 5, atMs: NOW })),
    ...[1, 2, 3].map((o) => unprocessedRecord('wizyty', consumerFailure({ offset: o, atMs: NOW - o * MIN }))),
  ];
  assert.deepEqual(retryAllPlan({ total: 4, records: recs, hasMore: false }), { retryable: 3, atWrite: 1, batch: 3, rest: 0, exact: true });
  assert.deepEqual(retryAllPlan({ total: 900, records: recs, hasMore: true }), { retryable: 899, atWrite: 1, batch: 500, rest: 399, exact: false });
});

test('who may retry, and who a republished message reaches', () => {
  assert.equal(whoCanRetry(['Anna Kowalska']), 'Ponawiać i odrzucać wiadomości może administrator topiku (Anna Kowalska).');
  assert.equal(whoCanRetry([]), 'Ponawiać i odrzucać wiadomości może administrator instancji.');
  assert.equal(receiversText(['aplikacja-lekarza']), 'Dostanie ją odbiorca aplikacja-lekarza.');
  assert.match(receiversText(['aplikacja-lekarza', 'raporty-laboratorium']), /wszyscy odbiorcy tego topiku \(aplikacja-lekarza i raporty-laboratorium\) — także ci, którzy już ją przetworzyli/);
  assert.equal(receiversText([]), 'Teraz tego topiku nie czyta żaden odbiorca.');
});

test('only the reader\'s topics with unprocessed messages get a tile, most first', () => {
  const stats = { topics: [
    { topic: 'wizyty', dlqDepth: 6, dlqLastAtMs: NOW },
    { topic: 'wyniki-badan', dlqDepth: 14, dlqLastAtMs: NOW - MIN },
    { topic: 'faktury', dlqDepth: 0 },
    { topic: '__dlq.wizyty', dlqDepth: 0 },
  ] };
  assert.deepEqual(unprocessedTopics(stats).map((t) => t.topic), ['wyniki-badan', 'wizyty']);
});

// ---------------------------------------------------------------------------
// The instance tab
// ---------------------------------------------------------------------------

function mountTab({ byTopic, access, stats, shown = LIST_STEP, notice = null }) {
  const body = document.createElement('div');
  document.body.appendChild(body);
  const moves = [];
  const view = { stats, byTopic, access, shown, notice, error: null, errorKind: null, instanceLabel: 'Produkcja', nowMs: NOW };
  const ctx = { view: () => view, go: (a) => moves.push(a) };
  drawUnprocessed(body, ctx);
  return { body, moves, view, redraw: () => drawUnprocessed(body, ctx) };
}

const tabStats = { topics: [{ topic: 'wyniki-badan', dlqDepth: 3, dlqLastAtMs: NOW - MIN }, { topic: 'wizyty', dlqDepth: 2, dlqLastAtMs: NOW - 2 * MIN }], groups: [] };
function tabData() {
  const results = [1, 3, 5].map((m, i) => unprocessedRecord('wyniki-badan', consumerFailure({ offset: 3 - i, atMs: NOW - m * MIN })));
  const visits = [unprocessedRecord('wizyty', writeRejection({ offset: 9, atMs: NOW - 2 * MIN })), unprocessedRecord('wizyty', consumerFailure({ offset: 1, atMs: NOW - 4 * MIN, group: 'rejestracja-online', reason: 'consumer_timeout' }))];
  return new Map([['wyniki-badan', { records: results, hasMore: false }], ['wizyty', { records: visits, hasMore: false }]]);
}

test('the tab: a tile per topic with its count, reason and last arrival; one list newest first', async () => {
  const access = new Map([['wyniki-badan', { canAdmin: true, adminLabels: [], maxAttempts: 5 }], ['wizyty', { canAdmin: false, adminLabels: ['Anna Kowalska'], maxAttempts: 5 }]]);
  const { body, moves } = mountTab({ byTopic: tabData(), access, stats: tabStats });
  await tick();
  const tiles = [...body.querySelectorAll('.tb-unp-tile')];
  assert.deepEqual(tiles.map((t) => t.dataset.topic), ['wyniki-badan', 'wizyty']);
  assert.equal(tiles[0].querySelector('[data-role="count"]').getAttribute('label'), '3');
  assert.match(norm(tiles[0].querySelector('[data-role="sub"]').textContent), /^najczęściej: program odbiorcy zgłosił błąd · ostatnia dziś \d\d:\d\d$/);
  const retryAll = tiles[0].querySelector('[data-role="retry-all"]');
  assert.equal(retryAll.hidden, false);
  assert.equal(norm(retryAll.textContent), 'Ponów wszystkie (3)');
  assert.equal(tiles[1].querySelector('[data-role="retry-all"]').hidden, true, 'no retry for a topic the reader does not administer');
  assert.match(norm(tiles[1].querySelector('[data-role="who"]').textContent), /administrator topiku \(Anna Kowalska\)/);

  const table = body.querySelector('[data-role="table"]');
  assert.deepEqual(table.rows.map((r) => r._topic), ['wyniki-badan', 'wizyty', 'wyniki-badan', 'wizyty', 'wyniki-badan']);
  assert.match(table.rows[1].reason, /przy zapisie/);
  assert.equal(table.rows[1].consumer, '—');
  assert.match(norm(body.querySelector('[data-role="footer"]').textContent), /Pokazano 5 z 5/);
  assert.equal(body.querySelector('[data-role="list-count"] tf-chip').getAttribute('label'), '5', 'the list says the instance total');
  assert.equal(tiles[0].querySelector('[data-role="count"]').querySelectorAll('tf-chip').length, 0, 'a tile\'s chip carries only its topic\'s count');
  assert.equal(norm(body.querySelector('[data-role="list-hint"]').textContent), 'Kliknij wiersz, aby ponowić albo odrzucić wiadomość w jej topiku.');
  assert.equal(body.querySelector('[data-role="more"]').hidden, true);

  retryAll.click();
  tiles[1].click();
  table.dispatchEvent(new window.CustomEvent('row-click', { detail: { row: table.rows[0], index: 0 } }));
  assert.deepEqual(moves, [{ kind: 'retry-all', topic: 'wyniki-badan' }, { kind: 'topic', topic: 'wizyty' }, { kind: 'topic', topic: 'wyniki-badan' }]);
});

test('a reader who administers no topic is not told to retry', () => {
  const access = new Map([['wyniki-badan', { canAdmin: false, adminLabels: [], maxAttempts: 5 }], ['wizyty', { canAdmin: false, adminLabels: [], maxAttempts: 5 }]]);
  const { body } = mountTab({ byTopic: tabData(), access, stats: tabStats });
  assert.equal(norm(body.querySelector('[data-role="list-hint"]').textContent), 'Kliknij wiersz, aby przejść do nieprzetworzonych wiadomości jego topiku.');
});

test('the tab shows ten rows at a time and offers more while more are known', async () => {
  const many = Array.from({ length: 12 }, (_, i) => unprocessedRecord('wyniki-badan', consumerFailure({ offset: 20 - i, atMs: NOW - i * MIN })));
  const { body } = mountTab({ byTopic: new Map([['wyniki-badan', { records: many, hasMore: false }]]), access: new Map(), stats: { topics: [{ topic: 'wyniki-badan', dlqDepth: 12 }] } });
  await tick();
  assert.equal(body.querySelector('[data-role="table"]').rows.length, 10);
  assert.equal(body.querySelector('[data-role="more"]').hidden, false);
  assert.match(norm(body.querySelector('[data-role="footer"]').textContent), /Pokazano 10 z 12/);
});

test('the tab with nothing unprocessed says so', () => {
  const { body } = mountTab({ byTopic: new Map(), access: new Map(), stats: { topics: [{ topic: 'wizyty', dlqDepth: 0 }] } });
  const empty = body.querySelector('tf-empty-state');
  assert.equal(empty.getAttribute('title'), 'Wszystkie wiadomości są przetworzone');
  assert.match(empty.getAttribute('message'), /Teraz nie ma żadnej\.$/);
  assert.equal(body.querySelector('[data-role="retry-all"]'), null);
});

// ---------------------------------------------------------------------------
// A topic's section
// ---------------------------------------------------------------------------

function mountSection({ canAdmin, records, total = records.length, adminLabels = ['Anna Kowalska'], notice = null }) {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const moves = [];
  const view = {
    topic: { name: 'wizyty', maxDeliveryAttempts: 5 },
    access: { canRead: true, canWrite: canAdmin, canAdmin },
    adminLabels,
    stats: { topics: [{ topic: 'wizyty', dlqDepth: total }], groups: [] },
    notice,
    nowMs: NOW,
    unprocessed: records == null ? null : { records, hasMore: false, error: null },
    shown: LIST_STEP,
  };
  const ctx = { go: (a) => moves.push(a) };
  paintUnprocessedSection(host, view, ctx);
  return { host, moves, view, table: host.querySelector('[data-role="table"]') };
}

const sectionRecords = () => [
  unprocessedRecord('wizyty', writeRejection({ offset: 9, atMs: NOW - 2 * MIN })),
  unprocessedRecord('wizyty', consumerFailure({ offset: 1, atMs: NOW - 4 * MIN, group: 'rejestracja-online' })),
];

test('the section of an administrator: every row can be shown and discarded, only a consumer failure retried', async () => {
  const { host, table, moves } = mountSection({ canAdmin: true, records: sectionRecords() });
  await tick();
  const actsOf = (i) => [...table.rowActions(table.rows[i], i).querySelectorAll('tf-button')].map((b) => b.dataset.act);
  assert.deepEqual(actsOf(0), ['view', 'discard'], 'a message rejected at write is not offered "Ponów"');
  assert.deepEqual(actsOf(1), ['view', 'retry', 'discard']);
  const retryAll = host.querySelector('[data-go="unp-retry-all"]');
  assert.equal(norm(retryAll.textContent), 'Ponów wszystkie (1)');
  assert.equal(host.querySelector('.tb-who-can'), null);
  table.rowActions(table.rows[1], 1).querySelector('[data-act="retry"]').click();
  assert.deepEqual(moves, [{ kind: 'unp-retry', key: 'wizyty\u00000:1' }]);
});

test('the section of a reader: only "Pokaż", no "Ponów wszystkie", and who can', async () => {
  const { host, table } = mountSection({ canAdmin: false, records: sectionRecords() });
  await tick();
  assert.deepEqual([...table.rowActions(table.rows[1], 1).querySelectorAll('tf-button')].map((b) => b.dataset.act), ['view']);
  assert.equal(host.querySelector('[data-go="unp-retry-all"]'), null);
  assert.match(norm(host.querySelector('.tb-who-can').textContent), /Ponawiać i odrzucać wiadomości może administrator topiku \(Anna Kowalska\)\./);
});

test('the section with nothing left says so and offers nothing', () => {
  const { host } = mountSection({ canAdmin: true, records: [], total: 0 });
  assert.equal(host.querySelector('tf-empty-state').getAttribute('message').endsWith('Teraz w tym topiku nie ma żadnej.'), true);
  assert.equal(host.querySelector('[data-go="unp-retry-all"]'), null);
  assert.equal(host.querySelector('[data-role="table"]').hidden, true);
});

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

test('"Pokaż": the failure, the times and the body as the reader may see it; the buttons lead on', () => {
  const rec = unprocessedRecord('wyniki-badan', consumerFailure({ offset: 7, atMs: NOW - 5 * MIN, payload: 'MSH|^~\\&|LIS\rPID|1||80010112345||' }));
  const led = [];
  const win = openUnprocessedView({ rec, maxAttempts: 5, nowMs: NOW, onRetry: () => led.push('retry'), onDiscard: () => led.push('discard') });
  assert.equal(norm(win._titleEl.textContent), 'Nieprzetworzona wiadomość — partycja 2, numer 72 373 099');
  const text = norm(win.textContent);
  assert.match(text, /Opis błędubrak odpowiedzi w ciągu 30 s/);
  assert.match(text, /Próby5 z 5/);
  assert.match(text, /Obowiązują tu te same zasady ukrywania danych/);
  assert.match(win.querySelector('[data-role="payload"]').textContent, /PID\|1\|\|80010112345/);
  win.querySelector('[data-act="retry"]').click();
  assert.deepEqual(led, ['retry']);
});

test('"Pokaż" says the error text is hidden when the topic\'s data-hiding rules blank it for the reader', () => {
  const r = consumerFailure({ offset: 7, atMs: NOW - 5 * MIN });
  r.headers = r.headers
    .map((h) => (h.key === 'dlq.error_message' ? { key: h.key, value: [] } : h))
    .concat(headers({ 'dlq.error_message_hidden': '1' }));
  const rec = unprocessedRecord('wyniki-badan', r);
  assert.equal(rec.errorHidden, true);
  const win = openUnprocessedView({ rec, maxAttempts: 5, nowMs: NOW });
  assert.match(norm(win.textContent), /Opis błęduukryty przez zasady ukrywania danych tego topiku/);
  win.close(true);
});

test('"Pokaż" of a reader has no buttons that change anything', () => {
  const rec = unprocessedRecord('wizyty', writeRejection({ offset: 3, atMs: NOW - MIN }));
  const win = openUnprocessedView({ rec, maxAttempts: 5, nowMs: NOW });
  assert.equal(win.querySelector('[data-act="retry"]'), null);
  assert.equal(win.querySelector('[data-act="discard"]'), null);
  assert.match(norm(win.textContent), /Odbiorcażaden — wiadomość sprawdzono przy zapisie/);
  assert.match(norm(win.textContent), /Szczegół techniczny\/pacjent: required/, 'the validator\'s text is labelled as technical');
  win.close(true);
});

test('what "Ponów" says will happen: the end of the topic, every consumer, off the list, back after the attempts', () => {
  const lines = retryImpact({ topic: 'wyniki-badan', consumers: ['aplikacja-lekarza', 'raporty-laboratorium'], maxAttempts: 5 });
  assert.equal(lines.length, 4);
  assert.match(lines[0], /wróci do topiku wyniki-badan jako nowa, na jego koniec/);
  assert.match(lines[1], /także ci, którzy już ją przetworzyli/);
  assert.match(lines[2], /nie da się jej ponowić drugi raz/);
  assert.match(norm(lines[3]), /po 5 próbach/);
});

test('"Ponów" sends once, closes and reports; a refusal stays in the window', async () => {
  const rec = unprocessedRecord('wyniki-badan', consumerFailure({ offset: 7, atMs: NOW - 5 * MIN }));
  let calls = 0;
  const done = [];
  const win = openRetryOne({
    rec, topic: 'wyniki-badan', consumers: ['aplikacja-lekarza'], maxAttempts: 5, nowMs: NOW,
    retry: async () => { calls += 1; if (calls === 1) throw new Error('bus.dlq_record_handled'); return 1; },
    describeError: () => 'Tę wiadomość już ponowiono albo odrzucono — odśwież listę.',
    onDone: (accepted) => done.push(accepted),
  });
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /Co się stanie po ponowieniu:/);
  win.querySelector('[data-act="go"]').click();
  await tick();
  assert.equal(win.querySelector('[data-role="error"]').hidden, false);
  assert.match(win.querySelector('[data-role="error"]').textContent, /już ponowiono/);
  assert.deepEqual(done, []);
  win.querySelector('[data-act="go"]').click();
  await tick();
  assert.deepEqual(done, [1]);
  assert.equal(calls, 2);
});

test('"Odrzuć" says it cannot be undone and that its consumer will not get it', () => {
  const rec = unprocessedRecord('wyniki-badan', consumerFailure({ offset: 7, atMs: NOW }));
  const win = openDiscardOne({ rec, topic: 'wyniki-badan', discard: async () => {}, describeError: String });
  const text = norm(win.textContent);
  assert.match(text, /nie da się przywrócić ani ponowić — odbiorca aplikacja-lekarza jej już nie dostanie/);
  assert.match(text, /liczba zmaleje o 1/);
  assert.equal(win.querySelector('[data-act="go"]').getAttribute('variant'), 'danger');
  win.close(true);
});

test('"Ponów wszystkie" counts what goes back, what stays and the limit of one call', () => {
  const plan = { retryable: 5, atWrite: 1, batch: 5, rest: 0, exact: true };
  const win = openRetryAll({ topic: 'wizyty', plan, consumers: ['rejestracja-online'], maxAttempts: 5, retryAll: async () => ({ retried: 5, failed: 0 }), describeError: String });
  const text = norm(win.textContent);
  assert.match(text, /Nieprzetworzone wiadomości z topiku wizyty, których odbiorca nie przetworzył \(5\)/);
  assert.match(text, /Zostanie 1 wiadomość, która nie pasowała do wzoru przy zapisie — ponowienie znów by ją odrzuciło\./);
  assert.match(text, /Na liście zostanie 1 wiadomość\./);
  assert.match(text, /najwyżej 500 wiadomości/);
  assert.equal(norm(win.querySelector('[data-act="go"]').textContent), 'Ponów 5 wiadomości');
  win.close(true);
  const big = retryAllImpact({ topic: 'wyniki-badan', consumers: [], plan: { retryable: 900, atWrite: 0, batch: 500, rest: 400, exact: false }, maxAttempts: 5 });
  assert.match(norm(big.join(' ')), /Na liście zostanie 400 wiadomości\./);
});
