// =============================================================================
// File: components/component-extensions.test.js
// Description: Tests for the additive extensions the feature modules needed
// from shared components — tf-tree node badges, the tf-tab dirty dot, the
// tf-chip mono variant, the tf-badge "hot" tone, tf-column hide-below, the
// three tf-agent-activity gaps (level attribute, child_spawned parenting,
// cards=off) and tf-select.setOptions replacing BOTH option lists.
//
// Every block also asserts the PRE-EXISTING behaviour of the same code path, so
// a regression in one of the several dozen modules using these components shows
// up here rather than in a browser.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const WWW_ROOT = join(here, '..', '..');

// tf-agent-activity imports its siblings by absolute `/js/...` browser paths.
const { register } = await import('node:module');
const { pathToFileURL } = await import('node:url');
register(
  `data:text/javascript,${encodeURIComponent(`
    const ROOT = ${JSON.stringify(pathToFileURL(`${WWW_ROOT}/`).href)};
    export async function resolve(spec, ctx, next) {
      if (spec.startsWith('/js/')) return { url: new URL('.' + spec, ROOT).href, shortCircuit: true };
      return next(spec, ctx);
    }
  `)}`,
  import.meta.url,
);
// Environment gaps the shared happy-dom harness does not export. tf-tabs feature
// -detects via `'ResizeObserver' in window` and then constructs it off the global
// scope, and shared-styles.js probes `Document.prototype`.
const { window } = await import('../sdk-runtime/_dom-test-harness.js');
if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
// tf-table adopts /css/controls.css on build; there is no server under Node, so
// the fetch is answered with an empty sheet — the adopt path then completes
// instead of leaving a rejected promise behind every table test.
globalThis.fetch = () => Promise.resolve({ ok: true, text: () => Promise.resolve('') });
process.on('unhandledRejection', () => {});

const { TfTree } = await import('./tf-tree.js');
const { TfTabs, TfTab } = await import('./tf-tabs.js');
const { TfChip } = await import('./tf-chip.js');
const { TfBadge } = await import('./tf-badge.js');
const { TfTable, TfColumn } = await import('./tf-table.js');
const { TfAgentActivity } = await import('./tf-agent-activity.js');
// The light-DOM adoption in tf-select/tf-button runs off MutationObserver, which
// happy-dom implements on the window but does not export as a bare global.
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}
const { TfSelect } = await import('./tf-select.js');
const { TfButton } = await import('./tf-button.js');

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

function mount(el, attrs = {}) {
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, v);
  document.body.appendChild(el);
  return el;
}

// ---------------------------------------------------------------------------
// 1. tf-tree — node.badge
// ---------------------------------------------------------------------------

test('tf-tree: the badge renders AFTER the label, not before it', () => {
  const tree = mount(new TfTree());
  tree.nodes = [{ id: 'f1', label: 'embeddings.rs', badge: { text: 'M', tone: 'm' } }];

  const row = tree.querySelector('.tf-tree__row');
  const kids = [...row.children].map((c) => c.className);
  const labelIdx = kids.findIndex((c) => c.includes('tf-tree__label'));
  const badgeIdx = kids.findIndex((c) => c.includes('tf-tree__badge'));
  assert.ok(labelIdx >= 0 && badgeIdx >= 0, 'both label and badge exist');
  assert.ok(badgeIdx > labelIdx, 'badge must follow the label');
  assert.equal(row.querySelector('.tf-tree__badge').textContent, 'M');
  assert.ok(row.querySelector('.tf-tree__badge').classList.contains('tf-tree__badge--m'));
});

test('tf-tree: the icon still renders BEFORE the label (unchanged)', () => {
  const tree = mount(new TfTree());
  const icon = document.createElement('span');
  tree.nodes = [{ id: 'f1', label: 'a.rs', icon, badge: 'D' }];

  const row = tree.querySelector('.tf-tree__row');
  const kids = [...row.children];
  const iconIdx = kids.findIndex((c) => c.classList.contains('tf-tree__icon'));
  const labelIdx = kids.findIndex((c) => c.classList.contains('tf-tree__label'));
  const badgeIdx = kids.findIndex((c) => c.classList.contains('tf-tree__badge'));
  assert.ok(iconIdx < labelIdx && labelIdx < badgeIdx, 'icon · label · badge');
});

test('tf-tree: a bare string badge works and gets no tone class', () => {
  const tree = mount(new TfTree());
  tree.nodes = [{ id: 'f1', label: 'a.rs', badge: 'nowy' }];
  const badge = tree.querySelector('.tf-tree__badge');
  assert.equal(badge.textContent, 'nowy');
  assert.equal(badge.className, 'tf-tree__badge');
});

test('tf-tree: an unknown tone falls back to the untoned badge', () => {
  const tree = mount(new TfTree());
  tree.nodes = [{ id: 'f1', label: 'a.rs', badge: { text: '!', tone: 'zzz' } }];
  assert.equal(tree.querySelector('.tf-tree__badge').className, 'tf-tree__badge');
});

test('tf-tree: nodes without a badge render exactly as before', () => {
  const tree = mount(new TfTree());
  tree.nodes = [{ id: 'a', label: 'A', children: [{ id: 'a1', label: 'A1' }] }];
  tree.expandedIds = ['a'];
  assert.equal(tree.querySelectorAll('.tf-tree__badge').length, 0);
  assert.equal(tree.querySelectorAll('.tf-tree__row').length, 2);
  // Selection/expand events still behave.
  const seen = [];
  tree.addEventListener('select', (e) => seen.push(e.detail.id));
  tree.querySelectorAll('.tf-tree__label')[1].click();
  assert.deepEqual(seen, ['a1']);
});

test('tf-tree: an empty or nullish badge renders nothing', () => {
  const tree = mount(new TfTree());
  tree.nodes = [
    { id: 'a', label: 'A', badge: '' },
    { id: 'b', label: 'B', badge: null },
    { id: 'c', label: 'C', badge: {} },
  ];
  assert.equal(tree.querySelectorAll('.tf-tree__badge').length, 0);
});

// ---------------------------------------------------------------------------
// 2. tf-tab — dirty dot
// ---------------------------------------------------------------------------

function tabs(spec) {
  const host = new TfTabs();
  for (const s of spec) {
    const tab = new TfTab();
    tab.id = s.id;
    tab.textContent = s.label;
    if (s.dirty) tab.setAttribute('dirty', '');
    if (s.count) tab.setAttribute('count', s.count);
    host.appendChild(tab);
  }
  return mount(host);
}

test('tf-tab: dirty renders a dot after the label and leaves the label text alone', () => {
  const host = tabs([{ id: 'a', label: 'embeddings.rs', dirty: true }]);
  const btn = host.querySelector('.tf-tab');
  assert.equal(btn.querySelector('.tf-tab-label').textContent, 'embeddings.rs');
  assert.ok(btn.querySelector('.tf-tab-dirty'), 'dot exists');
  assert.ok(btn.classList.contains('is-dirty'));
  const kids = [...btn.children].map((c) => c.className);
  assert.ok(kids.indexOf('tf-tab-dirty') > kids.indexOf('tf-tab-label'));
  assert.equal(btn.querySelector('.tf-tab-dirty').getAttribute('aria-hidden'), 'true');
});

test('tf-tab: removing dirty removes the dot', () => {
  const host = tabs([{ id: 'a', label: 'x', dirty: true }]);
  const tab = host.querySelector('tf-tab');
  tab.removeAttribute('dirty');
  const btn = host.querySelector('.tf-tab');
  assert.equal(btn.querySelector('.tf-tab-dirty'), null);
  assert.equal(btn.classList.contains('is-dirty'), false);
});

test('tf-tab: a clean tab keeps its previous markup (label + count, no dot)', () => {
  const host = tabs([{ id: 'a', label: 'Zmiany', count: '3' }]);
  const btn = host.querySelector('.tf-tab');
  assert.equal(btn.querySelector('.tf-tab-dirty'), null);
  assert.equal(btn.querySelector('.tf-tab-label').textContent, 'Zmiany');
  assert.equal(btn.querySelector('.tf-tab-count').textContent, '3');
});

test('tf-tab: dirty coexists with the count pill, dot first', () => {
  const host = tabs([{ id: 'a', label: 'x', dirty: true, count: '2' }]);
  const kids = [...host.querySelector('.tf-tab').children].map((c) => c.className);
  assert.ok(kids.indexOf('tf-tab-dirty') < kids.indexOf('tf-tab-count'));
});

// ---------------------------------------------------------------------------
// 3. tf-chip — mono + leading icon
// ---------------------------------------------------------------------------

test('tf-chip: mono adds the modifier class without dropping the status class', () => {
  const chip = new TfChip();
  chip.textContent = 'cs/piotr/9f2a1c4b';
  mount(chip, { mono: '', status: 'accent' });
  const span = chip.querySelector('span');
  assert.ok(span.classList.contains('tf-chip'));
  assert.ok(span.classList.contains('accent'));
  assert.ok(span.classList.contains('tf-chip--mono'));
  assert.equal(span.textContent, 'cs/piotr/9f2a1c4b');
});

test('tf-chip: the icon is the leading child, before the label text', () => {
  const chip = new TfChip();
  chip.textContent = 'cow';
  mount(chip, { mono: '', icon: 'layers' });
  const span = chip.querySelector('span.tf-chip');
  assert.equal(span.firstChild.nodeName.toLowerCase(), 'svg');
  assert.equal(span.firstChild.getAttribute('class'), 'tf-chip-icon');
  assert.equal(span.textContent, 'cow');
  assert.match(span.innerHTML, /#i-layers/);
});

test('tf-chip: mono is reactive and removable', () => {
  const chip = new TfChip();
  chip.textContent = 'x';
  mount(chip, { mono: '' });
  assert.ok(chip.querySelector('span').classList.contains('tf-chip--mono'));
  chip.removeAttribute('mono');
  assert.equal(chip.querySelector('span').classList.contains('tf-chip--mono'), false);
});

test('tf-chip: a plain chip is unchanged by the new attribute', () => {
  const chip = new TfChip();
  chip.textContent = 'Online';
  mount(chip, { status: 'online', dot: '' });
  const span = chip.querySelector('span');
  assert.equal(span.className, 'tf-chip online');
  assert.ok(span.querySelector('.tf-chip-dot'));
  assert.equal(span.textContent, 'Online');
});

test('tf-chip: an unsafe icon name is rejected (no markup injection)', () => {
  const chip = new TfChip();
  chip.textContent = 'x';
  mount(chip, { icon: '"><script>x</script>' });
  assert.equal(chip.querySelector('svg'), null);
});

// ---------------------------------------------------------------------------
// 4. tf-badge — hot tone
// ---------------------------------------------------------------------------

test('tf-badge: the hot tone is accepted', () => {
  const badge = new TfBadge();
  mount(badge, { tone: 'hot', value: '3' });
  assert.equal(badge.querySelector('span').className, 'tf-badge hot');
  assert.equal(badge.querySelector('span').textContent, '3');
});

test('tf-badge: the existing tones and the accent fallback are unchanged', () => {
  for (const tone of ['accent', 'danger', 'success', 'warning', 'info', 'neutral']) {
    const b = new TfBadge();
    mount(b, { tone, value: '1' });
    assert.equal(b.querySelector('span').className, `tf-badge ${tone}`);
  }
  const unknown = new TfBadge();
  mount(unknown, { tone: 'nope', value: '1' });
  assert.equal(unknown.querySelector('span').className, 'tf-badge accent');
});

test('tf-badge: the hot tone has a solid amber rule with dark text in controls.css', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  const rule = css.slice(css.indexOf('.tf-badge.hot {'));
  assert.match(rule.slice(0, 200), /background:\s*var\(--tf-warning\)/);
  assert.match(rule.slice(0, 200), /color:\s*#1a1200/);
  assert.match(rule.slice(0, 200), /animation:\s*tf-badge-pop/);
  assert.match(css, /@keyframes tf-badge-pop/);
});

// ---------------------------------------------------------------------------
// 5. tf-column — hide-below
// ---------------------------------------------------------------------------

function table(columns, rows, attrs = {}) {
  const t = new TfTable();
  for (const c of columns) {
    const col = new TfColumn();
    for (const [k, v] of Object.entries(c)) col.setAttribute(k, v);
    t.appendChild(col);
  }
  mount(t, attrs);
  t.rows = rows;
  return t;
}

function headerCells(t) {
  return [...t.shadowRoot.querySelectorAll('thead th')];
}
function bodyCells(t, rowIdx = 0) {
  return [...t.shadowRoot.querySelectorAll('tbody tr')[rowIdx].children];
}

test('tf-column: hide-below marks the matching th and td', () => {
  const t = table(
    [{ key: 'name', label: 'Nazwa' }, { key: 'node', label: 'Wezel', 'hide-below': '900' }],
    [{ name: 'a', node: 'gpu-01' }, { name: 'b', node: 'mac-studio' }],
  );
  assert.equal(headerCells(t)[0].classList.contains('tf-table__col--hide-below-900'), false);
  assert.ok(headerCells(t)[1].classList.contains('tf-table__col--hide-below-900'));
  assert.ok(bodyCells(t, 0)[1].classList.contains('tf-table__col--hide-below-900'));
  assert.ok(bodyCells(t, 1)[1].classList.contains('tf-table__col--hide-below-900'));
});

test('tf-column: the cells stay in the DOM so the table keeps its state', () => {
  const t = table(
    [{ key: 'name', label: 'Nazwa' }, { key: 'node', label: 'Wezel', 'hide-below': '900' }],
    [{ name: 'a', node: 'x' }, { name: 'b', node: 'y' }],
  );
  // Column count is identical with and without hide-below — nothing is dropped.
  assert.equal(headerCells(t).length, 2);
  assert.equal(bodyCells(t).length, 2);
  assert.equal(bodyCells(t)[1].textContent, 'x');
  assert.equal(t.columns.length, 2);
});

test('tf-column: hide-below does not force a row rebuild, so state survives', () => {
  const t = table(
    [{ key: 'name', label: 'Nazwa' }, { key: 'node', label: 'Wezel', 'hide-below': '900' }],
    [{ name: 'a', node: 'x' }],
  );
  const trBefore = t.shadowRoot.querySelector('tbody tr');
  // A marker on the recycled row stands in for whatever state the row carries.
  trBefore.dataset.marker = 'keep-me';
  t.rows = [{ name: 'a2', node: 'x2' }];
  const trAfter = t.shadowRoot.querySelector('tbody tr');
  assert.equal(trAfter, trBefore, 'row element recycled, not rebuilt');
  assert.equal(trAfter.dataset.marker, 'keep-me');
  assert.equal(trAfter.children[0].textContent, 'a2');
  assert.ok(trAfter.children[1].classList.contains('tf-table__col--hide-below-900'));
});

test('tf-column: an unsupported breakpoint leaves the column visible', () => {
  const t = table(
    [{ key: 'name', label: 'N' }, { key: 'x', label: 'X', 'hide-below': '777' }],
    [{ name: 'a', x: 'b' }],
  );
  assert.equal(t.columns[1].hideBelow, 0);
  assert.equal(
    [...headerCells(t)[1].classList].filter((c) => c.startsWith('tf-table__col--hide-below')).length,
    0,
  );
});

test('tf-column: a stale breakpoint class is dropped when the column changes', () => {
  const t = table(
    [{ key: 'name', label: 'N' }, { key: 'x', label: 'X', 'hide-below': '900' }],
    [{ name: 'a', x: 'b' }],
  );
  assert.ok(bodyCells(t)[1].classList.contains('tf-table__col--hide-below-900'));
  t.querySelectorAll('tf-column')[1].setAttribute('hide-below', '640');
  t.rows = [{ name: 'a', x: 'b' }];
  assert.equal(bodyCells(t)[1].classList.contains('tf-table__col--hide-below-900'), false);
  assert.ok(bodyCells(t)[1].classList.contains('tf-table__col--hide-below-640'));
});

test('tf-column: a table without hide-below is untouched', () => {
  const t = table([{ key: 'a', label: 'A' }, { key: 'b', label: 'B' }], [{ a: '1', b: '2' }]);
  for (const cell of [...headerCells(t), ...bodyCells(t)]) {
    assert.equal([...cell.classList].some((c) => c.includes('hide-below')), false);
  }
  assert.deepEqual(t.columns.map((c) => c.hideBelow), [0, 0]);
});

test('tf-table: actions-label names the trailing actions column', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1' }]);
  t.rowActions = () => document.createElement('span');
  const before = headerCells(t).at(-1);
  assert.equal(before.textContent, '');
  assert.equal(before.getAttribute('aria-label'), 'Akcje');

  t.setAttribute('actions-label', 'Akcje');
  const after = headerCells(t).at(-1);
  assert.ok(after.classList.contains('tf-table__actions-col'));
  assert.equal(after.textContent, 'Akcje');
  // The visible text IS the accessible name, so the redundant aria-label goes.
  assert.equal(after.getAttribute('aria-label'), null);
});

// ---------------------------------------------------------------------------
// tf-table — the actions cell is not rebuilt on every render
// ---------------------------------------------------------------------------
//
// `_updateRowCells` recycles the <tr> but called `_writeActionsCell`
// unconditionally, and that did `td.replaceChildren(el)` with a freshly built
// element every time. On a polled screen every action button in every row was
// therefore destroyed and recreated on a poll that moved nothing — a click
// target vanishing out from under the cursor mid-gesture.
//
// A guard on object identity cannot fix it: each poll builds brand-new row
// objects from fresh API data, so `row === lastRow` is never true.
// `rowActionsKey` let the host declare a signature instead — but that is an
// opt-in PROMISE: the caller lists the fields its markup and handlers read, and
// one left off the list silently leaves a kept element working on stale data.
// Measured: 42 rowActions call sites, exactly one of which supplied a key.
//
// So the staleness is made impossible rather than guarded. The builder receives
// a third argument, `currentRow()`, resolving the row in THIS slot at CALL
// time; handlers read through it instead of closing over the row they were
// built from. A rebuild producing identical markup can then be discarded — an
// element whose handlers resolve the current row is already correct for
// whatever row now sits there. `rowActionsKey` survives as an optional fast
// path that skips the build itself.
//
// Elements are compared by `===` and the result asserted as a BOOLEAN — see the
// note on `absent` in modules/tentanas.test.js for why a failing element
// comparison must never reach assert's differ.

test('tf-table: an unchanged actions signature keeps the very same element', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1', v: 1 }]);
  let built = 0;
  t.rowActionsKey = (row) => `${row.id}|${row.v}`;
  t.rowActions = (row) => {
    built += 1;
    const b = document.createElement('button');
    b.dataset.row = row.id;
    return b;
  };
  const el = bodyCells(t).at(-1).firstChild;
  assert.ok(el, 'the actions cell is filled on the first render');
  assert.equal(built, 1, 'built once');

  // A brand-new row OBJECT carrying identical values — what every poll hands in.
  t.rows = [{ a: '1', id: 'r1', v: 1 }];
  assert.equal(built, 1, 'the builder is not called again for an unchanged row');
  assert.equal(bodyCells(t).at(-1).firstChild === el, true, 'and the node is the same one');
});

test('tf-table: a new rowActions builder replaces the cells the old one produced', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1', v: 1 }]);
  t.rowActionsKey = (row) => `${row.id}|${row.v}`;
  t.rowActions = () => {
    const b = document.createElement('button');
    b.dataset.from = 'A';
    return b;
  };
  assert.equal(bodyCells(t).at(-1).firstChild.dataset.from, 'A');

  // Swapping the builder is how a host rewires handlers against new state — an
  // `isAdmin` that just changed, a fresh permission check. The cached signature
  // describes the ROW, not which builder made the element, so without
  // invalidating it the old element and its stale closure would live forever.
  t.rowActions = () => {
    const b = document.createElement('button');
    b.dataset.from = 'B';
    return b;
  };
  assert.equal(bodyCells(t).at(-1).firstChild.dataset.from, 'B', 'the new builder owns the cell');
});

test('tf-table: a signature that is neither a string nor a number keeps rebuilding', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1' }]);
  let built = 0;
  // A builder that returns nothing usable must count as "no signature" and NOT
  // as a match — otherwise every such cell would freeze on its first element.
  t.rowActionsKey = () => undefined;
  t.rowActions = () => {
    built += 1;
    return document.createElement('button');
  };
  assert.equal(built, 1, 'built on the first render');
  t.rows = [{ a: '1', id: 'r1' }];
  assert.equal(built, 2, 'and again, exactly as before the guard existed');
});

test('tf-table: an unchanged chip cell keeps its span, a changed one replaces it', () => {
  const t = table(
    [{ key: 'r', label: 'R', renderer: 'chip' }],
    [{ r: { status: 'ok', label: 'Zajety' } }],
  );
  const span = bodyCells(t)[0].firstElementChild;
  assert.ok(span, 'the chip is rendered');

  // A poll hands the same value in as a NEW object. Rewriting the cell anyway
  // swapped one span per row on every tick: measured on the live disks table,
  // 319 of the 329 DOM mutations left after the row-actions fix came from here.
  t.rows = [{ r: { status: 'ok', label: 'Zajety' } }];
  assert.equal(bodyCells(t)[0].firstElementChild === span, true, 'the same span survives a no-op poll');

  t.rows = [{ r: { status: 'warn', label: 'Zajety' } }];
  assert.equal(bodyCells(t)[0].firstElementChild === span, false, 'a changed status does replace it');
  assert.equal(bodyCells(t)[0].firstElementChild.className, 'tf-chip warn');
});

test('tf-table: a moved signature rebuilds the cell and binds it to the CURRENT row', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1', v: 1 }]);
  const fired = [];
  t.rowActionsKey = (row) => `${row.id}|${row.v}`;
  t.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const b = document.createElement('button');
    b.dataset.v = String(row.v);
    b.addEventListener('click', () => fired.push(`${live().id}:${live().v}`));
    return b;
  };
  const first = bodyCells(t).at(-1).firstChild;

  t.rows = [{ a: '1', id: 'r1', v: 2 }];
  const second = bodyCells(t).at(-1).firstChild;
  assert.equal(second === first, false, 'the markup moved with the signature, so the node is replaced');
  second.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.deepEqual(fired, ['r1:2'], 'the handler reads the row as it is NOW');
});

test('tf-table: a signature that omits the row identity can no longer strand a handler', () => {
  // Exactly the hazard rowActionsKey put on the caller: <tr> 0 is recycled but
  // now shows a DIFFERENT logical row, and a key that left the identity out
  // reads as "unchanged", so the builder never runs again. That used to strand
  // the handler on the row it was built for — a delete button quietly pointing
  // at the wrong record. Reading through currentRow() makes the bad key
  // harmless: the kept element still acts on the row that is actually there.
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1', v: 1 }]);
  const fired = [];
  t.rowActionsKey = (row) => `v:${row.v}`;
  t.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const b = document.createElement('button');
    b.addEventListener('click', () => fired.push(live().id));
    return b;
  };
  const first = bodyCells(t).at(-1).firstChild;

  // Same rendered actions, same `v`, different row.
  t.rows = [{ a: '2', id: 'r2', v: 1 }];
  assert.equal(bodyCells(t).at(-1).firstChild === first, true, 'the stale signature does keep the node');
  first.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.deepEqual(fired, ['r2'], 'and it still acts on the row now in that position');
});

test('tf-table: with no signature at all, a no-op poll still touches no node', () => {
  // The default path — 41 of the 42 rowActions call sites declare no signature.
  // The builder still RUNS on every render (the guard is on the DOM write, not
  // on the call), but an identical result is discarded and the element the user
  // may be reaching for is left exactly where it is.
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1' }]);
  let built = 0;
  t.rowActions = () => { built += 1; return document.createElement('span'); };
  const el = bodyCells(t).at(-1).firstChild;
  assert.ok(el, 'the actions cell is filled on the first render');
  assert.equal(built, 1);

  t.rows = [{ a: '1' }];
  assert.equal(built, 2, 'the builder runs again, exactly as before');
  assert.equal(bodyCells(t).at(-1).firstChild === el, true, 'but its identical output is discarded');
});

test('tf-table: a kept actions element acts on the row that is in its slot NOW', () => {
  // Markup identical for every row — the case a markup comparison alone cannot
  // tell apart, and the one a kebab menu really produces. A filter then drops
  // r1, so r2 slides into slot 0 underneath the very same element.
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1' }, { a: '2', id: 'r2' }]);
  const fired = [];
  t.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const b = document.createElement('button');
    b.addEventListener('click', () => fired.push(live().id));
    return b;
  };
  const first = bodyCells(t, 0).at(-1).firstChild;

  t.rows = [{ a: '2', id: 'r2' }];
  assert.equal(bodyCells(t, 0).at(-1).firstChild === first, true, 'the node is kept');
  first.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.deepEqual(fired, ['r2'], 'and it acts on the row now in that slot');
});

test('tf-table: a kept actions element reads the row object a poll just replaced', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1', n: 1 }]);
  let seen = null;
  t.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const b = document.createElement('button');
    b.addEventListener('click', () => { seen = live(); });
    return b;
  };
  const el = bodyCells(t).at(-1).firstChild;

  // Equal VALUES, brand-new object — what every poll hands in, and what object
  // identity can never distinguish from the row before it.
  const polled = { a: '1', id: 'r1', n: 1 };
  t.rows = [polled];
  assert.equal(bodyCells(t).at(-1).firstChild === el, true, 'nothing moved, so nothing is written');
  el.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.equal(seen === polled, true, 'the handler holds the row object that is live now');
});

test('tf-table: a kept actions element follows a sort that moved another row into its slot', () => {
  // currentRow() has to resolve through the SORTED view, the same one
  // _renderTbody wrote from — reading the unsorted `.rows` by index would hand
  // the handler a different record than the one its own <tr> displays.
  const t = table(
    [{ key: 'a', label: 'A', sortable: '' }],
    [{ a: 'b', id: 'r1' }, { a: 'a', id: 'r2' }],
    { sortable: '' },
  );
  const fired = [];
  t.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const b = document.createElement('button');
    b.addEventListener('click', () => fired.push(live().id));
    return b;
  };
  const first = bodyCells(t, 0).at(-1).firstChild;

  headerCells(t)[0].dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.equal(bodyCells(t, 0).at(-1).firstChild === first, true, 'sorting writes no actions node');
  first.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.deepEqual(fired, ['r2'], 'the top slot now belongs to r2 and the handler knows it');
});

test('tf-table: an actions element whose markup moved IS replaced', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1', paused: false }]);
  t.rowActions = (row) => {
    const b = document.createElement('button');
    b.setAttribute('icon', row.paused ? 'play' : 'pause');
    return b;
  };
  const first = bodyCells(t).at(-1).firstChild;

  t.rows = [{ a: '1', id: 'r1', paused: true }];
  const second = bodyCells(t).at(-1).firstChild;
  assert.equal(second === first, false, 'a changed icon is a changed cell');
  assert.equal(second.getAttribute('icon'), 'play');
});

test('tf-table: a new builder replaces the element even when the markup is identical', () => {
  // How a host rewires handlers against state that just moved — an `isAdmin`
  // flipping, a fresh permission check. The markup can be identical while the
  // closure is not, so the markup comparison alone must NOT keep the old node.
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1' }]);
  const fired = [];
  t.rowActions = () => {
    const b = document.createElement('button');
    b.addEventListener('click', () => fired.push('old'));
    return b;
  };
  const first = bodyCells(t).at(-1).firstChild;

  t.rowActions = () => {
    const b = document.createElement('button');
    b.addEventListener('click', () => fired.push('new'));
    return b;
  };
  const second = bodyCells(t).at(-1).firstChild;
  assert.equal(second === first, false, 'the new builder owns the cell');
  second.dispatchEvent(new MouseEvent('click', { bubbles: true }));
  assert.deepEqual(fired, ['new'], 'and the old closure is gone with it');
});

test('tf-table: a signature builder that throws falls back to rebuilding', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1' }]);
  let built = 0;
  t.rowActionsKey = () => { throw new Error('boom'); };
  t.rowActions = () => { built += 1; return document.createElement('span'); };
  assert.equal(built, 1);
  t.rows = [{ a: '1' }];
  assert.equal(built, 2, 'a broken signature must never read as "unchanged"');
});

test('tf-table: the actions column header is right-aligned in controls.css', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  const idx = css.indexOf('.tf-table th.tf-table__actions-col');
  assert.ok(idx > 0, 'header rule exists');
  assert.match(css.slice(idx, idx + 120), /text-align:\s*right/);
});

test('tf-column: every supported breakpoint has a matching rule in controls.css', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  for (const bp of [480, 640, 720, 900, 1024, 1180, 1280]) {
    const media = `@media (max-width: ${bp}px)`;
    const idx = css.lastIndexOf(`td.tf-table__col--hide-below-${bp}`);
    assert.ok(idx > 0, `td rule for ${bp} exists`);
    // The rule must sit inside the media query of the same width.
    const openIdx = css.lastIndexOf(media, idx);
    assert.ok(openIdx > 0 && openIdx < idx, `${bp}px rule lives in ${media}`);
    assert.match(css.slice(idx, idx + 60), /display:\s*none/);
  }
});

// ---------------------------------------------------------------------------
// 6. tf-agent-activity
// ---------------------------------------------------------------------------

function activity(attrs = {}) {
  return mount(new TfAgentActivity(), attrs);
}

test('tf-agent-activity: level="tree" renders the tree without a synthetic click', () => {
  const w = activity({ level: 'tree' });
  w.applyEvent({ kind: 'iteration_started', run_id: 'r1', agent: 'coder', n: 1 });
  assert.equal(w.level, 'tree');
  assert.ok(w.querySelector('.tf-aa-tree'), 'tree body rendered');
  assert.equal(w.querySelector('.tf-aa-bar'), null, 'no collapsed bar');
  assert.equal(w.querySelector('.tf-agent-activity').dataset.level, '1');
});

test('tf-agent-activity: the level property drives the attribute and back', () => {
  const w = activity();
  assert.equal(w.level, 'bar');
  w.level = 'tree';
  assert.equal(w.getAttribute('level'), 'tree');
  assert.equal(w.level, 'tree');
  w.setAttribute('level', 'bar');
  assert.equal(w.level, 'bar');
  w.level = 'nonsense';
  assert.equal(w.level, 'bar', 'an unknown level is ignored');
});

test('tf-agent-activity: level="tree" keeps the panel visible with no runs', () => {
  const w = activity({ level: 'tree' });
  assert.equal(w.querySelector('.tf-agent-activity').hidden, false);
  assert.ok(w.querySelector('.tf-aa-empty'));
});

test('tf-agent-activity: without the attribute the widget still auto-hides and expands', () => {
  const w = activity();
  assert.equal(w.querySelector('.tf-agent-activity').hidden, true, 'auto-hidden when idle');
  w.applyEvent({ kind: 'iteration_started', run_id: 'r1', agent: 'coder', n: 1 });
  assert.ok(w.querySelector('.tf-aa-bar'), 'collapsed bar by default');
  assert.equal(w.hasAttribute('level'), false, 'no attribute is written for uncontrolled hosts');
  w.querySelector('[data-action="expand"]').click();
  assert.ok(w.querySelector('.tf-aa-tree'));
  assert.equal(w.hasAttribute('level'), false);
});

test('tf-agent-activity: a controlled host sees internal navigation in the attribute', () => {
  const w = activity({ level: 'bar' });
  w.applyEvent({ kind: 'iteration_started', run_id: 'r1', agent: 'coder', n: 1 });
  w.querySelector('[data-action="expand"]').click();
  assert.equal(w.getAttribute('level'), 'tree');
  w.querySelector('[data-action="collapse"]').click();
  assert.equal(w.getAttribute('level'), 'bar');
});

test('tf-agent-activity: child_spawned sets the parent and the tree is deeper than one level', () => {
  const w = activity({ level: 'tree' });
  w.applyEvent({ kind: 'iteration_started', run_id: 'root', agent: 'lead', n: 1 });
  w.applyEvent({ kind: 'child_spawned', run_id: 'kid', scope: 'root', agent: 'tester' });

  const rows = [...w.querySelectorAll('.tf-aa-run')];
  assert.equal(rows.length, 2, 'both runs are in the tree');
  const depths = rows.map((r) => r.getAttribute('style'));
  assert.ok(depths.some((s) => /--depth:\s*0/.test(s)), 'a root at depth 0');
  assert.ok(depths.some((s) => /--depth:\s*1/.test(s)), 'a child at depth 1 — the tree is nested');

  const kidRow = rows.find((r) => r.dataset.run === 'kid');
  assert.ok(/--depth:\s*1/.test(kidRow.getAttribute('style')), 'the SPAWNED run is the nested one');
});

test('tf-agent-activity: a grandchild nests two levels deep', () => {
  const w = activity({ level: 'tree' });
  w.applyEvent({ kind: 'iteration_started', run_id: 'root', agent: 'lead', n: 1 });
  w.applyEvent({ kind: 'child_spawned', run_id: 'kid', scope: 'root', agent: 'tester' });
  w.applyEvent({ kind: 'child_spawned', run_id: 'grandkid', scope: 'kid', agent: 'fixer' });

  const rows = [...w.querySelectorAll('.tf-aa-run')];
  assert.equal(rows.length, 3);
  const gk = rows.find((r) => r.dataset.run === 'grandkid');
  assert.ok(/--depth:\s*2/.test(gk.getAttribute('style')));
});

test('tf-agent-activity: a child whose parent is unknown stays a visible root', () => {
  const w = activity({ level: 'tree' });
  // Joined mid-stream: no event ever introduced "unseen-root". Linking to it
  // would hide the child (the tree renders from roots down), so it stays a root.
  w.applyEvent({ kind: 'child_spawned', run_id: 'kid', scope: 'unseen-root', agent: 'tester' });
  const rows = [...w.querySelectorAll('.tf-aa-run')];
  assert.equal(rows.length, 1, 'no phantom parent row is invented');
  assert.equal(rows[0].dataset.run, 'kid', 'the child is not lost');
  assert.ok(/--depth:\s*0/.test(rows[0].getAttribute('style')));
});

test('tf-agent-activity: a scope-less child_spawned changes nothing (Code Studio feeds this)', () => {
  const w = activity({ level: 'tree' });
  w.applyEvent({ kind: 'child_spawned', run_id: 'kid', agent: 'tester' });
  const rows = [...w.querySelectorAll('.tf-aa-run')];
  assert.equal(rows.length, 1);
  assert.ok(/--depth:\s*0/.test(rows[0].getAttribute('style')));
});

test('tf-agent-activity: spawned runs count as background work on the bar', () => {
  // The background badge only exists in the narrow chat-audio bar.
  const w = activity();
  w.variant = 'chat-audio';   // variant is property-driven, not observed
  w.applyEvent({ kind: 'iteration_started', run_id: 'root', agent: 'lead', n: 1 });
  assert.equal(w.querySelector('.tf-aa-badge'), null, 'a lone root run is not background work');
  w.applyEvent({ kind: 'child_spawned', run_id: 'kid', scope: 'root', agent: 'tester' });
  assert.match(w.querySelector('.tf-aa-badge').textContent, /1 in background/);
});

test('tf-agent-activity: a scope-less event still creates a root run (unchanged)', () => {
  const w = activity({ level: 'tree' });
  w.applyEvent({ kind: 'tool_call_started', run_id: 'solo', name: 'fs_read' });
  const rows = [...w.querySelectorAll('.tf-aa-run')];
  assert.equal(rows.length, 1);
  assert.ok(/--depth:\s*0/.test(rows[0].getAttribute('style')));
});

test('tf-agent-activity: cards="off" hides the question card but keeps the amber dot', () => {
  const w = activity({ cards: 'off' });
  w.applyEvent({ kind: 'iteration_started', run_id: 'r1', agent: 'coder', n: 1 });
  w.setRunStatus('r1', 'waiting_user');

  assert.equal(w.querySelector('.tf-aa-card-question'), null, 'no question card');
  assert.ok(w.querySelector('.tf-aa-dot.is-waiting'), 'amber waiting dot present');
  assert.ok(w.querySelector('.tf-aa-bar.is-waiting'), 'the bar carries the waiting state');
  assert.equal(w.querySelector('.tf-agent-activity').hidden, false);
});

test('tf-agent-activity: cards="off" also suppresses a fed question and permission event', () => {
  const w = activity({ cards: 'off' });
  w.applyEvent({
    kind: 'user_question', run_id: 'r1', agent: 'coder',
    interaction_id: 'i1', question: 'Continue?', choices: ['yes', 'no'],
  });
  w.applyEvent({
    kind: 'permission_request', run_id: 'r2', agent: 'tester',
    interaction_id: 'i2', addon_id: 'notes', tool_name: 'write',
  });
  assert.equal(w.querySelector('.tf-aa-card-question'), null);
  assert.equal(w.querySelector('.tf-aa-card-perm'), null);
  assert.ok(w.querySelector('.tf-aa-dot.is-waiting'));
  assert.equal(w.hasWaiting(), true);
});

test('tf-agent-activity: without cards="off" the cards still render (unchanged)', () => {
  const w = activity();
  w.applyEvent({
    kind: 'user_question', run_id: 'r1', agent: 'coder',
    interaction_id: 'i1', question: 'Continue?', choices: ['yes', 'no'],
  });
  const card = w.querySelector('.tf-aa-card-question');
  assert.ok(card, 'question card renders by default');
  assert.match(card.textContent, /Continue\?/);
  assert.equal(card.querySelectorAll('tf-chip[data-choice]').length, 2);

  const seen = [];
  w.addEventListener('agent-reply', (e) => seen.push(e.detail));
  card.querySelector('tf-chip[data-choice="yes"]').click();
  assert.deepEqual(seen, [{ runId: 'r1', interactionId: 'i1', answer: 'yes' }]);
});

test('tf-agent-activity: cards can be re-enabled by dropping the attribute', () => {
  const w = activity({ cards: 'off' });
  w.applyEvent({
    kind: 'user_question', run_id: 'r1', agent: 'coder',
    interaction_id: 'i1', question: 'Q?', choices: [],
  });
  assert.equal(w.querySelector('.tf-aa-card-question'), null);
  w.removeAttribute('cards');
  assert.ok(w.querySelector('.tf-aa-card-question'));
});

test('tf-agent-activity: cancel and open-run still emit their events', () => {
  const w = activity({ level: 'tree' });
  w.applyEvent({ kind: 'iteration_started', run_id: 'r1', agent: 'coder', n: 1 });
  const cancels = [];
  const opens = [];
  w.addEventListener('agent-cancel', (e) => cancels.push(e.detail.runId));
  w.addEventListener('agent-open-run', (e) => opens.push(e.detail.runId));
  w.querySelector('[data-action="cancel-run"]').click();
  w.querySelector('[data-action="open-run"]').click();
  assert.deepEqual(cancels, ['r1']);
  assert.deepEqual(opens, ['r1']);
  assert.equal(w.level, 'detail');
  assert.ok(w.querySelector('.tf-aa-timeline'), 'the timeline renders at level 2');
});

// ---------------------------------------------------------------------------
// 7. tf-select / tf-button — light DOM written AFTER the upgrade
// ---------------------------------------------------------------------------

test('tf-select: options assigned after the upgrade end up inside the select', async () => {
  const select = mount(new TfSelect(), { value: 'a' });
  select.innerHTML = '<option value="a">A</option><option value="b">B</option>';
  await flush();

  const inner = select.querySelector('select.tf-select');
  assert.ok(inner, 'the built select survived the innerHTML write');
  assert.deepEqual([...inner.options].map((o) => o.value), ['a', 'b']);
  assert.equal(select.querySelector(':scope > option'), null, 'no option is left loose');
  assert.equal(inner.value, 'a', 'the value attribute is re-applied to the rebuilt select');
});

test('tf-select: an option appended later joins the existing options', async () => {
  const select = mount(new TfSelect());
  select.setOptions([{ value: 'a', label: 'A' }], 'a');
  const extra = document.createElement('option');
  extra.value = 'b';
  extra.textContent = 'B';
  select.appendChild(extra);
  await flush();

  const inner = select.querySelector('select.tf-select');
  assert.deepEqual([...inner.options].map((o) => o.value), ['a', 'b']);
});

test('tf-select: setOptions and light-DOM options still build the same select', () => {
  const declarative = new TfSelect();
  declarative.innerHTML = '<option value="x">X</option>';
  mount(declarative);
  assert.deepEqual([...declarative.querySelector('select').options].map((o) => o.value), ['x']);

  const programmatic = mount(new TfSelect());
  programmatic.setOptions([{ value: 'x', label: 'X' }], 'x');
  assert.deepEqual([...programmatic.querySelector('select').options].map((o) => o.value), ['x']);
});

test('tf-button: textContent written after the upgrade keeps a real button', async () => {
  const btn = mount(new TfButton(), { variant: 'primary' });
  btn.textContent = 'Dalej';
  await flush();

  const inner = btn.querySelector('button');
  assert.ok(inner, 'the component rebuilt its button');
  assert.equal(inner.className, 'tf-btn tf-btn-primary');
  assert.equal(inner.textContent, 'Dalej');
  assert.equal([...btn.childNodes].length, 1, 'no bare text node is left next to the button');
});

test('tf-button: the label attribute stays the direct text channel', () => {
  const btn = mount(new TfButton(), { variant: 'primary', label: 'Dalej' });
  assert.equal(btn.querySelector('button').textContent, 'Dalej');
  btn.setAttribute('label', 'Załóż workspace');
  assert.equal(btn.querySelector('button').textContent, 'Załóż workspace');
  assert.equal(btn.querySelector('button').className, 'tf-btn tf-btn-primary');
});

// ---------------------------------------------------------------------------
// 8. Icon sprite
// ---------------------------------------------------------------------------

test('index.html: the sprite carries every symbol Code Studio references', () => {
  const html = readFileSync(join(WWW_ROOT, 'index.html'), 'utf8');
  const ids = new Set([...html.matchAll(/<symbol[^>]*id="i-([a-z0-9_-]+)"/g)].map((m) => m[1]));
  for (const name of ['terminal', 'git', 'bot', 'file', 'save', 'layers',
    'arrow-left', 'check-circle', 'flask']) {
    assert.ok(ids.has(name), `symbol i-${name} exists`);
  }
});

test('index.html: the new symbols follow the sprite conventions', () => {
  const html = readFileSync(join(WWW_ROOT, 'index.html'), 'utf8');
  for (const name of ['git', 'bot', 'file', 'save', 'layers',
    'arrow-left', 'check-circle', 'flask']) {
    const m = new RegExp(`<symbol id="i-${name}"([^>]*)>`).exec(html);
    assert.ok(m, `i-${name} declared`);
    assert.match(m[1], /viewBox="0 0 24 24"/, `i-${name} uses the 24x24 box`);
    // Stroke/fill come from the sprite <svg> root, so a symbol must not re-declare them.
    assert.equal(/fill=|stroke=/.test(m[1]), false, `i-${name} inherits stroke/fill`);
  }
});

test('apps-home: the icon whitelist covers the new symbols and stays sprite-backed', () => {
  const src = readFileSync(join(WWW_ROOT, 'js', 'modules', 'apps-home.js'), 'utf8');
  const block = /const ICON_WHITELIST = new Set\(\[([\s\S]*?)\]\);/.exec(src)[1];
  const names = [...block.matchAll(/'([^']+)'/g)].map((m) => m[1]);
  for (const name of ['terminal', 'git', 'bot', 'file', 'save', 'layers',
    'arrow-left', 'check-circle', 'flask']) {
    assert.ok(names.includes(name), `${name} is whitelisted`);
  }
  assert.equal(new Set(names).size, names.length, 'no duplicate entries');

  const html = readFileSync(join(WWW_ROOT, 'index.html'), 'utf8');
  const ids = new Set([...html.matchAll(/<symbol[^>]*id="i-([a-z0-9_-]+)"/g)].map((m) => m[1]));
  const orphans = names.filter((n) => !ids.has(n));
  assert.deepEqual(orphans, [], 'every whitelisted icon has a sprite symbol');
});

test('apps-home + app.js: both icon whitelists stay in sync', () => {
  // app.js gates icon names coming from an UNTRUSTED addon manifest, apps-home
  // gates our own tiles; the comment in app.js calls them synchronised, so a
  // silent drift means an addon naming a real sprite icon renders 'apps'.
  const read = (file, name) => {
    const src = readFileSync(join(WWW_ROOT, file), 'utf8');
    const block = new RegExp(`const ${name} = new Set\\(\\[([\\s\\S]*?)\\]\\);`).exec(src)[1];
    return [...block.matchAll(/'([^']+)'/g)].map((m) => m[1]);
  };
  const addon = read('js/app.js', 'ADDON_ICON_WHITELIST');
  const home = read('js/modules/apps-home.js', 'ICON_WHITELIST');
  assert.deepEqual(addon, home);
});

// ---------------------------------------------------------------------------
// 8. tf-filter-chips — the scroll signal
// ---------------------------------------------------------------------------

test('tf-filter-chips: a scrolling row publishes its overflow side', async () => {
  const { TfFilterChips } = await import('./tf-filter-chips.js');
  const chips = mount(new TfFilterChips(), { scroll: '' });
  chips.filters = [{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }];
  const box = chips.querySelector('.tf-filter-chips');
  // Under a zero-layout DOM nothing overflows, so the honest answer is "none" —
  // what is pinned here is that the state is PUBLISHED at all.
  assert.equal(box.getAttribute('data-overflow'), 'none');
});

test('tf-filter-chips: a wrapping row publishes nothing (no edge to fade)', async () => {
  const { TfFilterChips } = await import('./tf-filter-chips.js');
  const chips = mount(new TfFilterChips());
  chips.filters = [{ id: 'a', label: 'A' }];
  assert.equal(chips.querySelector('.tf-filter-chips').hasAttribute('data-overflow'), false);
});

test('tf-filter-chips: the overflow state re-computes on scroll', async () => {
  const { TfFilterChips } = await import('./tf-filter-chips.js');
  const chips = mount(new TfFilterChips(), { scroll: '' });
  chips.filters = [{ id: 'a', label: 'A' }];
  const box = chips.querySelector('.tf-filter-chips');
  box.removeAttribute('data-overflow');
  box.dispatchEvent(new window.Event('scroll'));
  assert.equal(box.getAttribute('data-overflow'), 'none');
});

test('tf-filter-chips: controls.css fades the side that actually has more', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  for (const side of ['end', 'start', 'both']) {
    const rule = new RegExp(
      `tf-filter-chips\\[scroll\\] \\.tf-filter-chips\\[data-overflow="${side}"\\][^}]*mask-image`,
    );
    assert.match(css, rule, `${side} edge is faded`);
  }
});

// A poll chain hands the same filters back every few seconds — TentaNas
// rebuilds its disk bar every 5 s because the counts live in the chip labels.
// Rewriting innerHTML there destroys and recreates every button: the one under
// the cursor loses its hover and :active styling mid-press, and a click landing
// in that window is delivered to a node already detached from the document.
test('tf-filter-chips: an identical render leaves the chips alone', async () => {
  const { TfFilterChips } = await import('./tf-filter-chips.js');
  const chips = mount(new TfFilterChips());
  const bar = [{ id: 'all', label: 'Wszystkie 12', active: true }, { id: 'problems', label: 'Problemy 2' }];
  chips.filters = bar;
  const buttons = [...chips.querySelectorAll('.tf-filter-chip')];
  assert.equal(buttons.length, 2);

  chips.filters = bar.map((f) => ({ ...f }));
  const after = [...chips.querySelectorAll('.tf-filter-chip')];
  assert.equal(after.length, 2);
  after.forEach((el, i) => assert.equal(el === buttons[i], true, `chip ${i} is the same element`));

  // A count that genuinely moved must still re-render.
  chips.filters = [{ id: 'all', label: 'Wszystkie 13', active: true }, { id: 'problems', label: 'Problemy 2' }];
  assert.equal(chips.querySelector('.tf-filter-chip') === buttons[0], false, 'a changed label rebuilds the bar');
  assert.equal(chips.querySelectorAll('.tf-filter-chip')[0].textContent, 'Wszystkie 13');
  chips.remove();
});

test('tf-filter-chips: the guard does not freeze the selection a click makes', async () => {
  const { TfFilterChips } = await import('./tf-filter-chips.js');
  const chips = mount(new TfFilterChips(), { mode: 'single' });
  chips.filters = [{ id: 'all', label: 'A', active: true }, { id: 'open', label: 'B' }];
  chips.querySelectorAll('.tf-filter-chip')[1].dispatchEvent(new window.Event('click', { bubbles: true }));
  const after = [...chips.querySelectorAll('.tf-filter-chip')];
  assert.equal(after[1].classList.contains('active'), true, 'the clicked chip became active');
  assert.equal(after[0].classList.contains('active'), false, 'and the previous one gave it up');
  chips.remove();
});

// disconnectedCallback drops the ResizeObserver, but connectedCallback only
// built one when there was no container yet — and a re-attached element still
// has its container, because it moved with the element. A bar that is detached
// and put back (a tab switch that re-parents its toolbar) therefore never
// regained overflow tracking, and its edge fade froze on whatever the last
// resize before the detach happened to say.
test('tf-filter-chips: a re-attached bar regains its overflow tracking', async () => {
  const { TfFilterChips } = await import('./tf-filter-chips.js');
  const chips = mount(new TfFilterChips(), { scroll: '' });
  chips.filters = [{ id: 'a', label: 'A' }, { id: 'b', label: 'B' }];
  const box = chips.querySelector('.tf-filter-chips');
  assert.ok(chips._observer, 'the bar tracks its own width while attached');

  chips.remove();
  assert.equal(chips._observer, null, 'and lets the observer go while detached');

  document.body.appendChild(chips);
  assert.equal(chips.querySelector('.tf-filter-chips') === box, true, 'the container came back with the element');
  assert.ok(chips._observer, 'a re-attached bar observes its width again');
  assert.equal(box.getAttribute('data-overflow'), 'none', 'and still publishes its overflow state');
  chips.remove();
});

// ---------------------------------------------------------------------------
// 9. tf-menu — the compact panel for a row-anchored menu
// ---------------------------------------------------------------------------

test('tf-menu: compact drops the dialog min-width and tightens the item box', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  assert.match(css, /:host\(\[compact\]\) \.tf-menu \{[^}]*min-width: 0/);
  assert.match(css, /tf-menu\[compact\] \.tf-menu-item \{[^}]*padding: 6px 10px/);
  // The default panel keeps its floor — compact is opt-in, not a global shrink.
  assert.match(css, /\.tf-menu \{[^}]*min-width: 180px/);
});

// ---------------------------------------------------------------------------
// 10. tf-window — the close control sits top-right
// ---------------------------------------------------------------------------

test('tf-window: the controls group is the LAST header child', async () => {
  const { TfWindow } = await import('./tf-window.js');
  const win = mount(new TfWindow(), { title: 'X' });
  const header = win.shadowRoot.querySelector('.tf-window-header');
  const classes = [...header.children].map((c) => c.className);
  assert.deepEqual(classes, ['tf-window-title', 'tf-window-actions', 'tf-window-controls']);
});

test('tf-window: the default trio ends with close, so the corner closes', async () => {
  const { TfWindow } = await import('./tf-window.js');
  const win = mount(new TfWindow(), { title: 'X' });
  const actions = [...win.shadowRoot.querySelectorAll('.tf-window-control')]
    .map((b) => b.dataset.action);
  assert.deepEqual(actions, ['minimize', 'maximize', 'close']);
});

test('tf-window: a dialog still renders exactly one control, and it closes', async () => {
  const { TfWindow } = await import('./tf-window.js');
  const win = mount(new TfWindow(), { title: 'X', buttons: 'close' });
  const actions = [...win.shadowRoot.querySelectorAll('.tf-window-control')]
    .map((b) => b.dataset.action);
  assert.deepEqual(actions, ['close']);
});

test('tf-window: controls.css spaces the group from the title on its left', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  assert.match(css, /\.tf-window-controls \{[^}]*margin-left: 4px/);
  assert.doesNotMatch(css, /\.tf-window-controls \{[^}]*margin-right/);
});

// ---------------------------------------------------------------------------
// 11. tf-slider — aria-label reaches the input a screen reader focuses
// ---------------------------------------------------------------------------

test('tf-slider: aria-label is forwarded to the inner range input', async () => {
  const { TfSlider } = await import('./tf-slider.js');
  const slider = mount(new TfSlider(), { min: '0', max: '10', value: '4', 'aria-label': 'Krok' });
  const input = slider.querySelector('input[type="range"]');
  assert.equal(input.getAttribute('aria-label'), 'Krok');
  // The label follows the host, including its removal.
  slider.setAttribute('aria-label', 'Step');
  assert.equal(input.getAttribute('aria-label'), 'Step');
  slider.removeAttribute('aria-label');
  assert.equal(input.hasAttribute('aria-label'), false);
});

test('tf-slider: the pre-existing value/track behaviour is unchanged', async () => {
  const { TfSlider } = await import('./tf-slider.js');
  const slider = mount(new TfSlider(), { min: '0', max: '10', value: '4' });
  const input = slider.querySelector('input[type="range"]');
  assert.equal(input.value, '4');
  assert.equal(input.style.getPropertyValue('--tf-slider-pct'), '40%');
  slider.value = 8;
  assert.equal(input.value, '8');
  assert.equal(slider.getAttribute('value'), '8');
  assert.equal(input.style.getPropertyValue('--tf-slider-pct'), '80%');
});

// ---------------------------------------------------------------------------
// 12. tf-window — the opt-in bottom sheet on phones
// ---------------------------------------------------------------------------

test('tf-window: the `sheet` variant docks to the bottom edge below 640px', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  const idx = css.indexOf(':host([sheet]) .tf-window {');
  assert.ok(idx > 0, 'the sheet rule exists');
  // It must live inside the phone breakpoint — on a desktop a window stays a
  // floating dialog.
  const openIdx = css.lastIndexOf('@media (max-width: 640px)', idx);
  assert.ok(openIdx > 0 && openIdx < idx, 'the sheet rule sits in the 640px media query');
  const rule = css.slice(idx, css.indexOf('}', idx));
  assert.match(rule, /bottom:\s*0\s*!important/);
  assert.match(rule, /top:\s*auto\s*!important/);
  // The centred phone rule pins `transform` with !important, which outranks an
  // animation — so the entry animates `translate` instead.
  assert.match(rule, /transform:\s*none\s*!important/);
  const frames = css.indexOf('@keyframes tf-window-sheet-up');
  assert.ok(frames > 0, 'the slide-up keyframes exist');
  assert.match(css.slice(frames, frames + 160), /translate:\s*0\s*24px/);
});

test('tf-window: the sheet is opt-in — a plain window keeps the centred phone treatment', () => {
  const css = readFileSync(join(WWW_ROOT, 'css', 'controls.css'), 'utf8');
  // The centred rule is the one immediately above the sheet variant.
  const sheetIdx = css.indexOf(':host([sheet]) .tf-window {');
  const idx = css.lastIndexOf('.tf-window {', sheetIdx);
  const rule = css.slice(idx, css.indexOf('}', idx));
  assert.match(rule, /top:\s*50%\s*!important/, 'the default stays vertically centred');
});

// ---------------------------------------------------------------------------
// 13. tf-select — setOptions replaces the light-DOM options too
// ---------------------------------------------------------------------------

test('tf-select: setOptions drops markup options that were not adopted yet', async () => {
  await import('/js/components/tf-select.js');
  const host = document.createElement('div');
  document.body.appendChild(host);
  // The shape every async caller has: markup first (its mutation record is not
  // delivered yet), then the real list off the wire.
  host.innerHTML = '<tf-select value="a"><option value="a">A</option><option value="b">B</option></tf-select>';
  const select = host.querySelector('tf-select');
  select.setOptions([{ value: 'x', label: 'X' }, { value: 'y', label: 'Y', disabled: true }], 'x');
  // Let the observer run: an un-adopted <option> must not append itself after.
  await new Promise((resolve) => setTimeout(resolve, 0));
  const values = [...select.querySelectorAll('option')].map((o) => o.value);
  assert.deepEqual(values, ['x', 'y'], 'the list is replaced, not extended');
  assert.equal(select.querySelector('option[value="y"]').disabled, true);
  assert.equal(select.value, 'x');
  host.remove();
});

test('tf-select: markup options still reach the select when nothing replaces them', async () => {
  await import('/js/components/tf-select.js');
  const host = document.createElement('div');
  document.body.appendChild(host);
  host.innerHTML = '<tf-select value="b"><option value="a">A</option><option value="b">B</option></tf-select>';
  const select = host.querySelector('tf-select');
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.deepEqual([...select.querySelectorAll('option')].map((o) => o.value), ['a', 'b']);
  assert.equal(select.value, 'b');
  host.remove();
});

// The guarantee that vanished when the markup comparison replaced the opt-in
// signature: a builder written the OLD way never asked for the live row, so it
// closes over the row it was built from. Keeping its element would hand its
// handlers data from a poll ago with no visible symptom. tf-table therefore
// asks whether the builder requested the accessor at all.
test('tf-table: a builder that never asked for the live row keeps being rebuilt', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1' }]);
  let built = 0;
  // Two declared parameters — the shape every caller had before the accessor.
  t.rowActions = (row, idx) => {
    built += 1;
    const b = document.createElement('button');
    b.dataset.row = row.id;
    b.dataset.idx = String(idx);
    return b;
  };
  const first = bodyCells(t).at(-1).firstChild;
  assert.ok(first, 'the actions cell is filled');
  assert.equal(built, 1, 'built once');

  // A fresh row object carrying identical values: the markup comparison alone
  // would keep the node here, which is exactly what must NOT happen.
  t.rows = [{ a: '1', id: 'r1' }];
  assert.equal(built, 2, 'the builder ran again');
  assert.equal(bodyCells(t).at(-1).firstChild === first, false, 'and its element was replaced');
});

// The other half: the check must not cost a MIGRATED caller its guard, or the
// whole refactor is undone without a single test noticing.
test('tf-table: a builder that asked for the live row still keeps its element', () => {
  const t = table([{ key: 'a', label: 'A' }], [{ a: '1', id: 'r1' }]);
  let built = 0;
  t.rowActions = (row, idx, currentRow) => {
    built += 1;
    const b = document.createElement('button');
    b.dataset.row = (currentRow?.() ?? row).id;
    return b;
  };
  const first = bodyCells(t).at(-1).firstChild;
  assert.equal(built, 1, 'built once');

  t.rows = [{ a: '1', id: 'r1' }];
  assert.equal(bodyCells(t).at(-1).firstChild === first, true, 'the node survives for a migrated builder');
});
