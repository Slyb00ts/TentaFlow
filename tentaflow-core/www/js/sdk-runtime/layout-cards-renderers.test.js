// =============================================================================
// Plik: sdk-runtime/layout-cards-renderers.test.js
// Opis: Testy 4 cards Layout (Krok 3.3a-3): Card/SectionCard/Collapsible/Accordion.
// =============================================================================

import { window as harnessWindow } from './_dom-test-harness.js';
import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import { bootstrapSdkRuntime } from './bootstrap.js';
import { SPACER_TAG, DIVIDER_TAG } from './layout-atomic-renderers.js';
import {
  CARD_TAG, SECTION_CARD_TAG, COLLAPSIBLE_TAG, ACCORDION_TAG,
} from './layout-cards-renderers.js';
// Button (tag 0x0401) realny renderer rejestruje się w `bootstrapSdkRuntime`
// przez chunk 3.3b-1. Helper budowy fixture'owego Button-component'u
// (minimal valid shape) używamy w SectionCard.header_actions tests.
const BUTTON_TAG = 0x0401;

// The harness exports bound globals; a bound class has no .prototype, which
// breaks `class X extends HTMLElement`. Restore the raw constructor before
// loading web components (dynamic import runs after the harness).
globalThis.HTMLElement = harnessWindow.HTMLElement;
await import('../components/tf-section-card.js');
function makeButtonFixture(extra = {}) {
  return comp(BUTTON_TAG, [
    [0, 'primary'],
    [1, 'neutral'],
    [2, { kind: 'literal', value: 'OK' }],
    [5, 'md'],
    [6, false],
    [9, 'default'],
  ], extra);
}

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

const PATH = (...segs) =>
  segs.map((s) => typeof s === 'number'
    ? { kind: 'index', value: s }
    : { kind: 'key', value: s });

function makeStore() {
  return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
}
function makeEngine(store) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: { emit() {} },
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
// Attach to the document so custom elements upgrade (connectedCallback)
function mount(el) {
  document.body.appendChild(el);
  return el;
}

const BORDER_NONE = { kind: 'none' };

// ============================================================================
// Card
// ============================================================================

test('Card renders with all 11 required + defaults', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(CARD_TAG, [
      [0, 'filled'],
      [5, BORDER_NONE],
      [6, 'subtle'],
      [8, []],
      [9, false],
      [10, false],
    ])
  );
  assertEq(el.tagName, 'TF-SECTION-CARD');
  assert(el.hasAttribute('plain'));
  assert(el.classList.contains('tf-card'));
  assert(el.classList.contains('tf-card--variant-filled'));
  assert(el.classList.contains('tf-card--padding-lg')); // default
  assert(el.classList.contains('tf-card--gap-md'));     // default
  assert(el.classList.contains('tf-card--radius-lg'));  // default
  assert(el.classList.contains('tf-card--shadow-none')); // filled→none
  assert(el.classList.contains('tf-card--bg-subtle'));
  assert(el.classList.contains('tf-card--border-none'));
});

test('Card variant=elevated defaults shadow=subtle', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(CARD_TAG, [
      [0, 'elevated'],
      [5, BORDER_NONE],
      [6, 'subtle'],
      [9, false],
      [10, false],
    ])
  );
  assert(el.classList.contains('tf-card--shadow-subtle'));
});

test('Card accepts shadow=accent_glow', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(CARD_TAG, [
      [0, 'filled'],
      [4, 'accent_glow'],
      [5, BORDER_NONE],
      [6, 'subtle'],
      [9, false],
      [10, false],
    ])
  );
  assert(el.classList.contains('tf-card--shadow-accent_glow'));
});

test('Card clickable=true sets role=button and tabindex', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(CARD_TAG, [
      [0, 'filled'], [5, BORDER_NONE], [6, 'subtle'],
      [9, true], [10, true],
    ])
  );
  assertEq(el.getAttribute('role'), 'button');
  assertEq(el.getAttribute('tabindex'), '0');
  assert(el.classList.contains('tf-card--clickable'));
  assert(el.classList.contains('tf-card--interactive'));
});

test('Card border accent variant requires tone', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(CARD_TAG, [
      [0, 'outlined'],
      [5, { kind: 'accent', tone: 'primary' }],
      [6, 'subtle'],
      [9, false], [10, false],
    ])
  );
  assert(el.classList.contains('tf-card--border-accent'));
  assert(el.classList.contains('tf-card--border-tone-primary'));
});

test('Card border accent rejects unknown tone', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(CARD_TAG, [
        [0, 'outlined'],
        [5, { kind: 'accent', tone: 'galactic' }],
        [6, 'subtle'],
        [9, false], [10, false],
      ])
    )
  );
});

test('Card rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(CARD_TAG, [
        [0, 'filled'], [5, BORDER_NONE], [6, 'subtle'],
        [9, false], [10, false],
        [99, 'evil'],
      ])
    )
  );
});

test('Card renders children in order', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(CARD_TAG, [
      [0, 'filled'], [5, BORDER_NONE], [6, 'subtle'],
      [
        8,
        [
          comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'a' }),
          comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'b' }),
        ],
      ],
      [9, false], [10, false],
    ])
  );
  // plain variant keeps light DOM untouched also after connection
  mount(el);
  const ids = [...el.children].map((c) => c.getAttribute('data-testid'));
  assertEq(ids, ['a', 'b']);
});

// ============================================================================
// SectionCard
// ============================================================================

test('SectionCard renders header with title + subtitle + Button actions + divider', () => {
  setup();
  const engine = makeEngine();
  // header_actions MUSI być Button tag=0x0401 (spec §3 0x0107).
  const btn = makeButtonFixture({ test_id: 'btn1' });
  const el = engine.render(
    comp(SECTION_CARD_TAG, [
      [0, { kind: 'literal', value: 'Tytuł' }],
      [1, { kind: 'literal', value: 'Podtytuł' }],
      [2, [btn]],
      [3, true],
      [4, []],
      [8, 'filled'],
      [11, BORDER_NONE],
      [12, 'subtle'],
    ])
  );
  mount(el);
  assertEq(el.tagName, 'TF-SECTION-CARD');
  assert(el.querySelector('.tf-section-card') != null);
  assertEq(el.querySelector('.tf-section-card-title').textContent, 'Tytuł');
  assertEq(el.querySelector('.tf-section-card__subtitle').textContent, 'Podtytuł');
  assert(el.querySelector('.tf-section-card__actions [data-testid="btn1"]') != null);
  assert(el.hasAttribute('header-divider'));
  const divider = el.querySelector('.tf-section-card__header-divider');
  assert(divider != null && divider.style.display !== 'none');
});

test('SectionCard.header_actions rejects non-Button tag', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SECTION_CARD_TAG, [
        [0, { kind: 'literal', value: 'X' }],
        [2, [comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])]],  // SPACER ≠ Button
        [3, false], [4, []],
        [8, 'filled'], [11, BORDER_NONE], [12, 'subtle'],
      ])
    )
  );
});

test('SectionCard reactive title bound to store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('t'), value: 'A' }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SECTION_CARD_TAG, [
      [0, { kind: 'bound', path: PATH('t') }],
      [3, false],
      [4, []],
      [8, 'filled'],
      [11, BORDER_NONE],
      [12, 'subtle'],
    ])
  );
  mount(el);
  const titleEl = el.querySelector('.tf-section-card-title');
  assertEq(titleEl.textContent, 'A');
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('t'), op: { kind: 'set', value: 'B' } }],
  });
  assertEq(titleEl.textContent, 'B');
});

test('SectionCard footer optional, omitted by default', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SECTION_CARD_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [3, false], [4, []],
      [8, 'filled'], [11, BORDER_NONE], [12, 'subtle'],
    ])
  );
  mount(el);
  assertEq(el.querySelector('.tf-section-card__footer'), null);
});

test('SectionCard with footer renders separately', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SECTION_CARD_TAG, [
      [0, { kind: 'literal', value: 'X' }],
      [3, false], [4, []],
      [5, [comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'fb' })]],
      [8, 'filled'], [11, BORDER_NONE], [12, 'subtle'],
    ])
  );
  mount(el);
  const footer = el.querySelector('.tf-section-card__footer');
  assert(footer != null);
  assert(footer.querySelector('[data-testid="fb"]') != null);
});

// ============================================================================
// Collapsible
// ============================================================================

test('Collapsible respects expanded BindRef and toggles aria-expanded', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('exp'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(COLLAPSIBLE_TAG, [
      [0, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'hdr' })],
      [1, [comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'body' })]],
      [2, { kind: 'bound', path: PATH('exp') }],
      [3, true],
    ])
  );
  const header = el.querySelector('.tf-collapsible__header');
  const body = el.querySelector('.tf-collapsible__body');
  assertEq(header.getAttribute('aria-expanded'), 'false');
  assert(body.hasAttribute('hidden'));
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('exp'), op: { kind: 'set', value: true } }],
  });
  assertEq(header.getAttribute('aria-expanded'), 'true');
  assertEq(body.hasAttribute('hidden'), false);
});

test('Collapsible header click dispatches custom open/close event', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('exp'), value: false }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const wrapper = engine.render(
    comp(COLLAPSIBLE_TAG, [
      [0, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])],
      [1, []],
      [2, { kind: 'bound', path: PATH('exp') }],
      [3, false],
    ])
  );
  const events = [];
  wrapper.addEventListener('open', () => events.push('open'));
  wrapper.addEventListener('close', () => events.push('close'));
  const header = wrapper.querySelector('.tf-collapsible__header');
  header.click();
  store.applyPatch({
    base_revision: 0, new_revision: 1,
    ops: [{ path: PATH('exp'), op: { kind: 'set', value: true } }],
  });
  header.click();
  assertEq(events, ['open', 'close']);
});

test('Collapsible rejects missing animated', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(COLLAPSIBLE_TAG, [
        [0, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])],
        [1, []],
        [2, { kind: 'literal', value: true }],
      ])
    )
  );
});

// ============================================================================
// Accordion
// ============================================================================

test('Accordion renders items with item id + header + body', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('exp'), value: ['it1'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(ACCORDION_TAG, [
      [
        0,
        [
          [
            [0, 'it1'],
            [1, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'h1' })],
            [2, [comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'b1' })]],
            [3, true],
          ],
          [
            [0, 'it2'],
            [1, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'h2' })],
            [2, [comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'b2' })]],
            [3, false],
          ],
        ],
      ],
      [1, 'multiple'],
      [2, { kind: 'bound', path: PATH('exp') }],
    ])
  );
  const items = el.querySelectorAll('.tf-accordion__item');
  assertEq(items.length, 2);
  assertEq(items[0].getAttribute('data-accordion-id'), 'it1');
  const h1 = items[0].querySelector('.tf-accordion__header');
  const h2 = items[1].querySelector('.tf-accordion__header');
  assertEq(h1.getAttribute('aria-expanded'), 'true');
  assertEq(h2.getAttribute('aria-expanded'), 'false');
});

test('Accordion item click dispatches open/close with item_id', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('exp'), value: [] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(ACCORDION_TAG, [
      [
        0,
        [
          [
            [0, 'a'],
            [1, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])],
            [2, []],
            [3, false],
          ],
        ],
      ],
      [1, 'single'],
      [2, { kind: 'bound', path: PATH('exp') }],
    ])
  );
  let received = null;
  el.addEventListener('open', (e) => { received = e.detail; });
  const h = el.querySelector('.tf-accordion__header');
  h.click();
  assertEq(received, { item_id: 'a' });
});

test('Accordion rejects item with unknown key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(ACCORDION_TAG, [
        [
          0,
          [
            [
              [0, 'x'],
              [1, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])],
              [2, []],
              [3, false],
              [9, true],
            ],
          ],
        ],
        [1, 'single'],
        [2, { kind: 'literal', value: [] }],
      ])
    )
  );
});

test('Accordion uses default_expanded when bind resolves to non-array', () => {
  setup();
  const store = makeStore();
  // expanded_ids bound to path that doesn't exist → fallback do default_expanded.
  store.applySnapshot({ entries: [], state_revision: 0, truncated: false });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(ACCORDION_TAG, [
      [
        0,
        [
          [
            [0, 'a'],
            [1, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])],
            [2, []],
            [3, true],
          ],
          [
            [0, 'b'],
            [1, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])],
            [2, []],
            [3, false],
          ],
        ],
      ],
      [1, 'multiple'],
      [2, { kind: 'bound', path: PATH('missing') }],
    ])
  );
  const items = el.querySelectorAll('.tf-accordion__item');
  assertEq(items[0].querySelector('.tf-accordion__header').getAttribute('aria-expanded'), 'true');
  assertEq(items[1].querySelector('.tf-accordion__header').getAttribute('aria-expanded'), 'false');
});

test('Accordion mode=single clamps expanded_ids to first', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: PATH('exp'), value: ['a', 'b'] }],
    state_revision: 0, truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(ACCORDION_TAG, [
      [
        0,
        [
          [[0, 'a'], [1, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])], [2, []], [3, false]],
          [[0, 'b'], [1, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])], [2, []], [3, false]],
        ],
      ],
      [1, 'single'],
      [2, { kind: 'bound', path: PATH('exp') }],
    ])
  );
  const items = el.querySelectorAll('.tf-accordion__item');
  assertEq(items[0].querySelector('.tf-accordion__header').getAttribute('aria-expanded'), 'true');
  assertEq(items[1].querySelector('.tf-accordion__header').getAttribute('aria-expanded'), 'false');
});

test('Card clickable: Enter key synthesizes click', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(CARD_TAG, [
      [0, 'filled'], [5, BORDER_NONE], [6, 'subtle'],
      [9, false], [10, true],
    ])
  );
  let clicks = 0;
  el.addEventListener('click', () => clicks++);
  el.dispatchEvent(new window.KeyboardEvent('keydown', { key: 'Enter' }));
  assertEq(clicks, 1);
  el.dispatchEvent(new window.KeyboardEvent('keydown', { key: ' ' }));
  assertEq(clicks, 2);
});

test('Accordion rejects empty id', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(ACCORDION_TAG, [
        [
          0,
          [[
            [0, ''],
            [1, comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])],
            [2, []],
            [3, false],
          ]],
        ],
        [1, 'single'],
        [2, { kind: 'literal', value: [] }],
      ])
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
