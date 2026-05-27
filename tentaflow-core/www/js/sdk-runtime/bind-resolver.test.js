// =============================================================================
// Plik: sdk-runtime/bind-resolver.test.js
// Opis: Testy jednostkowe dla bind-resolver (Krok 3.2). Pokrywają
// `resolveBindRef`, `subscribeBindRef`, `subscribeBindSpec`, `readBindSpec`,
// `formatValue` dla wszystkich 10 wariantów ValueFormat.
// =============================================================================

import { StateStore } from './state-store.js';
import {
  resolveBindRef,
  subscribeBindRef,
  subscribeBindSpec,
  readBindSpec,
  bindSpecPaths,
  formatValue,
} from './bind-resolver.js';

// ---- harness ----

const results = [];
function test(name, fn) {
  try {
    fn();
    results.push({ name, ok: true });
  } catch (err) {
    results.push({ name, ok: false, err });
  }
}
function assertEq(actual, expected, msg) {
  const a = JSON.stringify(actual, (_k, v) =>
    typeof v === 'bigint' ? `${v}n` : v
  );
  const b = JSON.stringify(expected, (_k, v) =>
    typeof v === 'bigint' ? `${v}n` : v
  );
  if (a !== b) {
    throw new Error(`${msg || 'assertEq'}: expected ${b}, got ${a}`);
  }
}
function assert(cond, msg) {
  if (!cond) throw new Error(msg || 'assert failed');
}
function assertThrows(fn, msg) {
  let threw = false;
  try {
    fn();
  } catch {
    threw = true;
  }
  if (!threw) throw new Error(msg || 'expected throw');
}
function assertContains(s, sub, msg) {
  if (typeof s !== 'string' || !s.includes(sub)) {
    throw new Error(`${msg || 'assertContains'}: expected "${s}" to contain "${sub}"`);
  }
}

// ---- helpery ----

const PATH = (...segs) =>
  segs.map((s) =>
    typeof s === 'number'
      ? { kind: 'index', value: s }
      : { kind: 'key', value: s }
  );

function newStore() {
  return new StateStore({ addon_id: 'test', panel_id: 'p', panel_epoch: 1n });
}

// ============================================================================
// BindRef
// ============================================================================

test('resolveBindRef literal returns value directly', () => {
  const store = newStore();
  assertEq(resolveBindRef({ kind: 'literal', value: 42 }, store), 42);
  assertEq(resolveBindRef({ kind: 'literal', value: 'hi' }, store), 'hi');
  assertEq(resolveBindRef({ kind: 'literal', value: null }, store), null);
});

test('resolveBindRef bound reads from store', () => {
  const store = newStore();
  store.applySnapshot({
    entries: [{ path: PATH('user', 'name'), value: 'Ada' }],
    state_revision: 1,
    truncated: false,
  });
  assertEq(
    resolveBindRef({ kind: 'bound', path: PATH('user', 'name') }, store),
    'Ada'
  );
});

test('resolveBindRef bound returns undefined for missing path', () => {
  const store = newStore();
  store.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  assertEq(
    resolveBindRef({ kind: 'bound', path: PATH('missing') }, store),
    undefined
  );
});

test('resolveBindRef rejects bad shape', () => {
  const store = newStore();
  assertThrows(() => resolveBindRef(null, store));
  assertThrows(() => resolveBindRef({}, store));
  assertThrows(() => resolveBindRef({ kind: 'future' }, store));
  assertThrows(() => resolveBindRef({ kind: 'literal' }, store));
  assertThrows(() => resolveBindRef({ kind: 'bound', path: {} }, store));
});

test('resolveBindRef literal rejects extra keys (strict shape mirror Rust)', () => {
  const store = newStore();
  assertThrows(() => resolveBindRef({ kind: 'literal', value: 1, path: PATH('a') }, store));
  assertThrows(() => resolveBindRef({ kind: 'literal', value: 1, extra: true }, store));
});

test('resolveBindRef bound rejects extra keys (strict shape mirror Rust)', () => {
  const store = newStore();
  assertThrows(() => resolveBindRef({ kind: 'bound', path: PATH('a'), value: 1 }, store));
  assertThrows(() => resolveBindRef({ kind: 'bound', path: PATH('a'), extra: true }, store));
});

test('subscribeBindRef literal returns noop unsub', () => {
  const store = newStore();
  let hits = 0;
  const off = subscribeBindRef(
    { kind: 'literal', value: 7 },
    store,
    () => hits++
  );
  store.applySnapshot({
    entries: [{ path: PATH('a'), value: 1 }],
    state_revision: 0,
    truncated: false,
  });
  assertEq(hits, 0);
  off();
});

test('subscribeBindRef bound fires on path change', () => {
  const store = newStore();
  store.applySnapshot({
    entries: [{ path: PATH('a'), value: 1 }],
    state_revision: 0,
    truncated: false,
  });
  let hits = 0;
  subscribeBindRef({ kind: 'bound', path: PATH('a') }, store, () => hits++);
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 2 } }],
  });
  assertEq(hits, 1);
});

// ============================================================================
// BindSpec
// ============================================================================

test('bindSpecPaths returns [path] for every variant', () => {
  for (const kind of ['text', 'attr', 'class_toggle', 'show', 'list', 'two_way']) {
    const spec = { kind, path: PATH('x') };
    if (kind === 'attr') spec.name = 'href';
    if (kind === 'class_toggle') {
      spec.class_name = 'on';
      spec.negate = false;
    }
    if (kind === 'show') spec.negate = false;
    if (kind === 'list') spec.item_template_id = 'tpl';
    assertEq(bindSpecPaths(spec), [PATH('x')]);
  }
});

test('readBindSpec reads value under spec.path', () => {
  const store = newStore();
  store.applySnapshot({
    entries: [{ path: PATH('count'), value: 5 }],
    state_revision: 0,
    truncated: false,
  });
  const v = readBindSpec({ kind: 'text', path: PATH('count') }, store);
  assertEq(v, 5);
});

test('subscribeBindSpec passes fresh value to callback', () => {
  const store = newStore();
  store.applySnapshot({
    entries: [{ path: PATH('count'), value: 5 }],
    state_revision: 0,
    truncated: false,
  });
  const observed = [];
  subscribeBindSpec({ kind: 'text', path: PATH('count') }, store, (v) =>
    observed.push(v)
  );
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('count'), op: { kind: 'set', value: 6 } }],
  });
  assertEq(observed, [6]);
});

test('subscribeBindSpec unsubscribe stops callback', () => {
  const store = newStore();
  store.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  let hits = 0;
  const off = subscribeBindSpec(
    { kind: 'text', path: PATH('a') },
    store,
    () => hits++
  );
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 'x' } }],
  });
  off();
  store.applyPatch({
    base_revision: 1,
    new_revision: 2,
    ops: [{ path: PATH('a'), op: { kind: 'set', value: 'y' } }],
  });
  assertEq(hits, 1);
});

test('readBindSpec rejects bad shape', () => {
  const store = newStore();
  assertThrows(() => readBindSpec({}, store));
  assertThrows(() => readBindSpec({ kind: 'future', path: PATH('x') }, store));
  assertThrows(() => readBindSpec({ kind: 'text', path: {} }, store));
});

// ============================================================================
// ValueFormat — wszystkie 10 wariantów
// ============================================================================

test('formatValue plain stringifies primitives', () => {
  assertEq(formatValue(42, { kind: 'plain' }), '42');
  assertEq(formatValue('hi', { kind: 'plain' }), 'hi');
  assertEq(formatValue(true, { kind: 'plain' }), 'true');
});

test('formatValue plain stringifies BigInt without n suffix', () => {
  assertEq(formatValue(123n, { kind: 'plain' }), '123');
});

test('formatValue plain stringifies Uint8Array as size descriptor', () => {
  assertEq(formatValue(new Uint8Array([1, 2, 3]), { kind: 'plain' }), '[3 bytes]');
});

test('formatValue null/undefined renders empty string', () => {
  assertEq(formatValue(null, { kind: 'plain' }), '');
  assertEq(formatValue(undefined, { kind: 'plain' }), '');
});

test('formatValue number with decimals + thousands_sep', () => {
  // Lokal en-US dla deterministycznego formatu.
  const out = formatValue(
    1234567.891,
    { kind: 'number', decimals: 2, thousands_sep: true },
    'en-US'
  );
  assertEq(out, '1,234,567.89');
});

test('formatValue number without thousands_sep', () => {
  const out = formatValue(
    1234,
    { kind: 'number', decimals: 0, thousands_sep: false },
    'en-US'
  );
  assertEq(out, '1234');
});

test('formatValue currency uses Intl currency formatting', () => {
  const out = formatValue(
    19.99,
    { kind: 'currency', code: 'USD' },
    'en-US'
  );
  assertContains(out, '19.99');
  assertContains(out, '$');
});

test('formatValue currency rejects empty code', () => {
  assertThrows(() =>
    formatValue(10, { kind: 'currency', code: '' }, 'en-US')
  );
});

test('formatValue percent multiplies by 100 per Intl semantics', () => {
  const out = formatValue(
    0.25,
    { kind: 'percent', decimals: 0 },
    'en-US'
  );
  assertEq(out, '25%');
});

test('formatValue percent with decimals', () => {
  const out = formatValue(
    0.1234,
    { kind: 'percent', decimals: 2 },
    'en-US'
  );
  assertEq(out, '12.34%');
});

test('formatValue bytes SI (1000)', () => {
  assertEq(
    formatValue(1500, { kind: 'bytes', base: '1000' }),
    '1.5 KB'
  );
  assertEq(
    formatValue(2_500_000, { kind: 'bytes', base: '1000' }),
    '2.5 MB'
  );
  assertEq(
    formatValue(500, { kind: 'bytes', base: '1000' }),
    '500 B'
  );
});

test('formatValue bytes binary (1024)', () => {
  assertEq(
    formatValue(2048, { kind: 'bytes', base: '1024' }),
    '2.0 KiB'
  );
  assertEq(
    formatValue(1_048_576, { kind: 'bytes', base: '1024' }),
    '1.0 MiB'
  );
});

test('formatValue duration stopwatch', () => {
  assertEq(
    formatValue(3_661_000, { kind: 'duration', style: 'stopwatch' }),
    '01:01:01'
  );
});

test('formatValue duration short', () => {
  assertEq(
    formatValue(125_000, { kind: 'duration', style: 'short' }),
    '2m 5s'
  );
  assertEq(
    formatValue(5_000, { kind: 'duration', style: 'short' }),
    '5s'
  );
});

test('formatValue duration long pluralizes', () => {
  assertEq(
    formatValue(1_000, { kind: 'duration', style: 'long' }),
    '1 second'
  );
  assertEq(
    formatValue(2_000, { kind: 'duration', style: 'long' }),
    '2 seconds'
  );
});

test('formatValue duration negative', () => {
  assertEq(
    formatValue(-5_000, { kind: 'duration', style: 'short' }),
    '-5s'
  );
});

test('formatValue date short renders via Intl.DateTimeFormat', () => {
  // 2024-01-15T00:00:00Z
  const ts = Date.UTC(2024, 0, 15);
  const out = formatValue(ts, { kind: 'date', style: 'short' }, 'en-US');
  // en-US short = "1/15/24" or "1/14/24" depending on TZ — sanity check
  // that some digits + slash appear.
  assertContains(out, '24');
});

test('formatValue time short', () => {
  const ts = Date.UTC(2024, 0, 15, 13, 30);
  const out = formatValue(ts, { kind: 'time', style: 'short' }, 'en-US');
  // Some hours/minutes representation.
  assert(out.length > 0);
});

test('formatValue datetime combined', () => {
  const ts = Date.UTC(2024, 0, 15, 13, 30);
  const out = formatValue(ts, { kind: 'datetime', style: 'short' }, 'en-US');
  assert(out.length > 0);
});

test('formatValue relative uses Intl.RelativeTimeFormat', () => {
  // 5 minutes ago.
  const ts = Date.now() - 5 * 60_000;
  const out = formatValue(ts, { kind: 'relative' }, 'en-US');
  // Should mention "5" and "minute" (en-US numeric=auto).
  assertContains(out, 'minute');
});

test('formatValue rejects unknown kind', () => {
  assertThrows(() => formatValue(1, { kind: 'magic' }, 'en-US'));
});

test('formatValue number rejects decimals out of range', () => {
  assertThrows(() =>
    formatValue(1, { kind: 'number', decimals: 25, thousands_sep: false }, 'en-US')
  );
  assertThrows(() =>
    formatValue(1, { kind: 'number', decimals: -1, thousands_sep: false }, 'en-US')
  );
});

test('formatValue number accepts BigInt within safe range', () => {
  assertEq(
    formatValue(
      9007199254740991n,
      { kind: 'number', decimals: 0, thousands_sep: false },
      'en-US'
    ),
    '9007199254740991'
  );
});

test('formatValue number rejects BigInt outside safe range', () => {
  assertThrows(() =>
    formatValue(
      9007199254740993n,
      { kind: 'number', decimals: 0, thousands_sep: false },
      'en-US'
    )
  );
});

test('formatValue bytes rejects bad base', () => {
  assertThrows(() =>
    formatValue(100, { kind: 'bytes', base: '500' })
  );
});

test('formatValue duration rejects bad style', () => {
  assertThrows(() =>
    formatValue(1000, { kind: 'duration', style: 'epic' })
  );
});

test('formatValue date rejects bad style', () => {
  assertThrows(() =>
    formatValue(Date.now(), { kind: 'date', style: 'epic' }, 'en-US')
  );
});

test('formatValue rejects timestamp beyond Date TimeClip range', () => {
  // Number.MAX_SAFE_INTEGER (9.007e15) > MAX_TIME_CLIP_MS (8.64e15) →
  // poprzednia walidacja by przeszła, ale Intl rzuca Invalid time value.
  assertThrows(() =>
    formatValue(9_000_000_000_000_000, { kind: 'date', style: 'short' }, 'en-US')
  );
  assertThrows(() =>
    formatValue(9_000_000_000_000_000n, { kind: 'date', style: 'short' }, 'en-US')
  );
});

test('formatValue accepts BigInt timestamp within TimeClip range', () => {
  const ts = BigInt(Date.UTC(2024, 0, 15));
  const out = formatValue(ts, { kind: 'date', style: 'short' }, 'en-US');
  assertContains(out, '24');
});

test('formatValue falls back to en for invalid locale tag', () => {
  // `xx-not-a-tag-too-long` jest nieprawidłowy w BCP 47 → canonicalLocale
  // fallbackuje do 'en'. Sanity: nie ma rzutu RangeError z Intl.
  const out = formatValue(
    1234,
    { kind: 'number', decimals: 0, thousands_sep: false },
    '###'
  );
  assertEq(out, '1234');
});

// ---- report ----

function reportResults(target) {
  let pass = 0;
  let fail = 0;
  const lines = [];
  for (const r of results) {
    if (r.ok) {
      pass++;
      lines.push(`✓ ${r.name}`);
    } else {
      fail++;
      lines.push(
        `✗ ${r.name}\n    ${r.err && r.err.stack ? r.err.stack : r.err}`
      );
    }
  }
  lines.push('');
  lines.push(
    `${pass}/${pass + fail} tests passed${fail ? ` — ${fail} FAILED` : ''}`
  );
  const text = lines.join('\n');
  if (target) {
    target.textContent = text;
    target.dataset.status = fail === 0 ? 'pass' : 'fail';
  }
  return { pass, fail, text };
}

if (typeof window === 'undefined' && typeof process !== 'undefined') {
  const r = reportResults(null);
  // eslint-disable-next-line no-console
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}

export { reportResults };
