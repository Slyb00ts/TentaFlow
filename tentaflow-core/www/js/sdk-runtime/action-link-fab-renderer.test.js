// =============================================================================
// Plik: sdk-runtime/action-link-fab-renderer.test.js
// Opis: Testy LinkButton (0x0404) + Link (0x0405) + Fab (0x040C) — chunk 3.3b-3.
// =============================================================================

import './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import {
  LINK_BUTTON_TAG, LINK_TAG, FAB_TAG,
} from './action-link-fab-renderer.js';

const results = [];
function test(name, fn) {
  try { fn(); results.push({ name, ok: true }); }
  catch (err) { results.push({ name, ok: false, err }); }
}
function assertEq(a, e, m) {
  const aj = JSON.stringify(a, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  const ej = JSON.stringify(e, (_k, v) => typeof v === 'bigint' ? `${v}n` : v);
  if (aj !== ej) throw new Error(`${m || 'assertEq'}: expected ${ej}, got ${aj}`);
}
function assert(cond, m) { if (!cond) throw new Error(m || 'assert failed'); }
function assertThrows(fn, m) {
  let t = false; try { fn(); } catch { t = true; }
  if (!t) throw new Error(m || 'expected throw');
}

const PATH = (...segs) => segs.map((s) =>
  typeof s === 'number' ? { kind: 'index', value: s } : { kind: 'key', value: s });

function makeStore() {
  return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
}
function makeEngine(store, dispatcher) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: dispatcher || { emit() {} },
    locale: 'en-US',
  });
}
function comp(tag, fields, extra = {}) {
  return {
    tag, id: extra.id ?? 'c1', fields,
    handlers: extra.handlers ?? null,
    bind: extra.bind ?? null,
    a11y: extra.a11y ?? null,
    visibility: extra.visibility ?? null,
    test_id: extra.test_id ?? null,
  };
}
function setup() {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  document.body.innerHTML = '';
}

// ============================================================================
// LinkButton (0x0404)
// ============================================================================

test('LinkButton renders <tf-button variant=ghost> with tone attr + underline class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(LINK_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'Otwórz' }],
      [3, 'primary'],
      [4, 'hover'],
    ])
  );
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-BUTTON');
  assertEq(el.getAttribute('variant'), 'ghost');
  assertEq(el.getAttribute('tone'), 'primary');
  assert(!el.classList.contains('tf-link-button--tone-primary'));
  assert(el.classList.contains('tf-link-button--underline-hover'));
  assertEq(el.getAttribute('label'), 'Otwórz');
});

test('LinkButton reactive label from BindRef', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('lbl'), value: 'V1' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(LINK_BUTTON_TAG, [
      [0, { kind: 'bound', path: PATH('lbl') }],
      [3, 'neutral'], [4, 'never'],
    ])
  );
  assertEq(el.getAttribute('label'), 'V1');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('lbl'), op: { kind: 'set', value: 'V2' } }],
  });
  assertEq(el.getAttribute('label'), 'V2');
});

test('LinkButton named icon_leading maps to icon attr, icon_trailing renders svg child', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(LINK_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'Pobierz' }],
      [1, { kind: 'named', name: 'download' }],
      [2, { kind: 'named', name: 'external_link' }],
      [3, 'primary'], [4, 'always'],
    ])
  );
  document.body.appendChild(el);
  assertEq(el.getAttribute('icon'), 'download');
  const trailing = el.querySelectorAll('svg.tf-icon');
  assertEq(trailing.length, 1);
  assert(trailing[0].classList.contains('tf-icon--name-external_link'));
  assertEq(
    trailing[0].querySelector('use').getAttribute('href'),
    '/img/icons.svg#icon-external-link'
  );
});

test('LinkButton click via engine handler', () => {
  setup();
  const dispatched = [];
  const engine = makeEngine(undefined, { emit(e) { dispatched.push(e); } });
  const el = engine.render(
    comp(LINK_BUTTON_TAG, [
      [0, { kind: 'literal', value: 'X' }], [3, 'neutral'], [4, 'never'],
    ], {
      handlers: [['click', { kind: 'backend', operation_id: 'op' }]],
    })
  );
  el.click();
  assertEq(dispatched.length, 1);
  assertEq(dispatched[0].event_kind, 'click');
});

test('LinkButton rejects invalid underline', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(LINK_BUTTON_TAG, [
        [0, { kind: 'literal', value: 'X' }],
        [3, 'neutral'], [4, 'sometimes'],
      ])
    )
  );
});

test('LinkButton rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(LINK_BUTTON_TAG, [
        [0, { kind: 'literal', value: 'X' }], [3, 'neutral'], [4, 'never'],
        [99, 'evil'],
      ])
    )
  );
});

// ============================================================================
// Link (0x0405)
// ============================================================================

test('Link renders <tf-button role=link> with tone attr + underline class', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(LINK_TAG, [
      [0, { kind: 'literal', value: 'Zobacz' }],
      [1, 'hover'], [2, 'info'],
    ])
  );
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-BUTTON');
  assertEq(el.getAttribute('role'), 'link');
  assertEq(el.getAttribute('variant'), 'ghost');
  assertEq(el.getAttribute('tone'), 'info');
  assert(!el.classList.contains('tf-link--tone-info'));
  assert(el.classList.contains('tf-link--underline-hover'));
  assertEq(el.getAttribute('label'), 'Zobacz');
});

test('Link has no raw href (navigation only via engine handlers)', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(LINK_TAG, [
      [0, { kind: 'literal', value: 'X' }], [1, 'never'], [2, 'primary'],
    ])
  );
  assertEq(el.getAttribute('href'), null);
  assertEq(el.tagName, 'TF-BUTTON');
});

test('Link click goes through engine handler (separate from preventDefault)', () => {
  setup();
  const dispatched = [];
  const engine = makeEngine(undefined, { emit(e) { dispatched.push(e); } });
  const el = engine.render(
    comp(LINK_TAG, [
      [0, { kind: 'literal', value: 'X' }], [1, 'never'], [2, 'primary'],
    ], {
      handlers: [['click', { kind: 'backend', operation_id: 'op' }]],
    })
  );
  el.click();
  assertEq(dispatched[0].event_kind, 'click');
});

test('Link named leading_icon maps to icon attr, trailing_icon renders svg child', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(LINK_TAG, [
      [0, { kind: 'literal', value: 'Open' }],
      [1, 'always'], [2, 'primary'],
      [3, { kind: 'named', name: 'external_link' }],
      [4, { kind: 'named', name: 'arrow_right' }],
    ])
  );
  document.body.appendChild(el);
  assertEq(el.getAttribute('icon'), 'external_link');
  const trailing = el.querySelectorAll('svg.tf-icon');
  assertEq(trailing.length, 1);
  assert(trailing[0].classList.contains('tf-icon--name-arrow_right'));
});

test('Link rejects missing required tone', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(LINK_TAG, [
        [0, { kind: 'literal', value: 'X' }], [1, 'always'],
      ])
    )
  );
});

// ============================================================================
// Fab (0x040C)
// ============================================================================

const FAB_VALID = [
  [0, { kind: 'named', name: 'plus' }],
  [1, 'primary'],
  [2, 'md'],
  [3, 'bottom_right'],
];

test('Fab renders <tf-button> with icon attr + position class; icon-only requires a11y.label', () => {
  setup();
  const engine = makeEngine();
  // Without label field and without a11y.label → throw (a11y enforcement).
  assertThrows(() => engine.render(comp(FAB_TAG, FAB_VALID)));
  // With a11y.label OK.
  const el = engine.render(
    comp(FAB_TAG, FAB_VALID, {
      a11y: { label: { kind: 'literal', value: 'Add new' } },
    })
  );
  document.body.appendChild(el);
  assertEq(el.tagName, 'TF-BUTTON');
  assertEq(el.getAttribute('variant'), 'primary');
  assertEq(el.getAttribute('tone'), 'primary');
  assert(el.classList.contains('tf-fab'));
  assert(!el.classList.contains('tf-fab--tone-primary'));
  assert(el.classList.contains('tf-fab--size-md'));
  assert(el.classList.contains('tf-fab--position-bottom_right'));
  // Named icon maps to tf-button icon attribute (no child icon element).
  assertEq(el.getAttribute('icon'), 'plus');
  assertEq(el.getAttribute('aria-label'), 'Add new');
});

test('Fab with label renders extended variant', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(FAB_TAG, [
      ...FAB_VALID,
      [4, { kind: 'literal', value: 'New item' }],
    ])
  );
  assert(el.classList.contains('tf-fab--extended'));
  assertEq(el.getAttribute('label'), 'New item');
});

test('Fab label reactive from BindRef', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('fl'), value: 'Create' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(FAB_TAG, [
      ...FAB_VALID,
      [4, { kind: 'bound', path: PATH('fl') }],
    ])
  );
  assertEq(el.getAttribute('label'), 'Create');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('fl'), op: { kind: 'set', value: 'Add' } }],
  });
  assertEq(el.getAttribute('label'), 'Add');
});

test('Fab rejects missing required icon', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(FAB_TAG, [[1, 'primary'], [2, 'md'], [3, 'inline']]))
  );
});

test('Fab rejects invalid position', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(FAB_TAG, [
        [0, { kind: 'named', name: 'plus' }],
        [1, 'primary'], [2, 'md'], [3, 'top_left'],
      ], { a11y: { label: { kind: 'literal', value: 'X' } } })
    )
  );
});

test('Fab inline position in extended variant', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(FAB_TAG, [
      [0, { kind: 'named', name: 'plus' }],
      [1, 'success'], [2, 'lg'], [3, 'inline'],
      [4, { kind: 'literal', value: 'Add' }],
    ])
  );
  assert(el.classList.contains('tf-fab--position-inline'));
  assert(el.classList.contains('tf-fab--extended'));
});

test('Fab click via engine handler', () => {
  setup();
  const dispatched = [];
  const engine = makeEngine(undefined, { emit(e) { dispatched.push(e); } });
  const el = engine.render(
    comp(FAB_TAG, FAB_VALID, {
      a11y: { label: { kind: 'literal', value: 'X' } },
      handlers: [['click', { kind: 'backend', operation_id: 'op' }]],
    })
  );
  el.click();
  assertEq(dispatched[0].event_kind, 'click');
});

test('Fab rejects a11y.label resolving to empty string', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(FAB_TAG, FAB_VALID, {
        a11y: { label: { kind: 'literal', value: '' } },
      })
    )
  );
});

test('Fab rejects a11y.label resolving to whitespace-only string', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(FAB_TAG, FAB_VALID, {
        a11y: { label: { kind: 'literal', value: '   ' } },
      })
    )
  );
});

test('Fab rejects a11y.label bound to missing store path', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  assertThrows(() =>
    engine.render(
      comp(FAB_TAG, FAB_VALID, {
        a11y: { label: { kind: 'bound', path: PATH('missing') } },
      })
    )
  );
});

test('Fab rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(FAB_TAG, [...FAB_VALID, [99, 'rogue']], {
        a11y: { label: { kind: 'literal', value: 'X' } },
      })
    )
  );
});

// ---- report ----
function reportResults() {
  let pass = 0, fail = 0;
  const lines = [];
  for (const r of results) {
    if (r.ok) { pass++; lines.push(`✓ ${r.name}`); }
    else { fail++; lines.push(`✗ ${r.name}\n    ${r.err && r.err.stack ? r.err.stack : r.err}`); }
  }
  lines.push('');
  lines.push(`${pass}/${pass + fail} tests passed${fail ? ` — ${fail} FAILED` : ''}`);
  return { pass, fail, text: lines.join('\n') };
}
if (typeof process !== 'undefined') {
  const r = reportResults();
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}
export { reportResults };
