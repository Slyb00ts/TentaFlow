// =============================================================================
// File: lib/date-format.test.js
// Description: The shared day helpers: ISO validation and arithmetic, the UI
//   language's display format, typed input parsed back to ISO, and the
//   weekday/month names and first weekday that the calendar takes from Intl.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { I18n } from './actions/_test-setup.js';
import {
  addDays, daysBetween, firstWeekday, formatDay, isIsoDay, monthTitle, parseDay, uiLocale, weekdayLabels,
} from './date-format.js';

// Only the language code matters to the formatters; the catalogue of the switched-to language is not needed.
// `init` (not `setLanguage`) so no backend sync — and no socket — is started.
async function inLanguage(code, run) {
  const realFetch = globalThis.fetch;
  globalThis.fetch = (url, init) => (String(url).endsWith(`/i18n/${code}.json`)
    ? Promise.resolve({ ok: true, json: () => Promise.resolve({}) })
    : realFetch(url, init));
  try {
    localStorage.setItem('tentaflow_lang', code);
    await I18n.init();
    await run();
  } finally {
    localStorage.setItem('tentaflow_lang', 'en');
    await I18n.init();
    globalThis.fetch = realFetch;
  }
}

test('only a real calendar day in YYYY-MM-DD is a day', () => {
  assert.equal(isIsoDay('2026-09-30'), true);
  assert.equal(isIsoDay('2026-02-30'), false);
  assert.equal(isIsoDay('30.09.2026'), false);
  assert.equal(isIsoDay(''), false);
  assert.equal(isIsoDay(undefined), false);
});

test('days move and count in whole calendar days across month, year and clock changes', () => {
  assert.equal(addDays('2026-12-31', 1), '2027-01-01');
  assert.equal(addDays('2026-03-01', -1), '2026-02-28');
  assert.equal(addDays('2026-10-25', 1), '2026-10-26');
  assert.equal(daysBetween('2026-09-30', '2026-11-01'), 32);
  assert.equal(daysBetween('2026-11-01', '2026-09-30'), -32);
  assert.equal(daysBetween('2026-09-30', '2026-09-30'), 0);
});

test('english shows the day-first order, a non-day is shown as it came', () => {
  assert.equal(uiLocale(), 'en-GB');
  assert.equal(formatDay('2026-09-30'), '30/09/2026');
  assert.equal(formatDay('2026-09-30', { short: true }), '30/09');
  assert.equal(formatDay('yesterday'), 'yesterday');
  assert.equal(formatDay(null), '');
});

test('polish and german write 30.09.2026, spanish and french 30/09/2026', async () => {
  await inLanguage('pl', () => {
    assert.equal(formatDay('2026-09-30'), '30.09.2026');
    assert.equal(formatDay('2026-11-01', { short: true }), '01.11');
  });
  await inLanguage('de', () => assert.equal(formatDay('2026-09-30'), '30.09.2026'));
  await inLanguage('es', () => assert.equal(formatDay('2026-09-30'), '30/09/2026'));
  await inLanguage('fr', () => assert.equal(formatDay('2026-09-30'), '30/09/2026'));
});

test('a typed day is read in the language order and always back as ISO', async () => {
  assert.equal(parseDay('30/09/2026'), '2026-09-30');
  assert.equal(parseDay('1/9/2026'), '2026-09-01');
  assert.equal(parseDay('2026-09-30'), '2026-09-30');
  assert.equal(parseDay('31/02/2026'), null);
  assert.equal(parseDay('30/09/26'), null);
  assert.equal(parseDay('tomorrow'), null);
  assert.equal(parseDay(''), null);
  await inLanguage('pl', () => {
    assert.equal(parseDay('30.09.2026'), '2026-09-30');
    assert.equal(parseDay('30 09 2026'), '2026-09-30');
  });
});

test('weekday names, month title and first weekday come from the language', async () => {
  assert.equal(firstWeekday(), 1);
  assert.equal(weekdayLabels().length, 7);
  assert.match(weekdayLabels()[0], /^Mon/);
  assert.equal(monthTitle(2026, 8), 'September 2026');
  await inLanguage('pl', () => {
    assert.match(weekdayLabels()[0], /^pon/i);
    assert.match(monthTitle(2026, 8), /^wrzesień 2026/);
  });
  await inLanguage('de', () => assert.match(monthTitle(2026, 2), /^März 2026/));
});
