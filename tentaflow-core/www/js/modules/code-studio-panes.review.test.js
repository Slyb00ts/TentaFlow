// =============================================================================
// File: modules/code-studio-panes.review.test.js
// Description: Unit tests for the change-review arithmetic of the Code Studio
//       panes: the unified-diff parser and the hunk tally. Both are pinned here
//       because a reviewer caught the screen lying: the wire repeats the `@@`
//       header as the first row of a hunk body, which printed the header as a
//       line of the file AND shifted every line number in the hunk by one; and
//       the three hunk counters on the change screen were each counted in their
//       own place, so they could disagree about the same patch set.
//       The functions are not exported (the module pulls the whole dashboard
//       in), so their source is cut out of the real file by brace matching and
//       evaluated — the code under test is the shipped code.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const source = readFileSync(join(here, 'code-studio-panes.js'), 'utf8');

function cut(src, name) {
  const start = src.indexOf(`function ${name}(`);
  if (start < 0) throw new Error(`no definition: ${name}`);
  let depth = 0;
  let i = src.indexOf('{', start);
  for (; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

function cutConst(src, name) {
  const line = src.split('\n').find((l) => l.startsWith(`const ${name} =`));
  if (!line) throw new Error(`no constant: ${name}`);
  return line;
}

function build(names, consts = []) {
  const body = [...consts.map((c) => cutConst(source, c)), ...names.map((n) => cut(source, n))].join('\n');
  // eslint-disable-next-line no-new-func
  return new Function(`${body}\nreturn { ${names.join(', ')} };`)();
}

const { hunkLines, hunkStats, fileStats } = build(
  ['hunkLines', 'hunkStats', 'fileStats'],
  ['HUNK_HEADER_RE'],
);
const { tallyHunks, tallyDecisions, setHunks } = build(
  ['decisionOf', 'tallyHunks', 'setHunks', 'tallyDecisions'],
);

// The shape the server actually sends: `header` repeated as row 0 of `content`.
const README_HUNK = {
  patch_hunk_id: 'h3',
  header: '@@ -120,4 +124,8 @@ dual licensed as above',
  content: [
    '@@ -120,4 +124,8 @@ dual licensed as above',
    ' ',
    ' ## Transport',
    ' ',
    '-The transport layer is being split into a codec and a framing half.',
    '\\ No newline at end of file',
    '+The transport layer is being split into a codec and a framing half.',
    '+',
    '+## Framing',
    '',
  ].join('\n'),
};

// ---------------------------------------------------------------------------
// hunkLines — the header is positioning metadata, never a line of the file
// ---------------------------------------------------------------------------

test('hunkLines: the repeated @@ header is not emitted as a diff line', () => {
  const lines = hunkLines(README_HUNK);
  for (const line of lines) {
    assert.doesNotMatch(line.text, /^@ -\d+/, `header leaked into the body: ${line.text}`);
    assert.doesNotMatch(line.text, /^@@ -\d+/, `header leaked into the body: ${line.text}`);
  }
  assert.equal(lines[0].text, '');
  assert.equal(lines[1].text, '## Transport');
});

test('hunkLines: numbering starts at the header, not one line above it', () => {
  const [first] = hunkLines(README_HUNK);
  assert.equal(first.oldLn, 120);
  assert.equal(first.newLn, 124);
});

test('hunkLines: each side numbers strictly upwards and never repeats', () => {
  const lines = hunkLines(README_HUNK);
  for (const side of ['oldLn', 'newLn']) {
    const nums = lines.map((l) => l[side]).filter((n) => Number.isInteger(n));
    const sorted = [...nums].sort((a, b) => a - b);
    assert.deepEqual(nums, sorted, `${side} runs backwards: ${nums}`);
    assert.equal(new Set(nums).size, nums.length, `${side} repeats a number: ${nums}`);
  }
  assert.deepEqual(lines.map((l) => l.oldLn), [120, 121, 122, 123, null, null, null]);
  assert.deepEqual(lines.map((l) => l.newLn), [124, 125, 126, null, 127, 128, 129]);
});

test('hunkLines: "\\ No newline at end of file" is not a line either', () => {
  const texts = hunkLines(README_HUNK).map((l) => l.text);
  assert.equal(texts.filter((t) => t.includes('No newline')).length, 0);
});

test('hunkLines: a body carrying several hunks re-seeds both counters', () => {
  const lines = hunkLines({
    header: '@@ -1,2 +1,2 @@',
    content: '@@ -1,2 +1,2 @@\n a\n@@ -40,1 +41,1 @@\n b\n',
  });
  assert.deepEqual(lines.map((l) => [l.text, l.oldLn, l.newLn]), [
    ['a', 1, 1],
    ['b', 40, 41],
  ]);
});

test('hunkStats/fileStats count only real added and removed lines', () => {
  assert.deepEqual(hunkStats(README_HUNK), { add: 3, del: 1 });
  assert.deepEqual(fileStats({ hunks: [README_HUNK, README_HUNK] }), { add: 6, del: 2 });
});

// ---------------------------------------------------------------------------
// tallyHunks — one arithmetic behind every counter on the screen
// ---------------------------------------------------------------------------

const FILES = () => ([
  {
    patch_file_id: 'f1',
    hunks: [
      { patch_hunk_id: 'a', status: 'pending' },
      { patch_hunk_id: 'b', status: 'pending' },
      { patch_hunk_id: 'c', status: 'pending' },
    ],
  },
  {
    patch_file_id: 'f2',
    hunks: [
      { patch_hunk_id: 'd', status: 'accepted' },
      { patch_hunk_id: 'e', status: 'rejected' },
    ],
  },
]);

function bus(decisions = {}) {
  return { files: FILES(), decisions: new Map(Object.entries(decisions)) };
}

test('tallyHunks: decided is accepted plus rejected, and the parts sum to total', () => {
  const b = bus({ a: 'accept', c: 'reject' });
  const t = tallyHunks(b, b.files[0].hunks);
  assert.deepEqual(t, { accepted: 1, rejected: 1, pending: 1, decided: 2, total: 3 });
  assert.equal(t.accepted + t.rejected + t.pending, t.total);
});

test('tallyHunks: a saved status counts when no local decision overrides it', () => {
  const b = bus();
  assert.deepEqual(tallyHunks(b, b.files[1].hunks),
    { accepted: 1, rejected: 1, pending: 0, decided: 2, total: 2 });
  const flipped = bus({ d: 'reject' });
  assert.deepEqual(tallyHunks(flipped, flipped.files[1].hunks),
    { accepted: 0, rejected: 2, pending: 0, decided: 2, total: 2 });
});

test('the file counter and the set counter are the same function', () => {
  const b = bus({ a: 'accept', c: 'reject' });
  const set = tallyDecisions(b);
  assert.equal(setHunks(b).length, 5);
  assert.deepEqual(set, { accepted: 2, rejected: 2, pending: 1, decided: 4, total: 5 });

  // What the head chip, the footer and the legend read must add up to one truth:
  // the per-file counters sum to the set counter, field by field.
  const perFile = b.files.map((f) => tallyHunks(b, f.hunks));
  for (const field of ['accepted', 'rejected', 'pending', 'decided', 'total']) {
    assert.equal(perFile.reduce((sum, t) => sum + t[field], 0), set[field], field);
  }
  assert.equal(set.accepted + set.rejected + set.pending, set.total);
});

test('an empty patch set counts to zero instead of dividing by nothing', () => {
  const empty = { files: [], decisions: new Map() };
  assert.deepEqual(tallyDecisions(empty),
    { accepted: 0, rejected: 0, pending: 0, decided: 0, total: 0 });
});
