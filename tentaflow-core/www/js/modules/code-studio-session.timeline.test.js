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
const { shortHash, shortenHashes, operationFailure } = new Function(`
  ${cutConst('HASH_RE')}
  ${cutFn('shortHash')}
  ${cutFn('shortenHashes')}
  ${cutConst('OPERATION_ERRORS')}
  ${cutFn('operationFailure')}
  return { shortHash, shortenHashes, operationFailure };
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
