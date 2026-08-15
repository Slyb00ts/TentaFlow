// =============================================================================
// File: modules/code-studio-session.timeline.test.js
// Description: Unit tests for what the session timeline is allowed to print.
//       Two things were caught on a phone: a row pasted the server's English
//       `FsError` text into a Polish screen, and it pasted a 40-character blob
//       digest with it, which wrapped over two lines and ate a quarter of the
//       viewport. So the digest shortener and the failure dictionary are pinned
//       here, together with the promise that every key the dictionary can return
//       exists in all five locales with the placeholders it is called with.
//       The functions are not exported (the module pulls the whole dashboard
//       in), so their source is cut out of the real file and evaluated — the
//       code under test is the shipped code.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'code-studio-session.js'), 'utf8');

function cutBalanced(src, start, open, close) {
  let depth = 0;
  let i = src.indexOf(open, start);
  for (; i < src.length; i += 1) {
    if (src[i] === open) depth += 1;
    else if (src[i] === close) {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

function cutFn(name) {
  const start = source.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`no definition: ${name}`);
  return cutBalanced(source, start, '{', '}');
}

function cutConst(name) {
  const start = source.indexOf(`const ${name} = `);
  if (start < 0) throw new Error(`no constant: ${name}`);
  const eq = start + `const ${name} = `.length;
  if (source[eq] === '[') return `${source.slice(start, eq)}${cutBalanced(source, eq, '[', ']')};`;
  return source.split('\n').find((l) => l.startsWith(`const ${name} = `));
}

// eslint-disable-next-line no-new-func
const {
  shortHash, shortenHashes, operationFailure, execVerdict, mergeExecPage, parseAt, durationOf,
} = new Function(`
  ${cutConst('HASH_RE')}
  ${cutFn('shortHash')}
  ${cutFn('shortenHashes')}
  ${cutConst('OPERATION_ERRORS')}
  ${cutFn('operationFailure')}
  ${cutFn('execVerdict')}
  ${cutFn('mergeExecPage')}
  ${cutConst('NAIVE_TS_RE')}
  ${cutFn('parseAt')}
  ${cutFn('durationOf')}
  return { shortHash, shortenHashes, operationFailure, execVerdict, mergeExecPage, parseAt, durationOf };
`)();

const LOCALES = ['pl', 'en', 'de', 'es', 'fr'].map((lang) => [
  lang,
  JSON.parse(readFileSync(join(here, '..', '..', 'i18n', `${lang}.json`), 'utf8')).code_studio,
]);

function lookup(dict, key) {
  return key.split('.').reduce((node, part) => (node == null ? node : node[part]), dict);
}

// ---------------------------------------------------------------------------
// Digests
// ---------------------------------------------------------------------------

test('shortHash keeps the seven characters git itself prints', () => {
  assert.equal(shortHash('d159fef3450c49d7c5d0c08a9045f38c11c40c12'), 'd159fef…');
  assert.equal(shortHash(''), '');
  assert.equal(shortHash(null), '');
  // Nothing short enough to read whole is truncated to look truncated.
  assert.equal(shortHash('abc1234'), 'abc1234');
  assert.equal(shortHash('abc12345'), 'abc12345');
  assert.equal(shortHash('abc123456'), 'abc1234…');
});

test('shortenHashes rewrites every digest in a sentence and nothing else', () => {
  const raw = 'conflict: expected 93272e13ecbb73ffd6d5777fb2a7e51d55d9251e, '
    + 'found d159fef3450c49d7c5d0c08a9045f38c11c40c12';
  assert.equal(
    shortenHashes(raw),
    'conflict: expected 93272e1…, found d159fef…',
  );
  assert.match(shortenHashes(raw), /^[^]{0,60}$/);
  // Prose, paths and short hex survive untouched.
  assert.equal(shortenHashes('cannot open src/decade.rs'), 'cannot open src/decade.rs');
  assert.equal(shortenHashes('exit code 1'), 'exit code 1');
  assert.equal(shortenHashes('abcdef012345'), 'abcdef0…');
});

test('shortenHashes does not cut a run out of a hyphenated uuid', () => {
  const uuid = '00000000-0000-4000-8000-000000000002';
  assert.equal(shortenHashes(`decided by ${uuid}`), `decided by ${uuid}`);
});

test('no timeline argument can carry a digest longer than ten characters', () => {
  const samples = [
    'conflict: expected absent, found d159fef3450c49d7c5d0c08a9045f38c11c40c12',
    'refused: blob 1632ba7db25000cf8b9ef066fb7f771a696de988 is outside the worktree',
    '868d351ab8fa5b01cc8b9047465f5059e60905115a4e1789e523f0c563e09f04',
  ];
  for (const sample of samples) {
    for (const run of shortenHashes(sample).match(/[0-9a-f]{9,}/g) || []) {
      assert.fail(`digest left long on the timeline: ${run}`);
    }
  }
});

// ---------------------------------------------------------------------------
// Failure dictionary
// ---------------------------------------------------------------------------

// Every `Display` shape of the server's FsError (code_studio/fs/mod.rs), with
// the answer the timeline is expected to give.
const FS_ERRORS = [
  ['conflict: expected absent, found d159fef3450c49d7c5d0c08a9045f38c11c40c12',
    'event.err_conflict_absent', { current: 'd159fef…' }],
  ['conflict: expected 93272e13ecbb73ffd6d5777fb2a7e51d55d9251e, found nothing',
    'event.err_conflict_gone', { base: '93272e1…' }],
  ['conflict: expected 37f0dda32f9fe3e4e2ed2adbc5b5be693b07a42a, found 1632ba7db25000cf8b9ef066fb7f771a696de988',
    'event.err_conflict_moved', { base: '37f0dda…', current: '1632ba7…' }],
  ['no such file or directory', 'event.err_not_found', {}],
  ['already exists', 'event.err_exists', {}],
  ['not a directory', 'event.err_not_a_dir', {}],
  ['is a directory', 'event.err_is_a_dir', {}],
  ['file is not valid UTF-8 text', 'event.err_not_text', {}],
  ['edit is ambiguous: 3 occurrences of "fn main"; extend it until it is unique',
    'event.err_edit_ambiguous', { count: 3 }],
  ['edit target "fn main" does not occur in the file', 'event.err_edit_missing', {}],
  ['4194305 bytes exceeds the 4194304 byte limit',
    'event.err_too_large', { size: '4194305', limit: '4194304' }],
  ['limit exceeded: 512 open files', 'event.err_limit', { reason: '512 open files' }],
  ['refused: symlink escapes the worktree', 'event.err_denied', { reason: 'symlink escapes the worktree' }],
  ['invalid path: empty segment', 'event.err_bad_path', { reason: 'empty segment' }],
  ['invalid request: no content', 'event.err_bad_request', { reason: 'no content' }],
  ['io error: permission denied', 'event.err_io', { reason: 'permission denied' }],
];

test('every FsError shape is answered in the interface language, not quoted', () => {
  for (const [raw, key, vars] of FS_ERRORS) {
    const failure = operationFailure(raw);
    assert.equal(failure.key, key, raw);
    assert.deepEqual(failure.vars, vars, raw);
    assert.equal(failure.raw, raw, 'the untranslated text stays available for the title');
  }
});

test('an unmapped message is marked as a quotation from the tool', () => {
  const raw = 'fatal: refusing to merge unrelated histories';
  const failure = operationFailure(raw);
  assert.equal(failure.key, 'event.err_quoted');
  assert.deepEqual(failure.vars, { message: raw });
  assert.equal(failure.raw, raw);
});

test('an operation that failed without a reason still says so', () => {
  for (const empty of ['', null, undefined, '   ']) {
    assert.equal(operationFailure(empty).key, 'event.err_unknown');
  }
});

// ---------------------------------------------------------------------------
// The dictionary and the locales have to agree
// ---------------------------------------------------------------------------

test('every key the dictionary can return exists in all five locales', () => {
  const keys = new Set([
    ...FS_ERRORS.map(([, key]) => key),
    'event.err_quoted',
    'event.err_unknown',
  ]);
  for (const [lang, dict] of LOCALES) {
    for (const key of keys) {
      assert.equal(typeof lookup(dict, key), 'string', `${lang} is missing ${key}`);
    }
  }
});

test('every placeholder in a failure translation is one the caller supplies', () => {
  const supplied = new Map(FS_ERRORS.map(([, key, vars]) => [key, new Set(Object.keys(vars))]));
  supplied.set('event.err_quoted', new Set(['message']));
  supplied.set('event.err_unknown', new Set());
  for (const [lang, dict] of LOCALES) {
    for (const [key, names] of supplied) {
      const text = lookup(dict, key);
      for (const [, name] of text.matchAll(/\{(\w+)(?:\|[^}]*)?\}/g)) {
        assert.ok(names.has(name), `${lang}/${key} interpolates {${name}}, which is never passed`);
      }
    }
  }
});

test('the Polish failure texts carry no untranslated English left over', () => {
  const dict = LOCALES.find(([lang]) => lang === 'pl')[1];
  for (const [, key] of FS_ERRORS) {
    const text = lookup(dict, key);
    assert.doesNotMatch(text, /\b(expected|found|conflict|directory|exists)\b/, key);
  }
});

// ---------------------------------------------------------------------------
// A command that succeeded against a copy
// ---------------------------------------------------------------------------

test('a command narrowed to a copy is called out, whatever its exit code says', () => {
  const v = execVerdict({
    op_id: 'op-1', exit_code: 0, requested_mount_access: 'rw', writes_discarded: true,
  });
  assert.equal(v.discarded, true);
  assert.equal(v.requested, 'rw');
  assert.equal(v.tone, 'wait', 'exit 0 must not read as "done" when nothing landed');
  assert.equal(v.noteKey, 'exec.discarded_note');
  assert.equal(v.execId, 'op-1');
});

test('a command that really wrote gets no warning at all', () => {
  const v = execVerdict({
    op_id: 'op-2', exit_code: 0, requested_mount_access: 'rw', writes_discarded: false,
  });
  assert.equal(v.discarded, false);
  assert.equal(v.tone, 'ok');
});

test('a failure stays a failure even when its writes were dropped', () => {
  assert.equal(execVerdict({ exit_code: 1, writes_discarded: true }).tone, 'err');
  assert.equal(execVerdict({ exit_code: 1, writes_discarded: false }).tone, 'err');
});

test('a command still running is neither claimed to have landed nor to have failed', () => {
  const v = execVerdict({ op_id: 'op-3', exit_code: null });
  assert.equal(v.tone, 'run');
  assert.equal(v.discarded, false);
});

// The server that wrote the rows on disk today has neither field. Their absence
// is the one case that must NOT produce a warning — an old row would otherwise
// accuse every command in the archive of losing its writes.
test('a row from a server without the two fields makes no claim', () => {
  const v = execVerdict({ op_id: 'op-4', argv: ['sh'], cwd: '/w', exit_code: 0 });
  assert.equal(v.discarded, false);
  assert.equal(v.requested, '');
  assert.equal(v.tone, 'ok');
});

test('the request the PEP narrowed is named when the row carries it', () => {
  assert.equal(execVerdict({ writes_discarded: true, requested_mount_access: 'rw' }).noteKey,
    'exec.discarded_note');
  // A tool-side effect records no request; the sentence then states only the
  // outcome instead of interpolating an empty mount name.
  assert.equal(execVerdict({ writes_discarded: true, requested_mount_access: '' }).noteKey,
    'exec.discarded_note_plain');
});

test('camelCase and snake_case answers say the same thing', () => {
  const snake = execVerdict({ op_id: 'a', exit_code: 0, requested_mount_access: 'rw', writes_discarded: true });
  const camel = execVerdict({ opId: 'a', exitCode: 0, requestedMountAccess: 'rw', writesDiscarded: true });
  assert.deepEqual(camel, snake);
});

test('every exec key the timeline can print exists in all five locales', () => {
  const keys = ['exec.discarded_pill', 'exec.discarded_note', 'exec.discarded_note_plain',
    'exec.open', 'exec.refresh', 'exec.more', 'exec.lines', 'exec.no_output',
    'exec.still_running', 'exec.loading', 'exec.unavailable'];
  for (const [lang, dict] of LOCALES) {
    for (const key of keys) {
      assert.equal(typeof lookup(dict, key), 'string', `${lang} is missing ${key}`);
    }
  }
});

test('the exec sentences interpolate only what the caller passes', () => {
  const supplied = new Map([
    ['exec.discarded_note', new Set(['requested'])],
    ['exec.discarded_note_plain', new Set()],
    ['exec.lines', new Set(['count'])],
    ['exec.unavailable', new Set(['reason'])],
  ]);
  for (const [lang, dict] of LOCALES) {
    for (const [key, names] of supplied) {
      for (const [, name] of lookup(dict, key).matchAll(/\{(\w+)(?:\|[^}]*)?\}/g)) {
        assert.ok(names.has(name), `${lang}/${key} interpolates {${name}}, which is never passed`);
      }
    }
  }
});

test('the line counter carries a plural form per language, three of them in Polish', () => {
  for (const [lang, dict] of LOCALES) {
    const text = lookup(dict, 'exec.lines');
    const forms = /\{count\|([^}]*)\}/.exec(text);
    assert.ok(forms, `${lang} counts lines by concatenation instead of a plural form`);
    const expected = lang === 'pl' ? 3 : 2;
    assert.equal(forms[1].split('|').length, expected, `${lang} plural forms`);
  }
});

// ---------------------------------------------------------------------------
// Reading the transcript back
// ---------------------------------------------------------------------------

test('a page advances the cursor by the cursor the server answered with', () => {
  const view = { cursor: 0, count: 0, status: '' };
  const page = mergeExecPage(view, {
    lines: ['a', 'b'], status: 'completed', next_seq: 2, has_more: true,
  });
  assert.deepEqual(page.lines, ['a', 'b']);
  assert.equal(page.cursor, 2);
  assert.equal(page.count, 2);
  assert.equal(page.hasMore, true);
  assert.equal(page.status, 'completed');
});

test('a second page continues where the first stopped', () => {
  const view = { cursor: 2, count: 2, status: 'completed' };
  const page = mergeExecPage(view, { lines: ['c'], status: 'completed', next_seq: 3, has_more: false });
  assert.equal(page.cursor, 3);
  assert.equal(page.count, 3);
  assert.equal(page.hasMore, false);
});

test('a peer that omits the cursor is counted, not trusted backwards', () => {
  assert.equal(mergeExecPage({ cursor: 5, count: 5 }, { lines: ['x', 'y'] }).cursor, 7);
  // A cursor that went backwards would replay every line of the page.
  assert.equal(mergeExecPage({ cursor: 5, count: 5 }, { lines: ['x'], next_seq: 1 }).cursor, 6);
});

test('"there is more" is refused when the page moved nothing', () => {
  const page = mergeExecPage({ cursor: 4, count: 4 }, { lines: [], has_more: true, next_seq: 4 });
  assert.equal(page.hasMore, false, 'an empty page that claims more is an endless button');
  assert.equal(page.cursor, 4);
  assert.equal(page.count, 4);
});

test('an answer with no lines at all is read as an empty transcript, not a crash', () => {
  const page = mergeExecPage({ cursor: 0, count: 0, status: '' }, {});
  assert.deepEqual(page.lines, []);
  assert.equal(page.cursor, 0);
  assert.equal(page.count, 0);
  assert.equal(page.hasMore, false);
});

test('camelCase paging fields are accepted too', () => {
  const page = mergeExecPage({ cursor: 0, count: 0 }, { lines: ['a'], nextSeq: 1, hasMore: true });
  assert.equal(page.cursor, 1);
  assert.equal(page.hasMore, true);
});

// ---------------------------------------------------------------------------
// Server timestamps
// ---------------------------------------------------------------------------

// SQLite writes `CURRENT_TIMESTAMP` zoneless and in UTC. Read as local time it
// shifts every clock on the timeline by the viewer's offset, and it dates a run
// still in flight hours into the past.
test('a zoneless server timestamp is read as UTC, not as local time', () => {
  assert.equal(parseAt('2026-08-15 14:20:57'), Date.UTC(2026, 7, 15, 14, 20, 57));
  assert.equal(parseAt('2026-08-15T14:20:57'), Date.UTC(2026, 7, 15, 14, 20, 57));
  assert.equal(parseAt('2026-08-15 14:20:57.500'), Date.UTC(2026, 7, 15, 14, 20, 57, 500));
});

test('a timestamp that carries its own zone is left alone', () => {
  assert.equal(parseAt('2026-08-15T14:20:57Z'), Date.UTC(2026, 7, 15, 14, 20, 57));
  assert.equal(parseAt('2026-08-15T16:20:57+02:00'), Date.UTC(2026, 7, 15, 14, 20, 57));
});

test('nothing and nonsense are not dates', () => {
  for (const empty of ['', null, undefined, '   ']) assert.ok(Number.isNaN(parseAt(empty)));
  assert.ok(Number.isNaN(parseAt('yesterday')));
});

test('the real 74-second turn is reported as the minute and a bit it took', () => {
  assert.equal(durationOf('2026-08-15 14:20:57', '2026-08-15 14:22:11'), '1m 14s');
  assert.equal(durationOf('2026-08-15 14:20:57', '2026-08-15 14:21:20'), '23s');
  assert.equal(durationOf('', '2026-08-15 14:21:20'), '', 'a row with no start states nothing');
});
