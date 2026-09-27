// =============================================================================
// File: modules/tentabus/offset-move.test.js
// Description: "Przesuń miejsce czytania" (U3): the numbers of one partition,
// the target each of the four ways resolves to, what the move does counted
// exactly (read again / skip / nothing, and what waits afterwards), a typed
// number outside the kept range refused before it reaches the server, the
// time looked up before the count is shown, and the result the window hands
// back (the number the server really set).
// =============================================================================

import { window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;

const {
  partitionPlace, lastRead, lastMessage, keptRange, parseOffsetInput, moveTarget, moveEffect, moveImpact, movedText, modeDescription, openOffsetMove,
} = await import('./offset-move.js');

const norm = (s) => String(s).replace(/[  ]/g, ' ');
const tick = () => new Promise((r) => setTimeout(r, 20));

// The mockup's partition 0: read next 72 115 191, 3 021 waiting, kept from 30 412 118.
const place = partitionPlace({ partition: 0, committedOffset: 72_115_191, lag: 3021, earliestOffset: 30_412_118 });

test('a partition as the window reads it: next number read, next number written, oldest kept', () => {
  assert.deepEqual(place, { partition: 0, committed: 72_115_191, highWatermark: 72_118_212, earliest: 30_412_118, waiting: 3021 });
  assert.equal(lastRead(place), 72_115_190);
  assert.equal(lastMessage(place), 72_118_211);
  const fresh = partitionPlace({ partition: 1, committedOffset: 0, lag: 0, earliestOffset: 0 });
  assert.equal(lastRead(fresh), null);
  assert.equal(lastMessage(fresh), null);
});

test('each way resolves to a number; an unknown time or a bad number is not guessed', () => {
  assert.equal(moveTarget('earliest', place), 30_412_118);
  assert.equal(moveTarget('latest', place), 72_118_212);
  assert.equal(moveTarget('explicit', place, { explicit: 72_114_191 }), 72_114_191);
  assert.equal(moveTarget('explicit', place, { explicit: null }), null);
  assert.equal(moveTarget('timestamp', place, { resolved: null }), null);
  assert.equal(moveTarget('timestamp', place, { resolved: 72_000_000 }), 72_000_000);
});

test('a typed number must be a kept message number', () => {
  assert.equal(parseOffsetInput('72 114 191', place), 72_114_191);
  assert.equal(parseOffsetInput('72118211', place), 72_118_211, 'the newest message');
  assert.equal(parseOffsetInput('72118212', place), null, 'no message has that number yet: "Od najnowszej" starts there');
  assert.deepEqual(keptRange(place), { from: 30_412_118, to: 72_118_211 });
  const empty = partitionPlace({ partition: 3, committedOffset: 40, lag: 0, earliestOffset: 40 });
  assert.equal(keptRange(empty), null);
  assert.equal(parseOffsetInput('40', empty), null, 'a partition that keeps nothing has no number to pick');
  assert.equal(parseOffsetInput('30412117', place), null, 'below the oldest kept message');
  assert.equal(parseOffsetInput('', place), null);
  assert.equal(parseOffsetInput('-5', place), null);
  assert.equal(parseOffsetInput('1e3', place), null);
});

test('what a move does, counted exactly', () => {
  assert.deepEqual(moveEffect(place, 72_114_191), { kind: 'reread', count: 1000, waitingAfter: 4021 });
  assert.deepEqual(moveEffect(place, 72_118_212), { kind: 'skip', count: 3021, waitingAfter: 0 });
  assert.deepEqual(moveEffect(place, 72_116_191), { kind: 'skip', count: 1000, waitingAfter: 2021 });
  assert.deepEqual(moveEffect(place, 72_115_191), { kind: 'same', count: 0, waitingAfter: 3021 });
  assert.equal(moveEffect(place, null), null);
});

test('"Co się stanie" names the consumer, the count, the partition and what waits after', () => {
  const back = moveImpact({ group: 'aplikacja-lekarza', place, target: 72_114_191, paused: false, replicated: false }).map(norm);
  assert.deepEqual(back, [
    'odbiorca aplikacja-lekarza przeczyta ponownie 1 000 wiadomości z partycji 0 (od numeru 72 114 191); w tej partycji czekać będzie 4 021.',
    'Program odbiorcy, który czyta teraz, przejdzie na nowe miejsce przy następnym pobraniu wiadomości, a potwierdzenie wiadomości pobranych przed przesunięciem nie cofnie tej zmiany.',
  ]);
  const all = moveImpact({ group: 'aplikacja-lekarza', place, target: place.highWatermark, paused: true, replicated: false }).map(norm);
  assert.equal(all[0], 'odbiorca aplikacja-lekarza pominie 3 021 zaległych wiadomości z partycji 0 i zacznie od nowych.');
  assert.match(all[1], /wstrzymany — zacznie od nowego miejsca po wznowieniu/);
  const pausedCopies = moveImpact({ group: 'g', place, target: 72_114_191, paused: true, replicated: true }).map(norm);
  assert.ok(pausedCopies.some((l) => /jeśli prowadzenie przejdzie na inny node, odbiorca znów zacznie czytać/.test(l)), 'a pause kept by one node says so when the topic has copies');
  const copies = moveImpact({ group: 'g', place, target: 72_114_191, paused: false, replicated: true }).map(norm);
  assert.match(copies[2], /wróci do czytania co najmniej od numeru 72 115 191/, 'a move back on a replicated topic says what a leader change does');
  const future = moveImpact({ group: 'g', place, target: place.highWatermark, paused: false, replicated: false, timeAfterAll: true }).map(norm);
  assert.match(future[1], /także zapisane przed tą godziną/, 'a time after every message does not pretend to start at that time');
  const forward = moveImpact({ group: 'g', place, target: 72_116_191, paused: false, replicated: true });
  assert.equal(forward.length, 2, 'copies follow a move forward: nothing to warn about');
  assert.match(norm(moveImpact({ group: 'g', place, target: place.committed })[0]), /nic się nie zmieni/);
});

test('the choices carry this partition\'s numbers', () => {
  assert.equal(norm(modeDescription('earliest', place)), 'Odbiorca przeczyta jeszcze raz wszystko, co topik przechowuje (od numeru 30 412 118).');
  assert.equal(norm(modeDescription('latest', place)), 'Odbiorca pominie 3 021 zaległych wiadomości i zacznie od nowych.');
  const upToDate = partitionPlace({ partition: 0, committedOffset: 50, lag: 0, earliestOffset: 0 });
  assert.equal(modeDescription('latest', upToDate), 'Odbiorca jest na bieżąco, więc nic nie pominie.');
});

test('the result line: the number the server set and what waits by the answer after the move', () => {
  assert.equal(norm(movedText({ partition: 0, after: 72_114_191, waiting: 4021 })), 'Partycja 0: odbiorca czyta od numeru 72 114 191; w tej partycji czeka 4 021 wiadomości.');
  assert.equal(norm(movedText({ partition: 2, after: 15, waiting: 1 })), 'Partycja 2: odbiorca czyta od numeru 15; w tej partycji czeka 1 wiadomość.');
  assert.equal(norm(movedText({ partition: 1, after: 7, waiting: null })), 'Partycja 1: odbiorca czyta od numeru 7.');
});

function open(overrides = {}) {
  const calls = { moves: [], lookups: [], done: [] };
  const places = [place, partitionPlace({ partition: 1, committedOffset: 100, lag: 20, earliestOffset: 0 })];
  const win = openOffsetMove({
    group: 'aplikacja-lekarza',
    places,
    partition: 0,
    paused: false,
    replicated: false,
    resolveTimestamp: async (p, tsMs) => { calls.lookups.push([p, tsMs]); return 72_110_000; },
    move: async (req) => { calls.moves.push(req); return req.mode === 'explicit' ? req.offset : 72_110_000; },
    describeError: (e) => e.message,
    onMoved: (r) => calls.done.push(r),
    ...overrides,
  });
  return { win, calls };
}

test('the window: the partition from the row, "now", four ways, the count before confirming', async () => {
  const { win, calls } = open();
  await tick();
  assert.match(win._titleEl.textContent, /aplikacja-lekarza, partycja 0/);
  assert.equal(norm(win.querySelector('[data-role="now"]').textContent), 'przeczytano do numeru 72 115 190 z 72 118 211');
  assert.deepEqual([...win.querySelectorAll('tf-choice-card')].map((c) => c.getAttribute('value')), ['earliest', 'latest', 'explicit', 'timestamp']);
  // "Od wybranego numeru" starts one thousand back: the count is already shown.
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /przeczyta ponownie 1 000 wiadomości z partycji 0 \(od numeru 72 114 191\)/);
  const button = win.querySelector('[data-act="move"]');
  assert.equal(button.hasAttribute('disabled'), false);
  button.click();
  await tick();
  assert.deepEqual(calls.moves, [{ partition: 0, mode: 'explicit', offset: 72_114_191 }]);
  assert.deepEqual(calls.done, [{ partition: 0, after: 72_114_191 }]);
});

test('an answer to an older time lookup never lands after the choice changed', async () => {
  const answers = [];
  const { win, calls } = open({
    resolveTimestamp: (p, tsMs) => new Promise((resolve) => { calls.lookups.push([p, tsMs]); answers.push(resolve); }),
  });
  const mode = (value) => win.querySelector('#tb-move-mode').dispatchEvent(new window.CustomEvent('change', { detail: { value }, bubbles: true }));
  mode('timestamp');
  const time = win.querySelector('#tb-move-time');
  time.value = '2026-09-26T10:00:00';
  time.dispatchEvent(new window.Event('change', { bubbles: true }));
  await tick();
  // Leave the time mode (a lookup that asks nothing), pick another time and come back.
  mode('explicit');
  time.value = '2026-09-27T10:00:00';
  mode('timestamp');
  await tick();
  assert.equal(calls.lookups.length, 2);
  answers[0](72_110_000);
  await tick();
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Szukam pierwszej wiadomości/, 'the answer to the first time is dropped');
  assert.ok(win.querySelector('[data-act="move"]').hasAttribute('disabled'));
  answers[1](72_111_000);
  await tick();
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /od numeru 72 111 000/);
});

test('a number outside the kept range blocks the move with the range', async () => {
  const { win, calls } = open();
  const input = win.querySelector('#tb-move-offset');
  input.value = '999999999';
  input.dispatchEvent(new window.Event('input', { bubbles: true }));
  await tick();
  assert.ok(win.querySelector('[data-act="move"]').hasAttribute('disabled'));
  assert.match(norm(input.getAttribute('error')), /od 30 412 118 do 72 118 211/);
  win.querySelector('[data-act="move"]').click();
  await tick();
  assert.deepEqual(calls.moves, []);
});

test('"Od wybranej chwili" looks the time up first and counts from the answer', async () => {
  const { win, calls } = open();
  win.querySelector('#tb-move-mode').dispatchEvent(new window.CustomEvent('change', { detail: { value: 'timestamp' }, bubbles: true }));
  await tick();
  assert.match(win.querySelector('[data-role="impact"]').textContent, /Wybierz datę i godzinę/);
  assert.ok(win.querySelector('[data-act="move"]').hasAttribute('disabled'));
  const time = win.querySelector('#tb-move-time');
  time.value = '2026-09-26T10:00:00';
  time.dispatchEvent(new window.Event('change', { bubbles: true }));
  await tick();
  assert.equal(calls.lookups.length >= 1, true);
  assert.equal(calls.lookups.at(-1)[0], 0);
  assert.match(norm(win.querySelector('[data-role="impact"]').textContent), /przeczyta ponownie 5 191 wiadomości z partycji 0 \(od numeru 72 110 000\)/);
  win.querySelector('[data-act="move"]').click();
  await tick();
  assert.equal(calls.moves.at(-1).mode, 'timestamp');
  assert.equal(calls.moves.at(-1).tsMs, new Date('2026-09-26T10:00:00').getTime());
});

test('a refusal stays in the window with the reason, and nothing is reported as moved', async () => {
  const { win, calls } = open({ move: async () => { throw new Error('Tego miejsca nie ma już w partycji.'); } });
  win.querySelector('[data-act="move"]').click();
  await tick();
  const err = win.querySelector('[data-role="error"]');
  assert.equal(err.hidden, false);
  assert.match(err.textContent, /nie ma już w partycji/);
  assert.deepEqual(calls.done, []);
});
