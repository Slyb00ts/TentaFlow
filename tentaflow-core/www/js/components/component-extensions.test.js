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
