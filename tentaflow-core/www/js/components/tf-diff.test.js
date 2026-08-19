// =============================================================================
// File: components/tf-diff.test.js
// Description: Tests for <tf-diff> — per-hunk review, unified/split rendering
// and the conflict block. The important guarantee is surgical: deciding one hunk
// must not rebuild the rest of the diff, or the reviewer loses scroll position
// and selection mid-review.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const { TfDiff } = await import('./tf-diff.js');

const HUNKS = () => ([
  {
    id: 'h1',
    header: '@@ -1,4 +1,6 @@ module header',
    status: 'pending',
    lines: [
      { kind: 'ctx', oldLn: 1, newLn: 1, text: 'use axum::Json;' },
      { kind: 'add', oldLn: null, newLn: 2, text: 'use serde::Serialize;' },
    ],
  },
  {
    id: 'h2',
    header: '@@ -9,2 +11,3 @@ input shape',
    status: 'pending',
    lines: [
      { kind: 'del', oldLn: 9, newLn: null, text: '// TODO: batching' },
      { kind: 'add', oldLn: null, newLn: 11, text: 'const MAX_BATCH: usize = 64;' },
    ],
  },
  {
    id: 'h3',
    header: '@@ -30,0 +38,2 @@ diagnostic log',
    status: 'pending',
    lines: [
      { kind: 'add', oldLn: null, newLn: 38, text: 'tracing::debug!("x");' },
    ],
  },
]);

function mount(attrs = {}) {
  const el = new TfDiff();
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  document.body.appendChild(el);
  return el;
}

function hunkEl(el, id) {
  return el.querySelector(`.tf-diff__hunk[data-hunk-id="${id}"]`);
}

function actionBtn(el, id, decision) {
  return hunkEl(el, id).querySelector(`button[data-decision="${decision}"]`);
}

// ---- per-hunk decisions ----------------------------------------------------

test('a decision emits hunk-decide and touches only that hunk', () => {
  const el = mount({ reviewable: '' });
  el.hunks = HUNKS();

  const first = hunkEl(el, 'h1');
  const third = hunkEl(el, 'h3');
  const firstHtml = first.innerHTML;
  const thirdHtml = third.innerHTML;

  const seen = [];
  el.addEventListener('hunk-decide', (e) => seen.push(e.detail));
  actionBtn(el, 'h2', 'accept').click();

  assert.deepEqual(seen, [{ hunkId: 'h2', decision: 'accept' }]);
  assert.equal(hunkEl(el, 'h1'), first, 'hunk 1 node must survive');
  assert.equal(hunkEl(el, 'h3'), third, 'hunk 3 node must survive');
  assert.equal(first.innerHTML, firstHtml, 'hunk 1 markup must be untouched');
  assert.equal(third.innerHTML, thirdHtml, 'hunk 3 markup must be untouched');

  const second = hunkEl(el, 'h2');
  assert.equal(second.dataset.status, 'accepted');
  assert.ok(second.classList.contains('tf-diff__hunk--accepted'));
  assert.ok(second.querySelector('.tf-diff__state--accepted'));
});

test('the diff body is not rebuilt by a decision', () => {
  const el = mount({ reviewable: '' });
  el.hunks = HUNKS();
  const body = hunkEl(el, 'h2').querySelector('.tf-diff__body');
  actionBtn(el, 'h2', 'reject').click();
  assert.equal(hunkEl(el, 'h2').querySelector('.tf-diff__body'), body);
  assert.ok(hunkEl(el, 'h2').classList.contains('tf-diff__hunk--rejected'));
});

test('a decided hunk offers revert, and revert returns it to pending', () => {
  const el = mount({ reviewable: '' });
  el.hunks = HUNKS();
  actionBtn(el, 'h1', 'accept').click();
  assert.equal(actionBtn(el, 'h1', 'accept'), null);
  assert.ok(actionBtn(el, 'h1', 'revert'));

  const seen = [];
  el.addEventListener('hunk-decide', (e) => seen.push(e.detail));
  actionBtn(el, 'h1', 'revert').click();
  assert.deepEqual(seen, [{ hunkId: 'h1', decision: 'revert' }]);
  assert.equal(hunkEl(el, 'h1').dataset.status, 'pending');
  assert.ok(actionBtn(el, 'h1', 'accept'));
});

test('preventDefault leaves the status to the caller', () => {
  const el = mount({ reviewable: '' });
  el.hunks = HUNKS();
  el.addEventListener('hunk-decide', (e) => e.preventDefault());
  actionBtn(el, 'h2', 'accept').click();
  assert.equal(hunkEl(el, 'h2').dataset.status, 'pending');

  // The server-side outcome may be a conflict, applied surgically afterwards.
  el.setHunkStatus('h2', 'conflicted');
  assert.equal(hunkEl(el, 'h2').dataset.status, 'conflicted');
  assert.ok(hunkEl(el, 'h2').querySelector('.tf-diff__state--conflicted'));
  assert.equal(actionBtn(el, 'h2', 'accept'), null, 'a conflicted hunk has no per-hunk action');
});

test('the resolved counter matches the hunk statuses', () => {
  const el = mount({ reviewable: '' });
  el.summary = { path: 'src/api.rs', added: 4, removed: 1, changeKind: 'modify' };
  el.hunks = HUNKS();
  const count = () => el.querySelector('.tf-diff__count').textContent;

  assert.equal(count(), '0/3 hunks resolved');
  actionBtn(el, 'h1', 'accept').click();
  assert.equal(count(), '1/3 hunks resolved');
  actionBtn(el, 'h3', 'reject').click();
  assert.equal(count(), '2/3 hunks resolved');
  actionBtn(el, 'h1', 'revert').click();
  assert.equal(count(), '1/3 hunks resolved');
});

test('without reviewable there are no per-hunk actions', () => {
  const el = mount();
  el.summary = { path: 'a.rs', added: 1, removed: 0, changeKind: 'add' };
  el.hunks = HUNKS();
  assert.equal(el.querySelectorAll('button[data-decision]').length, 0);
  assert.equal(el.querySelector('.tf-diff__count').textContent, '');
});

// ---- rendering -------------------------------------------------------------

test('unified mode carries an old and a new number column', () => {
  const el = mount();
  el.hunks = HUNKS();
  const lines = hunkEl(el, 'h2').querySelectorAll('.tf-diff__line');
  assert.equal(lines.length, 2);
  for (const line of lines) assert.equal(line.querySelectorAll('.tf-diff__ln').length, 2);
  const cells = (i) => [...lines[i].querySelectorAll('.tf-diff__ln')].map((n) => n.textContent);
  assert.deepEqual(cells(0), ['9', '']);    // a deletion exists only on the old side
  assert.deepEqual(cells(1), ['', '11']);   // an addition only on the new side
  assert.ok(lines[0].querySelector('.tf-diff__ln--old'));
  assert.ok(lines[0].querySelector('.tf-diff__ln--new'));
});

// R2: one shared column would have to alternate between the two numberings, and
// a deletion after a run of context would send it backwards mid-hunk.
test('each unified number column rises strictly within a hunk', () => {
  const el = mount();
  el.hunks = [{
    id: 'h', header: '@@ -120,4 +124,8 @@', status: 'pending',
    lines: [
      { kind: 'ctx', oldLn: 120, newLn: 124, text: '' },
      { kind: 'ctx', oldLn: 121, newLn: 125, text: '## Transport' },
      { kind: 'ctx', oldLn: 122, newLn: 126, text: '' },
      { kind: 'ctx', oldLn: 123, newLn: 127, text: 'split into a codec' },
      { kind: 'del', oldLn: 124, newLn: null, text: 'and a framing half.' },
      { kind: 'add', oldLn: null, newLn: 128, text: 'and a framing half.' },
      { kind: 'add', oldLn: null, newLn: 129, text: '## Framing' },
    ],
  }];
  const column = (cls) => [...hunkEl(el, 'h').querySelectorAll(`.tf-diff__ln--${cls}`)]
    .map((n) => n.textContent).filter(Boolean).map(Number);
  for (const cls of ['old', 'new']) {
    const nums = column(cls);
    const sorted = [...nums].sort((a, b) => a - b);
    assert.deepEqual(nums, sorted, `${cls} column must never go backwards: ${nums}`);
    assert.equal(new Set(nums).size, nums.length, `${cls} column must not repeat: ${nums}`);
  }
  assert.deepEqual(column('old'), [120, 121, 122, 123, 124]);
  assert.deepEqual(column('new'), [124, 125, 126, 127, 128, 129]);
});

test('gutters narrows a unified body to the one side it lists', () => {
  const el = mount({ gutters: 'new' });
  el.hunks = HUNKS();
  const lines = hunkEl(el, 'h2').querySelectorAll('.tf-diff__line');
  for (const line of lines) {
    assert.equal(line.querySelectorAll('.tf-diff__ln').length, 1);
    assert.equal(line.querySelectorAll('.tf-diff__ln--old').length, 0);
  }
  assert.deepEqual([...lines].map((l) => l.querySelector('.tf-diff__ln').textContent), ['', '11']);

  el.gutters = 'old';
  const old = hunkEl(el, 'h2').querySelectorAll('.tf-diff__line');
  assert.deepEqual([...old].map((l) => l.querySelector('.tf-diff__ln').textContent), ['9', '']);

  el.gutters = 'nonsense';
  assert.equal(el.gutters, 'both');
});

test('split mode builds two independent panes: base and accepted result', () => {
  const el = mount({ mode: 'split' });
  el.hunks = HUNKS();
  const panes = hunkEl(el, 'h2').querySelectorAll('.tf-diff__pane');
  assert.equal(panes.length, 2);

  const baseTexts = [...panes[0].querySelectorAll('.tf-diff__text')].map((n) => n.textContent);
  const resultTexts = [...panes[1].querySelectorAll('.tf-diff__text')].map((n) => n.textContent);
  assert.deepEqual(baseTexts, ['// TODO: batching']);
  assert.deepEqual(resultTexts, ['const MAX_BATCH: usize = 64;']);
  assert.equal(panes[0].querySelectorAll('.tf-diff__ln').length, 1);
  assert.equal(panes[1].querySelectorAll('.tf-diff__ln').length, 1);
});

test('the split grid collapses to one column below 900 px', () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const css = readFileSync(join(here, '..', '..', 'css', 'controls.css'), 'utf8');
  // The rule has to sit inside a 900 px media query — several such blocks exist,
  // so locate the one that actually carries the split grid.
  const idx = css.indexOf('.tf-diff__split { grid-template-columns: 1fr; }');
  assert.ok(idx > 0, 'the single-column split rule exists');
  const mediaOpen = css.lastIndexOf('@media (max-width: 900px)', idx);
  const blockEnd = css.indexOf('\n}', mediaOpen);
  assert.ok(mediaOpen > 0 && blockEnd > idx, 'it lives inside @media (max-width: 900px)');
});

// A phone cannot afford both number columns, so below 480 px CSS drops the old
// one. That collapse is only lossless because of what the component renders: a
// separate cell per side, the new one filled for everything that survives into
// the result and empty exactly on a deletion. Merge the two cells and the phone
// rule starts printing borrowed numbers.
test('the unified gutter can be collapsed to the new side without losing a number', () => {
  const el = mount();
  el.hunks = [{
    id: 'h', header: '@@ -120,4 +124,8 @@', status: 'pending',
    lines: [
      { kind: 'ctx', oldLn: 120, newLn: 124, text: '## Transport' },
      { kind: 'del', oldLn: 121, newLn: null, text: 'and a framing half.' },
      { kind: 'add', oldLn: null, newLn: 125, text: 'and a framing half.' },
      { kind: 'ctx', oldLn: 122, newLn: 126, text: '' },
    ],
  }];
  const lines = [...hunkEl(el, 'h').querySelectorAll('.tf-diff__line')];
  for (const line of lines) {
    assert.equal(line.querySelectorAll('.tf-diff__ln--old').length, 1);
    assert.equal(line.querySelectorAll('.tf-diff__ln--new').length, 1);
  }
  const newSide = lines.map((l) => l.querySelector('.tf-diff__ln--new').textContent);
  assert.deepEqual(newSide, ['124', '', '125', '126']);
  // The one column left standing still rises, which a mixed column would not:
  // the deletion's old number (121) sits below the context line above it (124).
  const kept = newSide.filter(Boolean).map(Number);
  assert.deepEqual(kept, [...kept].sort((a, b) => a - b));
  assert.ok(lines[1].classList.contains('tf-diff__line--del'),
    'the row with no number is the one CSS marks with a minus');
});

test('the second number column is dropped below 480 px, and only when both exist', () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const css = readFileSync(join(here, '..', '..', 'css', 'controls.css'), 'utf8');
  const idx = css.indexOf('.tf-diff__ln--old {\n    display: none;\n  }');
  assert.ok(idx > 0, 'the single-gutter rule exists');
  const mediaOpen = css.lastIndexOf('@media (max-width: 480px)', idx);
  assert.ok(mediaOpen > 0 && mediaOpen < idx, 'it lives inside @media (max-width: 480px)');
  const selector = css.slice(css.lastIndexOf('tf-diff', idx), idx + 20);
  // A body already narrowed to one side would be left with no gutter at all.
  assert.match(selector, /:not\(\[gutters="old"\]\)/);
  assert.match(selector, /:not\(\[gutters="new"\]\)/);
  assert.match(selector, /:not\(\[mode="split"\]\)/);
  // A deletion has no line in the result, so its empty cell gets a marker — and
  // that marker has to live inside the same narrow block, not at every width.
  const blockEnd = css.indexOf('\n}\n', idx);
  const marker = css.indexOf('.tf-diff__line--del .tf-diff__ln--new::after', mediaOpen);
  assert.ok(marker > mediaOpen && marker < blockEnd,
    'the deletion marker rule sits inside @media (max-width: 480px)');
});

test('clicking a line reports both line numbers', () => {
  const el = mount();
  el.hunks = HUNKS();
  const seen = [];
  el.addEventListener('line-click', (e) => seen.push(e.detail));
  hunkEl(el, 'h1').querySelectorAll('.tf-diff__line')[1].click();
  assert.deepEqual(seen, [{ hunkId: 'h1', oldLn: null, newLn: 2 }]);
});

test('the summary header carries path, change kind and counts', () => {
  const el = mount();
  el.summary = { path: 'src/api.rs', oldPath: 'src/old.rs', added: 12, removed: 3, changeKind: 'rename' };
  el.hunks = HUNKS();
  const head = el.querySelector('.tf-diff__head');
  assert.equal(head.querySelector('.tf-diff__path').textContent, 'src/api.rs');
  assert.equal(head.querySelector('.tf-diff__oldpath').textContent, 'src/old.rs');
  assert.equal(head.querySelector('.tf-diff__kind').textContent, 'renamed');
  assert.equal(head.querySelector('.tf-diff__stat--add').textContent, '+12');
  assert.equal(head.querySelector('.tf-diff__stat--del').textContent, '-3');
});

test('an empty hunk list renders the empty state', () => {
  const el = mount();
  el.hunks = [];
  assert.ok(el.querySelector('.tf-diff__empty'));
});

// ---- conflict --------------------------------------------------------------

test('the conflict block shows both digests and is announced', () => {
  const el = mount();
  el.hunks = HUNKS();
  el.conflict = { basedOnSha: 'a41c9e2', currentSha: '7e02b18', message: 'saved by another agent' };

  const block = el.querySelector('.tf-diff__conflict');
  assert.equal(block.hidden, false);
  assert.equal(block.getAttribute('role'), 'alert');
  const shas = [...block.querySelectorAll('.tf-diff__sha')].map((n) => n.textContent);
  assert.deepEqual(shas, ['a41c9e2', '7e02b18']);
  assert.match(block.querySelector('.tf-diff__conflict-body').textContent, /a41c9e2/);
  assert.match(block.querySelector('.tf-diff__conflict-body').textContent, /7e02b18/);
  assert.match(block.querySelector('.tf-diff__conflict-body').textContent, /overwritten silently/);
  assert.equal(block.querySelector('.tf-diff__conflict-msg').textContent, 'saved by another agent');
});

test('clearing the conflict hides the block', () => {
  const el = mount();
  el.conflict = { basedOnSha: 'a', currentSha: 'b' };
  assert.equal(el.querySelector('.tf-diff__conflict').hidden, false);
  el.conflict = null;
  assert.equal(el.querySelector('.tf-diff__conflict').hidden, true);
});

test('labels are overridable without touching the component', () => {
  const el = mount({ reviewable: '' });
  el.labels = { accept: 'Przyjmij', reject: 'Odrzuć', resolved: '{done} z {total}' };
  el.summary = { path: 'a.rs', added: 1, removed: 0, changeKind: 'add' };
  el.hunks = HUNKS();
  assert.equal(actionBtn(el, 'h1', 'accept').textContent, 'Przyjmij');
  assert.equal(actionBtn(el, 'h1', 'reject').textContent, 'Odrzuć');
  assert.equal(el.querySelector('.tf-diff__count').textContent, '0 z 3');
});
