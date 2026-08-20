// =============================================================================
// File: utils.test.js
// Description: Unit tests for the analytics number formatters in utils.js
// (fmtCompact thresholds from SPEC D4, fmtExact, fmtCurrency, fmtPct, fmtMs,
// fmtDuration). utils.js has no imports, so no resolver hook is needed.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { fmtCompact, fmtExact, fmtCurrency, fmtPct, fmtMs, fmtDuration } from './utils.js';

// Intl emits narrow/regular no-break spaces as group separators; normalize.
const norm = (s) => s.replace(/[  ]/g, ' ');

test('fmtCompact pl thresholds (SPEC D4)', () => {
  assert.equal(norm(fmtCompact(842, 'pl')), '842');
  assert.equal(norm(fmtCompact(8421, 'pl')), '8 421');
  assert.equal(norm(fmtCompact(9999, 'pl')), '9 999');
  assert.equal(norm(fmtCompact(10000, 'pl')), '10 tys');
  assert.equal(norm(fmtCompact(12400, 'pl')), '12,4 tys');
  assert.equal(norm(fmtCompact(121000000, 'pl')), '121 mln');
  assert.equal(norm(fmtCompact(3200000000, 'pl')), '3,2 mld');
  assert.equal(norm(fmtCompact(52108440, 'pl')), '52,1 mln');
});

test('fmtCompact en', () => {
  assert.equal(norm(fmtCompact(12400, 'en')), '12.4K');
  assert.equal(norm(fmtCompact(121000000, 'en')), '121M');
  assert.equal(norm(fmtCompact(3200000000, 'en')), '3.2B');
  assert.equal(norm(fmtCompact(9999, 'en')), '9,999');
});

test('fmtCompact keeps the sign and drops ",0"', () => {
  assert.equal(norm(fmtCompact(-12400, 'pl')), '-12,4 tys');
  assert.equal(norm(fmtCompact(-121000000, 'en')), '-121M');
  assert.equal(norm(fmtCompact(10000, 'en')), '10K');
  assert.equal(norm(fmtCompact(2000000, 'pl')), '2 mln');
});

test('fmtCompact de/fr/es yields non-empty output with the digits', () => {
  for (const lang of ['de', 'fr', 'es']) {
    for (const [n, digits] of [[12400, '12'], [121000000, '121'], [3200000000, '3']]) {
      const out = fmtCompact(n, lang);
      assert.ok(out.length > 0, `${lang} ${n}`);
      assert.ok(out.includes(digits), `${lang} ${n} -> ${out}`);
      assert.ok(!/\.$/.test(out), `${lang} ${n} -> ${out} ends with a period`);
    }
  }
});

test('fmtCompact non-finite input', () => {
  assert.equal(fmtCompact(NaN, 'pl'), '—');
  assert.equal(fmtCompact(undefined, 'pl'), '—');
});

test('fmtExact', () => {
  assert.equal(norm(fmtExact(52108440, 'pl')), '52 108 440');
  assert.equal(norm(fmtExact(9999, 'pl')), '9 999');
  assert.equal(norm(fmtExact(52108440, 'en')), '52,108,440');
});

test('fmtCurrency', () => {
  assert.equal(norm(fmtCurrency(1044.18, 'PLN', 'pl')), '1 044,18 zł');
  assert.equal(norm(fmtCurrency(1044.18, 'USD', 'en')), '$1,044.18');
});

test('fmtPct', () => {
  assert.equal(fmtPct(0.0008, 2, 'pl'), '0,08%');
  assert.equal(fmtPct(0.123, 1, 'pl'), '12,3%');
  assert.equal(fmtPct(0.5, 1, 'en'), '50%');
});

test('fmtMs', () => {
  assert.equal(fmtMs(204, 'pl'), '204 ms');
  assert.equal(fmtMs(1400, 'pl'), '1,4 s');
  assert.equal(fmtMs(1400, 'en'), '1.4 s');
});

test('fmtDuration', () => {
  assert.equal(fmtDuration(4.1 * 3600e3, 'pl'), '4,1 h');
  assert.equal(fmtDuration(12 * 60e3, 'pl'), '12 min');
  assert.equal(fmtDuration(40e3, 'pl'), '40 s');
});

test('default language falls back to pl without a DOM', () => {
  assert.equal(norm(fmtCompact(12400)), '12,4 tys');
});
