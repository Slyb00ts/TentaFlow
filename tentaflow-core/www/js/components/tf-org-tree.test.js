// =============================================================================
// File: components/tf-org-tree.test.js
// Description: The organization chart component: what is drawn by default (three
//   expanded levels, "+N" on the folded rest), expansion, selection, the ARIA
//   tree and its keyboard, the path highlight, units view, level of detail,
//   drawing only what is near the viewport, pan / zoom, and the standalone SVG.
// =============================================================================

import '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { window } = await import('../sdk-runtime/_dom-test-harness.js');
await import('./tf-org-tree.js');
const { buildTreeModel, pathTo } = await import('../modules/org-structure/tree.js');
const { labels, meHex, sampleView, syntheticView, t } = await import('../modules/org-structure/tree-fixture.js');

const model = () => buildTreeModel(sampleView(), { t, meHex });
const settle = () => new Promise((resolve) => setTimeout(resolve, 40));

function mount(m = model()) {
  document.body.innerHTML = '';
  const el = document.createElement('tf-org-tree');
  document.body.appendChild(el);
  el.labels = labels;
  el.model = m;
  el._render();
  return el;
}

function pointer(target, type, props = {}) {
  const event = new window.Event(type, { bubbles: true, cancelable: true });
  Object.assign(event, { pointerId: 1, pointerType: 'mouse', button: 0, clientX: 0, clientY: 0, ...props });
  target.dispatchEvent(event);
}

function click(el, target) {
  pointer(target, 'pointerdown');
  pointer(target, 'pointerup');
  return el;
}

function key(el, name, extra = {}) {
  el.querySelector('.tf-orgtree__svg').dispatchEvent(new window.KeyboardEvent('keydown', { key: name, bubbles: true, cancelable: true, ...extra }));
}

const card = (el, id) => el.querySelector(`.ot-card[data-node="${id}"]`);
const ids = (el) => [...el.querySelectorAll('.ot-card')].map((c) => c.dataset.node);

test('the default view shows three levels expanded and folds the rest behind a "+N" pill', () => {
  const el = mount();
  assert.ok(card(el, 'p-ceo') && card(el, 'p-delivery') && card(el, 'p-lead'));
  assert.equal(card(el, 'p-dev1'), null, 'the fourth level down is folded');
  const pill = el.querySelector('.ot-more[data-more="p-lead"]');
  assert.ok(pill);
  assert.equal(pill.textContent.trim().replace(/\s+/g, ' ').includes('+5'), true);
  assert.equal(card(el, 'p-lead').getAttribute('aria-expanded'), 'false');
  assert.equal(card(el, 'p-ceo').getAttribute('aria-expanded'), 'true');
});

test('cards are tree items with level and selection, and the chart is a labelled tree', () => {
  const el = mount();
  const svg = el.querySelector('.tf-orgtree__svg');
  assert.equal(svg.getAttribute('role'), 'tree');
  assert.equal(svg.getAttribute('aria-label'), 'Org chart');
  assert.equal(card(el, 'p-ceo').getAttribute('role'), 'treeitem');
  assert.equal(card(el, 'p-ceo').getAttribute('aria-level'), '1');
  assert.equal(card(el, 'p-lead').getAttribute('aria-level'), '4');
  assert.equal(card(el, 'p-assistant').getAttribute('aria-level'), '2', 'staff hang one level under their manager');
  assert.equal(card(el, 'p-ceo').getAttribute('aria-selected'), 'false');
});

test('a card says who it is, what they do and carries its badges', () => {
  const el = mount();
  const lead = card(el, 'p-lead');
  assert.match(lead.textContent, /Anna Kowalska/);
  assert.match(lead.textContent, /Kierownik zespołu/);
  assert.match(lead.textContent, /You/);
  assert.match(card(el, 'p-assistant').textContent, /staff/);
  assert.match(card(el, 'p-cto-deputy').textContent, /deputy/);
});

test('clicking the "+N" pill expands the branch and stacks a leaf team under its manager', () => {
  const el = mount();
  click(el, el.querySelector('.ot-more[data-more="p-lead"] rect'));
  el._render();
  assert.ok(card(el, 'p-dev1') && card(el, 'p-tester-auto'));
  assert.equal(card(el, 'p-lead').getAttribute('aria-expanded'), 'true');
  assert.equal(el.querySelector('.ot-more[data-more="p-lead"]'), null);
  const lead = el.layout ?? el._layout.byId.get('p-lead');
  const dev1 = el._layout.byId.get('p-dev1');
  assert.equal(dev1.role, 'stacked');
  assert.ok(dev1.x > lead.x, 'the team is indented under the manager');
});

test('the count pill of an expanded manager collapses it again', () => {
  const el = mount();
  click(el, el.querySelector('.ot-card[data-node="p-cto"] .ot-count rect'));
  el._render();
  assert.equal(card(el, 'p-delivery'), null);
  assert.ok(el.querySelector('.ot-more[data-more="p-cto"]'));
});

test('clicking a card selects it and reports the id, and a drag does not', () => {
  const el = mount();
  const events = [];
  el.addEventListener('node-select', (e) => events.push(e.detail));
  click(el, card(el, 'p-sales').querySelector('.ot-name'));
  assert.deepEqual(events, [{ id: 'p-sales', kind: 'position' }]);
  assert.equal(el.selectedId, 'p-sales');
  el._render();
  assert.match(card(el, 'p-sales').getAttribute('class'), /ot-selected/);
  assert.equal(card(el, 'p-sales').getAttribute('aria-selected'), 'true');

  const svg = el.querySelector('.tf-orgtree__svg');
  const before = el._view.x;
  pointer(card(el, 'p-fin'), 'pointerdown', { clientX: 100, clientY: 100 });
  pointer(svg, 'pointermove', { clientX: 160, clientY: 120 });
  pointer(svg, 'pointerup', { clientX: 160, clientY: 120 });
  assert.equal(events.length, 1, 'a drag ending on a card is not a click');
  assert.equal(el._view.x, before + 60);
});

test('a double click opens the node', () => {
  const el = mount();
  const opened = [];
  el.addEventListener('node-open', (e) => opened.push(e.detail));
  card(el, 'p-fin').dispatchEvent(new window.MouseEvent('dblclick', { bubbles: true }));
  assert.deepEqual(opened, [{ id: 'p-fin', kind: 'position' }]);
});

test('arrow keys walk parent, first child and siblings; Enter folds and unfolds', () => {
  const el = mount();
  const active = () => el._activeId;
  key(el, 'ArrowDown');
  assert.equal(active(), 'p-ceo', 'the first key lands on the root');
  key(el, 'ArrowDown');
  assert.equal(active(), 'p-cto');
  key(el, 'ArrowRight');
  assert.equal(active(), 'p-sales');
  key(el, 'ArrowRight');
  assert.equal(active(), 'p-fin');
  key(el, 'ArrowRight');
  assert.equal(active(), 'p-assistant', 'the staff follow the reports');
  key(el, 'ArrowRight');
  assert.equal(active(), 'p-assistant', 'no sibling beyond the last');
  key(el, 'ArrowLeft');
  key(el, 'ArrowLeft');
  assert.equal(active(), 'p-sales');
  key(el, 'ArrowUp');
  assert.equal(active(), 'p-ceo');
  key(el, 'ArrowUp');
  assert.equal(active(), 'p-ceo', 'the root has no parent');

  key(el, 'Enter');
  el._render();
  assert.equal(card(el, 'p-ceo').getAttribute('aria-expanded'), 'false');
  assert.equal(card(el, 'p-cto'), null);
  key(el, 'Enter');
  el._render();
  assert.ok(card(el, 'p-cto'));
});

test('arrow down on a folded card unfolds it and enters its first child', () => {
  const el = mount();
  el._activeId = 'p-lead';
  key(el, 'ArrowDown');
  el._render();
  assert.equal(el._activeId, 'p-lead-deputy');
  assert.ok(card(el, 'p-lead-deputy'));
});

test('the active card is announced through aria-activedescendant', () => {
  const el = mount();
  key(el, 'ArrowDown');
  key(el, 'ArrowDown');
  el._render();
  const svg = el.querySelector('.tf-orgtree__svg');
  assert.equal(svg.getAttribute('aria-activedescendant'), card(el, 'p-cto').id);
});

test('space selects the active card and Enter on a leaf opens it', () => {
  const el = mount();
  const selected = [];
  const opened = [];
  el.addEventListener('node-select', (e) => selected.push(e.detail));
  el.addEventListener('node-open', (e) => opened.push(e.detail));
  el._activeId = 'p-fin-1';
  key(el, ' ');
  assert.deepEqual(selected, [{ id: 'p-fin-1', kind: 'position' }]);
  key(el, 'Enter');
  assert.deepEqual(opened, [{ id: 'p-fin-1', kind: 'position' }]);
});

test('staff are reachable by the arrows next to their manager', () => {
  const el = mount();
  el._activeId = 'p-fin';
  key(el, 'ArrowRight');
  assert.equal(el._activeId, 'p-assistant');
  key(el, 'ArrowUp');
  assert.equal(el._activeId, 'p-ceo');
  el._activeId = 'p-assistant';
  key(el, 'ArrowDown');
  assert.equal(el._activeId, 'p-assistant', 'a staff card has nothing below it');
});

test('the path is highlighted and everything above the target is expanded to show it', () => {
  const m = model();
  const el = mount(m);
  assert.equal(card(el, 'p-dev1'), null);
  el.pathIds = pathTo(m, 'p-dev1');
  el._render();
  assert.ok(card(el, 'p-dev1'), 'the found person is on screen');
  for (const id of ['p-ceo', 'p-cto', 'p-delivery', 'p-lead', 'p-dev1']) {
    assert.match(card(el, id).getAttribute('class'), /ot-path/, id);
  }
  assert.doesNotMatch(card(el, 'p-sales').getAttribute('class'), /ot-path/);
  assert.ok(el.querySelector('.ot-lines path.hl').getAttribute('d').length > 0, 'the connectors of the path are drawn highlighted');
  el.pathIds = [];
  el._render();
  assert.equal(el.querySelector('.ot-lines path.hl').getAttribute('d'), '');
});

test('matches are marked on the cards', () => {
  const el = mount();
  el.matchIds = ['p-sales', 'p-fin'];
  el._render();
  assert.match(card(el, 'p-sales').getAttribute('class'), /ot-match/);
  assert.match(card(el, 'p-fin').getAttribute('class'), /ot-match/);
  assert.doesNotMatch(card(el, 'p-cto').getAttribute('class'), /ot-match/);
});

test('functional lines are drawn only when switched on', () => {
  const el = mount();
  el.pathIds = pathTo(model(), 'p-dev1');
  el._render();
  assert.equal(el.querySelector('.ot-functional').innerHTML, '');
  el.functional = true;
  el._render();
  assert.match(el.querySelector('.ot-functional path').getAttribute('d'), /^M/);
});

test('staff are drawn to the side of their manager with a horizontal line', () => {
  const el = mount();
  const ceo = el._layout.byId.get('p-ceo');
  const assistant = el._layout.byId.get('p-assistant');
  assert.equal(assistant.y, ceo.y);
  assert.ok(assistant.x > ceo.x + ceo.w);
  assert.match(el.querySelector('.ot-lines path.staff').getAttribute('d'), /^M[\d.]+ [\d.]+H[\d.]+$/);
});

test('a vacancy is a dashed card with no initials', () => {
  const el = mount();
  el.pathIds = pathTo(model(), 'p-tester-auto');
  el._render();
  const vacancy = card(el, 'p-tester-auto');
  assert.match(vacancy.getAttribute('class'), /ot-vacant/);
  assert.ok(vacancy.querySelector('.ot-av-vac'));
  assert.equal(vacancy.querySelector('.ot-av-text'), null);
  assert.match(vacancy.textContent, /vacancy/);
});

test('the units view draws a frame per unit with its head and team', () => {
  const el = mount();
  resizeTo(el, 3200, 1600);
  el.mode = 'units';
  el._render();
  const frames = [...el.querySelectorAll('.ot-frame')].map((f) => f.dataset.unit);
  assert.deepEqual(frames.sort(), ['u-board', 'u-delivery', 'u-fin', 'u-sales', 'u-tech']);
  const delivery = el.querySelector('.ot-frame[data-unit="u-delivery"]');
  assert.match(delivery.textContent, /Realizacja/);
  assert.match(delivery.textContent, /6 people/);
  assert.match(delivery.textContent, /1 vacancies/);
  assert.ok(delivery.querySelector('.ot-tile.ot-head[data-node="p-delivery"]'));
  assert.ok(delivery.querySelector('.ot-tile[data-node="p-lead"]'));
});

test('a click on a frame selects the unit, a click on its member selects the person', () => {
  const el = mount();
  resizeTo(el, 3200, 1600);
  el.mode = 'units';
  el._render();
  const events = [];
  el.addEventListener('node-select', (e) => events.push(e.detail));
  click(el, el.querySelector('.ot-frame[data-unit="u-fin"] .ot-frame-bg'));
  click(el, el.querySelector('.ot-tile[data-node="p-fin-1"] .ot-name'));
  assert.deepEqual(events, [{ id: 'u-fin', kind: 'unit' }, { id: 'p-fin-1', kind: 'position' }]);
});

test('frames fold too: a unit shows "+N" for its collapsed sub-units', () => {
  const el = mount();
  el.mode = 'units';
  el._render();
  click(el, el.querySelector('.ot-frame[data-unit="u-board"] .ot-count rect'));
  el._render();
  assert.equal(el.querySelectorAll('.ot-frame').length, 1);
  assert.ok(el.querySelector('.ot-more[data-more="unit:u-board"]'));
});

test('zoomed far out the cards collapse to colour blocks and unit tiles', () => {
  window.matchMedia = () => ({ matches: true });
  const el = mount();
  el.zoomBy(0.2);
  el._render();
  assert.equal(el.querySelectorAll('.ot-card').length, 0);
  assert.ok(el.querySelectorAll('.ot-lod').length > 5);
  const tile = el.querySelector('.ot-lod .ot-lod-name');
  assert.ok(tile, 'a unit head is a named tile');
  assert.match(tile.parentNode.textContent, /people/);
  delete window.matchMedia;
});

test('only the slice near the viewport is drawn', () => {
  const big = buildTreeModel(syntheticView(2000, 6), { t, meHex });
  const el = mount(big);
  el._expanded.clear();
  for (const n of big.nodes) el._expanded.set(n.id, true);
  el._relayout({ refit: false });
  el._view = { x: 0, y: 0, k: 1 };
  el._render();
  const drawn = el.querySelectorAll('.ot-card').length;
  assert.ok(el._layout.items.length > 1000);
  assert.ok(drawn > 0 && drawn < 400, `drew ${drawn} of ${el._layout.items.length}`);
  el._view = { x: -el._layout.width * 0.8, y: -el._layout.height * 0.5, k: 1 };
  el._render();
  assert.ok(el.querySelectorAll('.ot-card').length > 0, 'panning moves the slice');
});

test('ctrl + wheel zooms around the pointer, plain wheel pans', () => {
  window.matchMedia = () => ({ matches: true });
  const el = mount();
  const svg = el.querySelector('.tf-orgtree__svg');
  const k0 = el._view.k;
  const wheel = (props) => {
    const e = new window.Event('wheel', { bubbles: true, cancelable: true });
    Object.assign(e, { deltaX: 0, deltaY: 0, deltaMode: 0, clientX: 0, clientY: 0, ...props });
    svg.dispatchEvent(e);
    return e;
  };
  const zoomed = wheel({ ctrlKey: true, deltaY: -100 });
  assert.equal(zoomed.defaultPrevented, true, 'the page does not scroll under the chart');
  assert.ok(el._view.k > k0);
  const y0 = el._view.y;
  wheel({ deltaY: 50 });
  assert.equal(el._view.y, y0 - 50);
  delete window.matchMedia;
});

test('the tools zoom, fit and hand off presentation and export', () => {
  window.matchMedia = () => ({ matches: true });
  const el = mount();
  const buttons = [...el.querySelectorAll('.tf-orgtree__tools [data-tool]')];
  assert.deepEqual(buttons.map((b) => b.dataset.tool), ['zoom-out', 'zoom-reset', 'zoom-in', 'fit', 'present', 'export']);
  assert.equal(buttons[0].getAttribute('aria-label'), 'Zoom out');
  const tool = (name) => buttons.find((b) => b.dataset.tool === name);
  const k0 = el._view.k;
  tool('zoom-in').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.ok(el._view.k > k0);
  tool('zoom-reset').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.equal(el._view.k, 1);
  const seen = [];
  el.addEventListener('present-toggle', () => seen.push('present'));
  el.addEventListener('export-menu', (e) => seen.push(`export:${e.detail.anchor.dataset.tool}`));
  tool('present').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  tool('export').dispatchEvent(new window.MouseEvent('click', { bubbles: true }));
  assert.deepEqual(seen, ['present', 'export:export']);
  delete window.matchMedia;
});

test('focusing a node expands what hides it and keeps it in view', () => {
  const el = mount();
  el.focusNode('p-dev1', { select: true });
  el._render();
  assert.ok(card(el, 'p-dev1'));
  const item = el._layout.byId.get('p-dev1');
  const v = el._viewportWorld();
  assert.ok(item.x >= v.x0 && item.x + item.w <= v.x1 && item.y >= v.y0 && item.y + item.h <= v.y1);
  assert.equal(clipped(el), 0);
});

test('presentation enlarges the cards', () => {
  const el = mount();
  const normal = el._layout.byId.get('p-ceo').w;
  el.presentation = true;
  assert.ok(el._layout.byId.get('p-ceo').w > normal);
  el.presentation = false;
  assert.equal(el._layout.byId.get('p-ceo').w, normal);
});

test('the SVG export is standalone, fully expanded and escapes what the data says', () => {
  const view = sampleView();
  view.assignments[0].display_name = '<script>alert(1)</script> & "Co"';
  const el = mount(buildTreeModel(view, { t, meHex }));
  const out = el.toSvg({ theme: 'light' });
  assert.match(out.svg, /^<svg xmlns="http:\/\/www\.w3\.org\/2000\/svg"/);
  assert.match(out.svg, /<style>/);
  assert.equal(out.svg.includes('<script>'), false);
  assert.match(out.svg, /&lt;script&gt;/);
  for (const name of ['Marek Nowak', 'Ewa Wiśniewska', 'Renata Kot']) assert.ok(out.svg.includes(name), name);
  assert.equal((out.svg.match(/class="ot-card[ "]/g) ?? []).length, 16);
  assert.ok(out.width > 500 && out.height > 300);
  assert.equal(out.svg.includes('var(--'), false, 'a file on disk has no theme tokens');
});

test('the export of one unit holds only its own positions', () => {
  const el = mount();
  const out = el.toSvg({ theme: 'dark', unitId: 'u-delivery' });
  assert.ok(out.svg.includes('Anna Kowalska'));
  assert.equal(out.svg.includes('Adam Woźniak'), false);
  assert.equal((out.svg.match(/class="ot-card[ "]/g) ?? []).length, 7);
  assert.match(out.svg, /#0a0d24/);
});

test('an empty model draws nothing and exports nothing', () => {
  const el = mount({ nodes: [], units: [], functional: [], meIds: [] });
  assert.equal(el.querySelectorAll('.ot-card').length, 0);
  assert.equal(el.toSvg(), null);
});

test('replacing the model resets selection and expansion', () => {
  const el = mount();
  click(el, el.querySelector('.ot-more[data-more="p-lead"] rect'));
  assert.equal(el._expanded.size, 1);
  el.model = model();
  el._render();
  assert.equal(el._expanded.size, 0);
  assert.equal(el.selectedId, null);
});

const clipped = (el) => {
  const v = el._viewportWorld();
  return el._layout.items.filter((i) => i.x < v.x0 - 0.5 || i.y < v.y0 - 0.5 || i.x + i.w > v.x1 + 0.5 || i.y + i.h > v.y1 + 0.5).length;
};

function resizeTo(el, w, h) {
  Object.defineProperty(el._box, 'clientWidth', { value: w, configurable: true });
  Object.defineProperty(el._box, 'clientHeight', { value: h, configurable: true });
  el._onResize();
}

test('the chart opens fitted: nothing is cut off, at any size', () => {
  for (const [w, h] of [[800, 600], [1100, 700], [420, 500]]) {
    const el = mount(buildTreeModel(syntheticView(300, 6), { t, meHex }));
    resizeTo(el, w, h);
    assert.equal(clipped(el), 0, `${w}x${h}`);
  }
});

test('when the whole tree would be unreadable the opening levels are reduced, not cropped', () => {
  const el = mount(buildTreeModel(syntheticView(2000, 8), { t, meHex }));
  resizeTo(el, 1100, 700);
  el._render();
  assert.ok(el._expandDepth < 3, `depth ${el._expandDepth}`);
  assert.ok(el._view.k >= 0.45, `zoom ${el._view.k}`);
  assert.equal(clipped(el), 0);
  assert.ok(el.querySelector('.ot-more'), 'the rest waits behind "+N"');
});

test('a chart the user has not touched refits when its width changes', () => {
  const el = mount();
  resizeTo(el, 1200, 700);
  const wide = el._view.k;
  resizeTo(el, 500, 700);
  assert.ok(el._view.k < wide);
  assert.equal(clipped(el), 0);
});

test('a chart the user has moved keeps its zoom on resize and keeps the selected card in view', () => {
  window.matchMedia = () => ({ matches: true });
  const el = mount();
  resizeTo(el, 1200, 700);
  el.selectedId = 'p-fin-1';
  el.zoomBy(2);
  const k = el._view.k;
  el._view = { k, x: -5000, y: -5000 };
  resizeTo(el, 700, 700);
  assert.equal(el._view.k, k, 'zoom is kept');
  const item = el._layout.byId.get('p-fin-1');
  const v = el._viewportWorld();
  assert.ok(item.x >= v.x0 && item.x + item.w <= v.x1 && item.y >= v.y0 && item.y + item.h <= v.y1);
  delete window.matchMedia;
});

test('fit refits and stays fitted on the next resize', () => {
  window.matchMedia = () => ({ matches: true });
  const el = mount();
  resizeTo(el, 1200, 700);
  el.zoomBy(1.5);
  el.fit();
  assert.equal(el._pristine, true);
  delete window.matchMedia;
});

test('level-of-detail tiles are tree items, named, and reachable by the keyboard', () => {
  window.matchMedia = () => ({ matches: true });
  const el = mount();
  el.zoomBy(0.2);
  el._render();
  const tile = el.querySelector('.ot-lod[data-node="p-cto"]');
  assert.equal(tile.getAttribute('role'), 'treeitem');
  assert.match(tile.getAttribute('aria-label'), /Adam Woźniak/);
  assert.ok(tile.id);
  assert.equal(tile.getAttribute('aria-level'), '2');
  key(el, 'ArrowDown');
  key(el, 'ArrowDown');
  el._render();
  assert.equal(el._activeId, 'p-cto');
  assert.equal(el.querySelector('.tf-orgtree__svg').getAttribute('aria-activedescendant'), tile.id);
  assert.match(el.querySelector('.ot-lod[data-node="p-cto"]').getAttribute('class'), /ot-active/);
  delete window.matchMedia;
});

test('one wheel event never jumps the zoom by more than a quarter', () => {
  window.matchMedia = () => ({ matches: true });
  const el = mount();
  const svg = el.querySelector('.tf-orgtree__svg');
  const k0 = el._view.k;
  const e = new window.Event('wheel', { bubbles: true, cancelable: true });
  Object.assign(e, { deltaX: 0, deltaY: -5000, deltaMode: 0, clientX: 0, clientY: 0, ctrlKey: true });
  svg.dispatchEvent(e);
  assert.ok(el._view.k / k0 <= 1.2501);
  delete window.matchMedia;
});

test('new data keeps the expansion and the selection, and drops what no longer exists', () => {
  const el = mount();
  click(el, el.querySelector('.ot-more[data-more="p-lead"] rect'));
  el.selectedId = 'p-dev1';
  const view = sampleView();
  view.positions = view.positions.filter((p) => p.position_id !== 'p-tester');
  el.updateModel(buildTreeModel(view, { t, meHex }));
  el._render();
  assert.ok(card(el, 'p-dev1'), 'still expanded');
  assert.equal(card(el, 'p-tester'), null);
  assert.equal(el.selectedId, 'p-dev1');
  const gone = sampleView();
  gone.positions = gone.positions.filter((p) => p.position_id !== 'p-dev1');
  el.updateModel(buildTreeModel(gone, { t, meHex }));
  assert.equal(el.selectedId, null);
});

test('the horizontal layout runs levels along x, hangs staff below and joins with sideways elbows', () => {
  const el = mount();
  el.horizontal = true;
  el._render();
  const ceo = el._layout.byId.get('p-ceo');
  const cto = el._layout.byId.get('p-cto');
  const assistant = el._layout.byId.get('p-assistant');
  assert.ok(cto.x >= ceo.x + ceo.w, 'the next level is to the right');
  assert.equal(assistant.x, ceo.x);
  assert.ok(assistant.y >= ceo.y + ceo.h, 'staff sit below their manager');
  assert.match(el.querySelector('.ot-lines path.staff').getAttribute('d'), /^M[\d.]+ [\d.]+V[\d.]+$/);
  assert.match(el.querySelector('.ot-lines path').getAttribute('d'), /^M[\d.]+ [\d.]+H/);
  let overlap = 0;
  const items = el._layout.items;
  for (let i = 0; i < items.length; i += 1) for (let j = i + 1; j < items.length; j += 1) {
    const a = items[i]; const b = items[j];
    if (!(a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y)) overlap += 1;
  }
  assert.equal(overlap, 0);
  assert.equal(clipped(el), 0);
});

const nameFont = (el) => 12.5 * (el.presentation ? 1.3 : 1) * el._view.k;
const roleFont = (el) => 10.5 * (el.presentation ? 1.3 : 1) * el._view.k;

test('the automatic view never shrinks the text below 9 px (name) and 8 px (role)', () => {
  for (const people of [30, 300, 2000]) {
    for (const [w, h] of [[954, 478], [618, 478], [1246, 766]]) {
      const el = mount(buildTreeModel(syntheticView(people, 7), { t, meHex }));
      resizeTo(el, w, h);
      assert.ok(nameFont(el) >= 9 - 1e-9, `${people} people in ${w}x${h}: name ${nameFont(el).toFixed(1)} px`);
      assert.ok(roleFont(el) >= 8 - 1e-9, `${people} people in ${w}x${h}: role ${roleFont(el).toFixed(1)} px`);
      assert.equal(clipped(el), 0, `${people} people in ${w}x${h}`);
    }
  }
});

test('in presentation the floor is met at the larger card size', () => {
  const el = mount(buildTreeModel(syntheticView(300, 7), { t, meHex }));
  el.presentation = true;
  resizeTo(el, 1246, 766);
  assert.ok(nameFont(el) >= 12 - 1e-9);
  assert.equal(clipped(el), 0);
});

test('levels give way before the text does', () => {
  const el = mount(buildTreeModel(syntheticView(2000, 7), { t, meHex }));
  resizeTo(el, 954, 478);
  assert.ok(el._expandDepth < 3);
  assert.ok(el._view.k >= 0.77 - 1e-9);
});

test('when even one level cannot fit at the floor, the chart keeps the floor and centres on the card that matters', () => {
  const el = mount(buildTreeModel(syntheticView(400, 300), { t, meHex }));
  resizeTo(el, 150, 250);
  el.focusNode('p150');
  assert.equal(el._fallback, true);
  assert.ok(Math.abs(el._view.k - 0.77) < 1e-9);
  const item = el._layout.byId.get('p150');
  const v = el._viewportWorld();
  assert.ok(item.x >= v.x0 && item.x + item.w <= v.x1 && item.y >= v.y0 && item.y + item.h <= v.y1);
  assert.equal(el.querySelector('.tf-orgtree__minimap').hidden, false, 'the minimap shows what is out of sight');
});

test('the minimap stays out of the way while the whole chart is in view', () => {
  window.matchMedia = () => ({ matches: true });
  const el = mount();
  resizeTo(el, 1200, 700);
  const minimap = el.querySelector('.tf-orgtree__minimap');
  assert.equal(minimap.hidden, true);
  el.zoomBy(1.5);
  assert.equal(minimap.hidden, false);
  el.fit();
  assert.equal(minimap.hidden, true);
  delete window.matchMedia;
});

test('nothing is laid out under the tools or the hint band in the fitted state', () => {
  const el = mount(buildTreeModel(syntheticView(60, 5), { t, meHex }));
  resizeTo(el, 900, 500);
  const v = el._viewportWorld();
  const top = v.y0 + 64 / el._view.k;
  const bottom = v.y1 - 56 / el._view.k;
  for (const item of el._layout.items) {
    assert.ok(item.y >= top - 0.5 && item.y + item.h <= bottom + 0.5, `${item.id} at ${item.y}`);
  }
});

test('jumping to a card refits the whole tree while the view is automatic, and clearing the path returns to the plain fit', () => {
  const m = buildTreeModel(syntheticView(40, 4), { t, meHex });
  const el = mount(m);
  resizeTo(el, 954, 478);
  el.pathIds = pathTo(m, 'p17');
  el.focusNode('p17', { select: true });
  assert.equal(clipped(el), 0);
  assert.ok(el._layout.byId.has('p17'));
  assert.ok(nameFont(el) >= 9 - 1e-9);
  el.selectedId = null;
  el.pathIds = [];
  assert.equal(el._pathOpen.size, 0, 'the branches opened for the path are released');
  assert.equal(clipped(el), 0);
});

test('a path too big to fit at the floor is shown at the floor, centred, instead of shrinking the text', () => {
  const m = buildTreeModel(syntheticView(300, 6), { t, meHex });
  const el = mount(m);
  resizeTo(el, 954, 478);
  el.pathIds = pathTo(m, 'p200');
  el.focusNode('p200', { select: true });
  assert.ok(nameFont(el) >= 9 - 1e-9);
  const item = el._layout.byId.get('p200');
  const v = el._viewportWorld();
  assert.ok(item.x >= v.x0 && item.x + item.w <= v.x1 && item.y >= v.y0 && item.y + item.h <= v.y1);
});

test('the details panel narrowing the chart and closing again refits both ways', () => {
  const el = mount(buildTreeModel(syntheticView(60, 5), { t, meHex }));
  resizeTo(el, 954, 478);
  const wide = el._view.k;
  resizeTo(el, 618, 478);
  assert.equal(clipped(el), 0);
  resizeTo(el, 954, 478);
  assert.equal(clipped(el), 0);
  assert.equal(el._view.k, wide);
});

test('a path that does not fit with its siblings is shown alone, the siblings behind "+N", at readable size', () => {
  const m = buildTreeModel(syntheticView(300, 6), { t, meHex });
  const el = mount(m);
  resizeTo(el, 618, 478);
  el.pathIds = pathTo(m, 'p200');
  el.focusNode('p200', { select: true });
  assert.equal(el._trim, true);
  assert.equal(clipped(el), 0);
  assert.ok(nameFont(el) >= 9 - 1e-9);
  assert.ok(el._layout.byId.has('p200'));
  el._render();
  const pill = el.querySelector('.ot-more');
  assert.ok(pill, 'the trimmed siblings are counted');
  click(el, pill.querySelector('rect'));
  assert.equal(el._trim, false, 'a click on the "+N" brings the siblings back');
});

test('unit frames give their grid and member list before the text shrinks', () => {
  const el = mount(buildTreeModel(syntheticView(400, 7), { t, meHex }));
  el.mode = 'units';
  resizeTo(el, 954, 478);
  const k = el._view.k;
  assert.ok(k >= 0.85 - 1e-9 || el._expandDepth === 1, `zoom ${k}`);
  assert.equal(clipped(el), 0);
});

const LONG_NAME = 'Krzysztof Maksymilian Wiśniewski-Kowalczykowski-Żółkiewski Młodszy';
const LONG_ROLE = 'Starszy Specjalista do spraw Rozwoju Współpracy Międzynarodowej i Zarządzania Relacjami z Kluczowymi Klientami Instytucjonalnymi';

function longModel() {
  const view = sampleView();
  view.assignments.find((a) => a.position_id === 'p-cto').display_name = LONG_NAME;
  view.positions.find((p) => p.position_id === 'p-cto').name = LONG_ROLE;
  view.units.find((u) => u.unit_id === 'u-tech').name = 'Pion Technologii i Innowacji Cyfrowych Grupy Kapitałowej Solutio';
  return buildTreeModel(view, { t, meHex });
}

test('a very long Polish name and role wrap on the card: nothing is cut with an ellipsis and the card grows', () => {
  const el = mount(longModel());
  resizeTo(el, 3000, 1500);
  const cto = card(el, 'p-cto');
  const html = cto.outerHTML;
  assert.equal(html.includes('…'), false);
  const text = [...cto.querySelectorAll('.ot-name')].map((n) => n.textContent).join(' ').replace(/- /g, '-');
  assert.equal(text, LONG_NAME, 'every word of the name is drawn');
  assert.equal([...cto.querySelectorAll('.ot-role')].map((n) => n.textContent).join(' '), LONG_ROLE);
  const item = el._layout.byId.get('p-cto');
  const plain = el._layout.byId.get('p-fin');
  assert.ok(item.h > plain.h, 'the card is taller');
  assert.ok(item.w <= 300 + 1e-9 && item.w > plain.w, 'and wider, up to the maximum');
  assert.ok([...cto.querySelectorAll('.ot-name')].length > 1);
  assert.equal(clipped(el), 0);
});

test('a short name keeps the normal card, a longer one widens it before it wraps', () => {
  const view = sampleView();
  view.assignments.find((a) => a.position_id === 'p-cto').display_name = 'Aleksandra Wojciechowska-Nowak';
  const el = mount(buildTreeModel(view, { t, meHex }));
  resizeTo(el, 3000, 1500);
  const wide = el._layout.byId.get('p-cto');
  const normal = el._layout.byId.get('p-fin');
  assert.ok(wide.w >= normal.w);
  assert.equal(wide.h, normal.h, 'one line each: no extra height');
});

test('unit frames and member tiles wrap long titles and names too', () => {
  const el = mount(longModel());
  resizeTo(el, 3200, 1600);
  el.mode = 'units';
  el._render();
  const frame = el.querySelector('.ot-frame[data-unit="u-tech"]');
  assert.equal(frame.outerHTML.includes('…'), false);
  assert.equal([...frame.querySelectorAll('.ot-frame-title')].map((n) => n.textContent).join(' '),
    'Pion Technologii i Innowacji Cyfrowych Grupy Kapitałowej Solutio');
  const head = frame.querySelector('.ot-tile.ot-head');
  assert.equal([...head.querySelectorAll('.ot-name')].map((n) => n.textContent).join(' ').replace(/- /g, '-'), LONG_NAME);
});

test('the exported SVG carries the wrapped text as well', () => {
  const out = mount(longModel()).toSvg({ theme: 'light' });
  assert.equal(out.svg.includes('…'), false);
  assert.ok(out.svg.includes('Żółkiewski'));
});

// ---- edit mode -------------------------------------------------------------------------------

const editable = (m = model()) => {
  const el = mount(m);
  resizeTo(el, 3000, 1500);
  el.editing = true;
  el._render();
  return el;
};

const centerOf = (el, id) => {
  const item = el._layout.byId.get(id);
  return { x: item.x + item.w / 2, y: item.y + item.h / 2 };
};

// Client coordinates of a world point: the chart is at the page origin in the test DOM.
const at = (el, point) => ({ clientX: point.x * el._view.k + el._view.x, clientY: point.y * el._view.k + el._view.y });

test('the edit mode gives every card a "⋯" handle, and no export or read-only chart has one', () => {
  const el = mount();
  assert.equal(el.querySelector('[data-menu]'), null);
  el.editing = true;
  el._render();
  assert.equal(el.querySelectorAll('.ot-card [data-menu]').length, el.querySelectorAll('.ot-card').length);
  assert.equal(el.toSvg({ theme: 'light' }).svg.includes('data-menu'), false, 'the export is the plain chart');
});

test('the handle takes room from the text: a card is wider in the edit mode, so nothing runs under it', () => {
  const view = sampleView();
  view.assignments.find((a) => a.position_id === 'p-cto').display_name = 'Aleksandra Wojciechowska-Nowak';
  const el = mount(buildTreeModel(view, { t, meHex }));
  resizeTo(el, 3000, 1500);
  const before = el._layout.byId.get('p-cto');
  el.editing = true;
  const after = el._layout.byId.get('p-cto');
  assert.ok(after.w > before.w, `${before.w} -> ${after.w}`);
});

test('clicking the handle raises node-menu with the card and where the handle is; the click does not select', () => {
  const el = editable();
  const seen = [];
  el.addEventListener('node-menu', (e) => seen.push(e.detail));
  el.addEventListener('node-select', () => seen.push('select'));
  click(el, card(el, 'p-lead').querySelector('[data-menu] circle'));
  assert.equal(seen.length, 1);
  assert.equal(seen[0].id, 'p-lead');
  assert.equal(seen[0].kind, 'position');
  assert.deepEqual(Object.keys(seen[0].rect).sort(), ['height', 'left', 'top', 'width']);
});

test('the Menu key, Shift+F10 and the right button open the menu of the active card', () => {
  const el = editable();
  const seen = [];
  el.addEventListener('node-menu', (e) => seen.push(e.detail));
  el._activeId = 'p-fin';
  key(el, 'ContextMenu');
  key(el, 'F10', { shiftKey: true });
  el.querySelector('.tf-orgtree__svg').dispatchEvent(Object.assign(new window.Event('contextmenu', { bubbles: true, cancelable: true }), { clientX: 30, clientY: 40 }));
  assert.equal(seen.length, 2, 'the two keys');
  assert.deepEqual(seen.map((d) => d.id), ['p-fin', 'p-fin']);

  const target = card(el, 'p-sales').querySelector('.ot-card-bg');
  target.dispatchEvent(Object.assign(new window.Event('contextmenu', { bubbles: true, cancelable: true }), { clientX: 30, clientY: 40 }));
  assert.equal(seen.at(-1).id, 'p-sales');
  assert.deepEqual(seen.at(-1).rect, { left: 30, top: 40, width: 1, height: 1 });
});

test('outside the edit mode the Menu key and the right button do nothing', () => {
  const el = mount();
  const seen = [];
  el.addEventListener('node-menu', (e) => seen.push(e));
  el._activeId = 'p-fin';
  key(el, 'ContextMenu');
  assert.equal(seen.length, 0);
});

function drag(el, fromId, toPoint, { release = true } = {}) {
  const start = at(el, centerOf(el, fromId));
  const target = card(el, fromId).querySelector('.ot-card-bg');
  pointer(target, 'pointerdown', start);
  pointer(target, 'pointermove', { clientX: start.clientX + 30, clientY: start.clientY + 30 });
  pointer(target, 'pointermove', toPoint);
  if (release) pointer(target, 'pointerup', toPoint);
}

test('a card dragged onto another raises node-drop with the rule\'s verdict, and the chart itself changes nothing', () => {
  const el = editable();
  el.dropRule = (source, target) => (target.id === 'p-dev1' ? { ok: false, reason: 'cycle' } : { ok: true });
  const drops = [];
  el.addEventListener('node-drop', (e) => drops.push(e.detail));
  const nodesBefore = el.model.nodes.map((n) => [n.id, n.parentId]);

  drag(el, 'p-sales-1', at(el, centerOf(el, 'p-fin')));
  assert.deepEqual(drops.at(-1), { source: { id: 'p-sales-1', kind: 'position' }, target: { id: 'p-fin', kind: 'position' }, verdict: { ok: true } });
  assert.deepEqual(el.model.nodes.map((n) => [n.id, n.parentId]), nodesBefore, 'the host decides, the chart draws');
  assert.equal(el.querySelector('.tf-orgtree__ghost'), null, 'the ghost is gone after the drop');
  assert.equal(el.querySelector('.ot-drop-ok, .ot-drop-bad'), null);
});

test('while dragging, the card under the pointer is outlined green when the drop is allowed and red when it is not', () => {
  const el = editable();
  el.dropRule = (source, target) => (target.id === 'p-fin' ? { ok: true } : { ok: false, reason: 'cycle' });
  drag(el, 'p-sales-1', at(el, centerOf(el, 'p-fin')), { release: false });
  assert.ok(card(el, 'p-fin').classList.contains('ot-drop-ok'));
  assert.equal(el.querySelector('.tf-orgtree__ghost').dataset.state, 'ok');
  assert.ok(card(el, 'p-sales-1').classList.contains('ot-dragging'), 'the source is dimmed');

  const target = card(el, 'p-sales-1').querySelector('.ot-card-bg');
  pointer(target, 'pointermove', at(el, centerOf(el, 'p-sales-2')));
  assert.equal(card(el, 'p-fin').classList.contains('ot-drop-ok'), false);
  assert.ok(card(el, 'p-sales-2').classList.contains('ot-drop-bad'));
  assert.equal(el.querySelector('.tf-orgtree__ghost').dataset.state, 'bad');

  key(el, 'Escape');
  assert.equal(el.querySelector('.tf-orgtree__ghost'), null, 'Escape abandons the drag');
  assert.equal(el.querySelector('.ot-drop-bad'), null);
});

test('a card dropped on empty canvas, or on itself, raises nothing; a plain click still selects', () => {
  const el = editable();
  const drops = [];
  const selects = [];
  el.addEventListener('node-drop', (e) => drops.push(e.detail));
  el.addEventListener('node-select', (e) => selects.push(e.detail));
  drag(el, 'p-sales-1', { clientX: 5, clientY: 5 });
  drag(el, 'p-sales-1', at(el, centerOf(el, 'p-sales-1')));
  assert.equal(drops.length, 0);
  click(el, card(el, 'p-sales-1').querySelector('.ot-card-bg'));
  assert.deepEqual(selects.at(-1), { id: 'p-sales-1', kind: 'position' });
});

test('outside the edit mode a drag pans the chart and never raises node-drop', () => {
  const el = mount();
  resizeTo(el, 3000, 1500);
  const drops = [];
  el.addEventListener('node-drop', (e) => drops.push(e));
  const view = { ...el._view };
  drag(el, 'p-sales-1', at(el, centerOf(el, 'p-fin')));
  assert.equal(drops.length, 0);
  assert.notDeepEqual({ ...el._view }, view, 'the chart panned');
});

test('nodeAt answers with the card under a screen point, limited to the kind asked for', () => {
  const el = editable();
  const point = at(el, centerOf(el, 'p-fin'));
  assert.deepEqual(el.nodeAt(point.clientX, point.clientY), { id: 'p-fin', kind: 'position' });
  assert.equal(el.nodeAt(point.clientX, point.clientY, 'unit'), null);
  assert.equal(el.nodeAt(-500, -500), null);
});

test('in the units view a frame can be dragged onto another frame, and its member tiles cannot', () => {
  const el = editable();
  el.mode = 'units';
  el._render();
  const seen = [];
  el.addEventListener('node-drop', (e) => seen.push(e.detail));
  const frame = el.querySelector('.ot-frame[data-unit="u-sales"]');
  const item = el._layout.byId.get('unit:u-sales');
  const from = at(el, { x: item.x + item.w / 2, y: item.y + 3 });
  const finItem = el._layout.byId.get('unit:u-fin');
  const to = at(el, { x: finItem.x + finItem.w / 2, y: finItem.y + finItem.h / 2 });
  const grip = frame.querySelector('.ot-frame-bg');
  pointer(grip, 'pointerdown', from);
  pointer(grip, 'pointermove', { clientX: from.clientX + 30, clientY: from.clientY + 30 });
  pointer(grip, 'pointermove', to);
  pointer(grip, 'pointerup', to);
  assert.deepEqual(seen.at(-1)?.source, { id: 'u-sales', kind: 'unit' });
  assert.deepEqual(seen.at(-1)?.target, { id: 'u-fin', kind: 'unit' });

  seen.length = 0;
  const tile = el.querySelector('.ot-frame[data-unit="u-sales"] .ot-tile');
  if (tile) {
    pointer(tile, 'pointerdown', from);
    pointer(tile, 'pointermove', { clientX: from.clientX + 30, clientY: from.clientY + 30 });
    pointer(tile, 'pointermove', to);
    pointer(tile, 'pointerup', to);
    assert.equal(seen.length, 0, 'a member tile is not a unit');
  }
});
