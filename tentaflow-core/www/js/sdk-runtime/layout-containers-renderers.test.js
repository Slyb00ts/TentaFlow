// =============================================================================
// Plik: sdk-runtime/layout-containers-renderers.test.js
// Opis: Testy containerów Layout: Flex, Grid, Stack, Cluster, Split.
// =============================================================================

import './_dom-test-harness.js';

import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  _clearComponentRendererRegistry,
  _clearResponsiveStyles,
} from './component-renderer.js';
import { registerLayoutAtomicRenderers, SPACER_TAG, DIVIDER_TAG } from './layout-atomic-renderers.js';
import {
  registerLayoutContainersRenderers,
  FLEX_TAG,
  GRID_TAG,
  STACK_TAG,
  CLUSTER_TAG,
  SPLIT_TAG,
  BOX_TAG,
} from './layout-containers-renderers.js';
import { bootstrapSdkRuntime } from './bootstrap.js';

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

// Helpery
function makeStore() {
  return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
}
function makeDispatcher() {
  return { emit() {} };
}
function makeEngine() {
  return new ComponentRenderer({
    store: makeStore(),
    eventDispatcher: makeDispatcher(),
    locale: 'en-US',
  });
}
function comp(tag, fields, extra = {}) {
  return {
    tag,
    id: 'cmp1',
    fields,
    handlers: extra.handlers ?? null,
    bind: extra.bind ?? null,
    a11y: extra.a11y ?? null,
    visibility: extra.visibility ?? null,
    test_id: extra.test_id ?? null,
  };
}
function setup() {
  _clearComponentRendererRegistry();
  _clearResponsiveStyles();
  registerLayoutAtomicRenderers();
  registerLayoutContainersRenderers();
  document.body.innerHTML = '';
}

function injectedResponsiveCss() {
  const el = document.head.querySelector('style[data-sdk-responsive]');
  return el ? el.textContent : '';
}

// ============================================================================
// Flex
// ============================================================================

test('Flex renders with all required + optional classes', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(FLEX_TAG, [
      [0, 'row'],
      [1, 'lg'],
      [2, 'space_between'],
      [3, 'center'],
      [4, 'wrap'],
      [5, []],
      [6, 'md'],
      [7, 'subtle'],
      [8, 'lg'],
    ])
  );
  assert(el.classList.contains('tf-flex'));
  assert(el.classList.contains('tf-flex--direction-row'));
  assert(el.classList.contains('tf-flex--gap-lg'));
  assert(el.classList.contains('tf-flex--justify-space_between'));
  assert(el.classList.contains('tf-flex--align-center'));
  assert(el.classList.contains('tf-flex--wrap-wrap'));
  assert(el.classList.contains('tf-flex--padding-md'));
  assert(el.classList.contains('tf-flex--bg-subtle'));
  assert(el.classList.contains('tf-flex--radius-lg'));
});

test('Flex uses default gap "md" when absent', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(FLEX_TAG, [
      [0, 'column'],
      [2, 'start'],
      [3, 'stretch'],
      [4, 'no_wrap'],
    ])
  );
  assert(el.classList.contains('tf-flex--gap-md'));
});

test('Flex renders children in order', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(FLEX_TAG, [
      [0, 'row'],
      [1, 'sm'],
      [2, 'start'],
      [3, 'center'],
      [4, 'no_wrap'],
      [
        5,
        [
          comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'a' }),
          comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'b' }),
          comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'c' }),
        ],
      ],
    ])
  );
  const tids = [...el.children].map((c) => c.getAttribute('data-testid'));
  assertEq(tids, ['a', 'b', 'c']);
});

test('Flex rejects invalid direction', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(FLEX_TAG, [
        [0, 'diagonal'],
        [1, 'md'],
        [2, 'start'],
        [3, 'center'],
        [4, 'no_wrap'],
      ])
    )
  );
});

// ============================================================================
// Grid
// ============================================================================

test('Grid Equal sets repeat(N, minmax(0, 1fr))', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(GRID_TAG, [
      [0, { kind: 'equal', count: 3 }],
      [1, 'md'],
      [4, []],
    ])
  );
  assertEq(el.style.gridTemplateColumns, 'repeat(3, minmax(0, 1fr))');
});

test('Grid Explicit maps each GridCol variant', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(GRID_TAG, [
      [
        0,
        {
          kind: 'explicit',
          cols: [
            { kind: 'auto' },
            { kind: 'fr', value: 2 },
            { kind: 'px', value: 240 },
            { kind: 'min_content' },
            { kind: 'max_content' },
            { kind: 'fill' },
          ],
        },
      ],
      [1, 'sm'],
      [4, []],
    ])
  );
  assertEq(
    el.style.gridTemplateColumns,
    'auto 2fr 240px min-content max-content minmax(0, 1fr)'
  );
});

test('Grid Equal accepts BigInt count from wire decoder', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(GRID_TAG, [
      [0, { kind: 'equal', count: 4n }],
      [1, 'md'],
      [4, []],
    ])
  );
  assertEq(el.style.gridTemplateColumns, 'repeat(4, minmax(0, 1fr))');
});

test('Grid Equal with out-of-range BigInt count rejected', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(GRID_TAG, [
        [0, { kind: 'equal', count: 300n }],
        [1, 'md'],
        [4, []],
      ])
    )
  );
});

test('Grid Equal with count=0 rejected', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(GRID_TAG, [
        [0, { kind: 'equal', count: 0 }],
        [1, 'md'],
        [4, []],
      ])
    )
  );
});

test('Grid Explicit with empty cols rejected', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(GRID_TAG, [
        [0, { kind: 'explicit', cols: [] }],
        [1, 'md'],
        [4, []],
      ])
    )
  );
});

test('Grid renders GridChild with col_span, row_span, col_start, align_self', () => {
  setup();
  const engine = makeEngine();
  const child = comp(SPACER_TAG, [[0, 'xs'], [1, 'x']], { test_id: 'gc' });
  const el = engine.render(
    comp(GRID_TAG, [
      [0, { kind: 'equal', count: 3 }],
      [1, 'md'],
      [
        4,
        [
          [
            [0, child],
            [1, 2n],
            [2, 1],
            [3, 2],
            [5, 'center'],
            [6, 'space_around'],
          ],
        ],
      ],
    ])
  );
  const ch = el.querySelector('[data-testid="gc"]');
  assertEq(ch.style.gridColumn, 'span 2');
  assertEq(ch.style.gridRow, 'span 1');
  assertEq(ch.style.gridColumnStart, '2');
  assertEq(ch.style.alignSelf, 'center');
  assertEq(ch.style.justifySelf, 'space-around');
});

test('Grid applies row_gap/column_gap/align_items classes when present', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(GRID_TAG, [
      [0, { kind: 'equal', count: 2 }],
      [1, 'md'],
      [2, 'xs'],
      [3, 'lg'],
      [4, []],
      [5, 'sm'],
      [6, 'baseline'],
    ])
  );
  assert(el.classList.contains('tf-grid--row-gap-xs'));
  assert(el.classList.contains('tf-grid--col-gap-lg'));
  assert(el.classList.contains('tf-grid--padding-sm'));
  assert(el.classList.contains('tf-grid--align-baseline'));
});

// ============================================================================
// Stack
// ============================================================================

test('Stack uses defaults gap=md align=stretch when fields absent', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(STACK_TAG, []));
  assert(el.classList.contains('tf-stack'));
  assert(el.classList.contains('tf-stack--gap-md'));
  assert(el.classList.contains('tf-stack--align-stretch'));
});

test('Stack renders children + optional padding', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(STACK_TAG, [
      [0, 'xl'],
      [1, 'center'],
      [2, [comp(DIVIDER_TAG, [[0, 'horizontal'], [1, 'default'], [2, 'sm']])]],
      [3, 'md'],
    ])
  );
  assert(el.classList.contains('tf-stack--gap-xl'));
  assert(el.classList.contains('tf-stack--align-center'));
  assert(el.classList.contains('tf-stack--padding-md'));
  assertEq(el.children.length, 1);
  assert(el.children[0].classList.contains('tf-divider-rule'));
});

test('Stack rejects invalid align', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(STACK_TAG, [[0, 'md'], [1, 'middle'], [2, []]]))
  );
});

// ============================================================================
// Cluster
// ============================================================================

test('Cluster renders with all 3 mandatory tokens + children', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(CLUSTER_TAG, [
      [0, 'sm'],
      [1, 'start'],
      [2, 'space_evenly'],
      [3, [comp(SPACER_TAG, [[0, 'xs'], [1, 'x']])]],
    ])
  );
  assert(el.classList.contains('tf-cluster'));
  assert(el.classList.contains('tf-cluster--gap-sm'));
  assert(el.classList.contains('tf-cluster--align-start'));
  assert(el.classList.contains('tf-cluster--justify-space_evenly'));
  assertEq(el.children.length, 1);
});

test('Flex rejects unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(FLEX_TAG, [
        [0, 'row'], [1, 'md'], [2, 'start'], [3, 'center'], [4, 'no_wrap'],
        [99, 'rogue'],
      ])
    )
  );
});

test('GridCol.fr rejects extra keys', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(GRID_TAG, [
        [0, { kind: 'explicit', cols: [{ kind: 'fr', value: 1, malicious: true }] }],
        [1, 'md'],
        [4, []],
      ])
    )
  );
});

test('GridTrack.equal rejects cols field (variant mixing)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(GRID_TAG, [
        [0, { kind: 'equal', count: 3, cols: [] }],
        [1, 'md'],
        [4, []],
      ])
    )
  );
});

test('GridTrack.explicit rejects cols.length > MAX_GRID_COLS', () => {
  setup();
  const engine = makeEngine();
  const tooMany = new Array(300).fill({ kind: 'auto' });
  assertThrows(() =>
    engine.render(
      comp(GRID_TAG, [
        [0, { kind: 'explicit', cols: tooMany }],
        [1, 'md'],
        [4, []],
      ])
    )
  );
});

test('GridCol.px rejects value > MAX_GRID_PX', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(GRID_TAG, [
        [0, { kind: 'explicit', cols: [{ kind: 'px', value: 9_999_999 }] }],
        [1, 'md'],
        [4, []],
      ])
    )
  );
});

test('Cluster rejects missing gap', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(CLUSTER_TAG, [[1, 'start'], [2, 'center'], [3, []]]))
  );
});

// ============================================================================
// Split
// ============================================================================

// All 7 fields are required by SPLIT_SCHEMA (schema/data.rs).
function splitFields(overrides = {}) {
  const base = {
    0: 'horizontal',
    1: { kind: 'percent', value: 30 },
    2: 100,
    3: 600,
    4: false,
    5: 'pane-a',
    6: 'pane-b',
  };
  Object.assign(base, overrides);
  return Object.entries(base)
    .filter(([, v]) => v !== undefined)
    .map(([k, v]) => [Number(k), v]);
}

test('Split horizontal renders panes, divider and percent basis', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SPLIT_TAG, splitFields()));
  assert(el.classList.contains('tf-split'));
  assert(el.classList.contains('tf-split--horizontal'));
  assert(!el.classList.contains('tf-split--resizable'));
  assertEq(el.children.length, 3);
  const [primary, divider, secondary] = el.children;
  assert(primary.classList.contains('tf-split__pane--primary'));
  assertEq(primary.getAttribute('data-slot-id'), 'pane-a');
  assertEq(primary.style.flexBasis, '30%');
  assertEq(primary.style.minWidth, '100px');
  assertEq(primary.style.maxWidth, '600px');
  assertEq(divider.getAttribute('role'), 'separator');
  // Horizontal split (panes side by side) → the divider line is vertical.
  assertEq(divider.getAttribute('aria-orientation'), 'vertical');
  assert(secondary.classList.contains('tf-split__pane--secondary'));
  assertEq(secondary.getAttribute('data-slot-id'), 'pane-b');
});

test('Split vertical with px size uses min/max-height', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SPLIT_TAG, splitFields({ 0: 'vertical', 1: { kind: 'px', value: 240 } }))
  );
  assert(el.classList.contains('tf-split--vertical'));
  const primary = el.children[0];
  assertEq(primary.style.flexBasis, '240px');
  assertEq(primary.style.minHeight, '100px');
  assertEq(primary.style.maxHeight, '600px');
  assertEq(el.children[1].getAttribute('aria-orientation'), 'horizontal');
});

test('Split auto size maps to flex-basis auto', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SPLIT_TAG, splitFields({ 1: { kind: 'auto' } })));
  assertEq(el.children[0].style.flexBasis, 'auto');
});

test('Split accepts BigInt min/max/px from wire decoder', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SPLIT_TAG, splitFields({ 1: { kind: 'px', value: 200n }, 2: 50n, 3: 400n }))
  );
  const primary = el.children[0];
  assertEq(primary.style.flexBasis, '200px');
  assertEq(primary.style.minWidth, '50px');
  assertEq(primary.style.maxWidth, '400px');
});

test('Split validation rejects malformed fields', () => {
  setup();
  const engine = makeEngine();
  const cases = [
    splitFields({ 0: 'diagonal' }),                          // bad orientation
    splitFields({ 1: { kind: 'percent', value: 120 } }),     // percent > 100
    splitFields({ 1: { kind: 'percent', value: NaN } }),     // non-finite percent
    splitFields({ 1: { kind: 'auto', value: 5 } }),          // auto must not carry value
    splitFields({ 1: { kind: 'px', value: -1 } }),           // negative px
    splitFields({ 2: 700 }),                                 // min_primary > max_primary
    splitFields({ 4: undefined }),                           // missing resizable
    splitFields({ 5: '' }),                                  // empty primary_slot
    splitFields().concat([[7, 'x']]),                        // unknown field key
  ];
  for (const fields of cases) {
    assertThrows(() => engine.render(comp(SPLIT_TAG, fields)));
  }
});

test('Split resizable drag clamps flex-basis to min/max', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SPLIT_TAG, splitFields({ 4: true })));
  assert(el.classList.contains('tf-split--resizable'));
  const [primary, divider] = el.children;
  // happy-dom getBoundingClientRect returns 0 → startSize = 0; drag deltas
  // drive the basis directly, clamped to [min_primary, max_primary].
  divider.dispatchEvent(new MouseEvent('pointerdown', { clientX: 200, bubbles: true }));
  document.dispatchEvent(new MouseEvent('pointermove', { clientX: 350 }));
  assertEq(primary.style.flexBasis, '150px');
  document.dispatchEvent(new MouseEvent('pointermove', { clientX: 1200 }));
  assertEq(primary.style.flexBasis, '600px'); // clamped to max_primary
  document.dispatchEvent(new MouseEvent('pointermove', { clientX: 210 }));
  assertEq(primary.style.flexBasis, '100px'); // clamped to min_primary
  document.dispatchEvent(new MouseEvent('pointerup', {}));
  document.dispatchEvent(new MouseEvent('pointermove', { clientX: 500 }));
  // After pointerup the drag is finished — no further resizing.
  assertEq(primary.style.flexBasis, '100px');
});

test('Split destroy removes document drag listeners', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(SPLIT_TAG, splitFields({ 4: true })));
  const [primary, divider] = el.children;
  divider.dispatchEvent(new MouseEvent('pointerdown', { clientX: 0, bubbles: true }));
  document.dispatchEvent(new MouseEvent('pointermove', { clientX: 300 }));
  assertEq(primary.style.flexBasis, '300px');
  engine.destroy(el);
  document.dispatchEvent(new MouseEvent('pointermove', { clientX: 550 }));
  // Listeners are unhooked on destroy — basis stays frozen.
  assertEq(primary.style.flexBasis, '300px');
});

// ============================================================================
// Cleanup propagacja przez children
// ============================================================================

test('destroy on Flex root unhooks bound child subscriptions', () => {
  setup();
  const store = new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'lbl' }], value: 'first' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = new ComponentRenderer({
    store,
    eventDispatcher: makeDispatcher(),
    locale: 'en-US',
  });
  const child = comp(DIVIDER_TAG, [
    [0, 'horizontal'],
    [1, 'default'],
    [2, 'sm'],
    [3, { kind: 'bound', path: [{ kind: 'key', value: 'lbl' }] }],
  ]);
  const root = engine.render(
    comp(FLEX_TAG, [
      [0, 'row'],
      [1, 'sm'],
      [2, 'start'],
      [3, 'center'],
      [4, 'no_wrap'],
      [5, [child]],
    ])
  );
  const labelEl = root.querySelector('.tf-divider__label');
  assertEq(labelEl.textContent, 'first');
  engine.destroy(root);
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      {
        path: [{ kind: 'key', value: 'lbl' }],
        op: { kind: 'set', value: 'second' },
      },
    ],
  });
  // Po destroy subskrypcja dziecka jest zwolniona — tekst nie zmienia się.
  assertEq(labelEl.textContent, 'first');
});

// ============================================================================
// BoxStyle (spec §1.5) — shared container styling
// ============================================================================

// BoxStyle FieldMap: 0=margin, 1=padding, 2=border, 3=background, 4=radius,
// 5=width, 6=height, 7=min_width, 8=min_height, 9=max_width, 10=max_height,
// 11=overflow_x, 12=overflow_y.
function tokenSpace(t) { return { kind: 'token', value: t }; }
function pxSpace(v) { return { kind: 'px', value: v }; }

test('Flex applies BoxStyle margins/paddings/border/background/radius', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(FLEX_TAG, [
      [0, 'row'], [1, 'sm'], [2, 'start'], [3, 'center'], [4, 'no_wrap'],
      [9, [
        [0, [[0, tokenSpace('md')], [2, tokenSpace('md')]]],           // margin y
        [1, [[1, pxSpace(12)], [3, pxSpace(12)]]],                     // padding x px
        [2, [[2, [[0, 2], [1, 'accent'], [2, 'dashed']]]]],            // border bottom
        [3, 'subtle'],                                                 // background
        [4, [[0, tokenSpace('lg')], [1, tokenSpace('lg')]]],           // radius top
        [5, { kind: 'full' }],                                         // width
        [10, { kind: 'px', value: 480 }],                              // max_height
        [12, 'auto'],                                                  // overflow_y
      ]],
    ])
  );
  assertEq(el.style.marginTop, 'var(--tf-space-md)');
  assertEq(el.style.marginBottom, 'var(--tf-space-md)');
  assertEq(el.style.marginLeft, '');
  assertEq(el.style.paddingRight, '12px');
  assertEq(el.style.paddingLeft, '12px');
  assertEq(el.style.borderBottomWidth, '2px');
  assertEq(el.style.borderBottomStyle, 'dashed');
  assertEq(el.style.borderBottomColor, 'var(--tf-accent-1)');
  assertEq(el.style.borderTopStyle, '');
  assertEq(el.style.background, 'var(--tf-bg-subtle)');
  assertEq(el.style.borderTopLeftRadius, 'var(--tf-radius-lg)');
  assertEq(el.style.borderTopRightRadius, 'var(--tf-radius-lg)');
  assertEq(el.style.width, '100%');
  assertEq(el.style.maxHeight, '480px');
  assertEq(el.style.overflowY, 'auto');
});

test('BoxStyle rejects unknown keys, bad tokens and out-of-range px', () => {
  setup();
  const engine = makeEngine();
  const flexWithStyle = (style) =>
    comp(FLEX_TAG, [
      [0, 'row'], [1, 'sm'], [2, 'start'], [3, 'center'], [4, 'no_wrap'],
      [9, style],
    ]);
  // Unknown BoxStyle key 13.
  assertThrows(() => engine.render(flexWithStyle([[13, 'x']])));
  // Unknown Spacing token in margin.
  assertThrows(() => engine.render(flexWithStyle([[0, [[0, tokenSpace('gigantic')]]]])));
  // Px out of u16 range.
  assertThrows(() => engine.render(flexWithStyle([[1, [[0, pxSpace(70000)]]]])));
  // Unknown BorderColor.
  assertThrows(() => engine.render(flexWithStyle([[2, [[0, [[0, 1], [1, 'magenta'], [2, 'solid']]]]]])));
  // Unknown overflow token.
  assertThrows(() => engine.render(flexWithStyle([[11, 'wrap']])));
});

test('BoxStyle rejects duplicate keys in nested FieldMaps', () => {
  setup();
  const engine = makeEngine();
  const flexWithStyle = (style) =>
    comp(FLEX_TAG, [
      [0, 'row'], [1, 'sm'], [2, 'start'], [3, 'center'], [4, 'no_wrap'],
      [9, style],
    ]);
  // Duplicate BoxStyle key (padding twice).
  assertThrows(() => engine.render(flexWithStyle([
    [1, [[0, pxSpace(4)]]],
    [1, [[0, pxSpace(8)]]],
  ])));
  // Duplicate edge inside EdgeValues (margin.top twice — first-wins forbidden).
  assertThrows(() => engine.render(flexWithStyle([
    [0, [[0, pxSpace(4)], [0, pxSpace(8)]]],
  ])));
  // Duplicate side inside BorderEdges.
  assertThrows(() => engine.render(flexWithStyle([
    [2, [[0, [[0, 1], [1, 'default'], [2, 'solid']]], [0, [[0, 3], [1, 'danger'], [2, 'solid']]]]],
  ])));
  // Duplicate field inside BorderSide (width_px twice).
  assertThrows(() => engine.render(flexWithStyle([
    [2, [[0, [[0, 1], [0, 2], [1, 'default'], [2, 'solid']]]]],
  ])));
  // Duplicate corner inside CornerValues.
  assertThrows(() => engine.render(flexWithStyle([
    [4, [[0, tokenSpace('md')], [0, tokenSpace('lg')]]],
  ])));
});

test('BorderLineStyle none maps to border none', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(FLEX_TAG, [
      [0, 'row'], [1, 'sm'], [2, 'start'], [3, 'center'], [4, 'no_wrap'],
      [9, [[2, [[0, [[0, 1], [1, 'default'], [2, 'none']]]]]]],
    ])
  );
  assertEq(el.style.borderTopStyle, 'none');
  assertEq(el.style.borderTopWidth, '');
});

test('Grid and Stack accept BoxStyle field', () => {
  setup();
  const engine = makeEngine();
  const grid = engine.render(
    comp(GRID_TAG, [
      [0, { kind: 'equal', count: 2 }],
      [1, 'md'],
      [4, []],
      [7, [[1, [[0, pxSpace(8)]]]]],
    ])
  );
  assertEq(grid.style.paddingTop, '8px');
  const stack = engine.render(
    comp(STACK_TAG, [
      [5, [[0, [[3, tokenSpace('xl')]]]]],
    ])
  );
  assertEq(stack.style.marginLeft, 'var(--tf-space-xl)');
});

// ============================================================================
// Box (0x0115) — style(6), direction(7), gap(8), align(9), justify(10)
// ============================================================================

test('Box renders flex behavior + BoxStyle', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BOX_TAG, [
      [5, []],
      [6, [[2, [[0, [[0, 1], [1, 'default'], [2, 'solid']]], [1, [[0, 1], [1, 'default'], [2, 'solid']]], [2, [[0, 1], [1, 'default'], [2, 'solid']]], [3, [[0, 1], [1, 'default'], [2, 'solid']]]]]]],
      [7, 'column'],
      [8, 'sm'],
      [9, 'center'],
      [10, 'space_between'],
    ])
  );
  assert(el.classList.contains('tf-box'));
  assertEq(el.style.display, 'flex');
  assertEq(el.style.flexDirection, 'column');
  assertEq(el.style.gap, 'var(--tf-space-sm)');
  assertEq(el.style.alignItems, 'center');
  assertEq(el.style.justifyContent, 'space-between');
  assertEq(el.style.borderTopWidth, '1px');
  assertEq(el.style.borderTopStyle, 'solid');
  assertEq(el.style.borderTopColor, 'var(--tf-border)');
  assertEq(el.style.borderLeftStyle, 'solid');
});

test('Box grow=true → flex-grow 1 + flex-basis 0 (equal fill)', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BOX_TAG, [[1, true], [5, []]]));
  assertEq(el.style.flexGrow, '1');
  // happy-dom normalizes `0` → `0px`; both are the same zero flex-basis.
  assert(el.style.flexBasis === '0' || el.style.flexBasis === '0px',
    `flex-basis should be zero, got ${el.style.flexBasis}`);
});

test('Box grow absent → no flex-grow/basis', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BOX_TAG, [[5, []]]));
  assertEq(el.style.flexGrow, '');
  assertEq(el.style.flexBasis, '');
});

test('Box without flex fields stays a plain div', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BOX_TAG, [[5, []]]));
  assertEq(el.style.display, '');
});

test('Box rejects invalid direction and unknown field key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BOX_TAG, [[7, 'diagonal']])));
  assertThrows(() => engine.render(comp(BOX_TAG, [[11, 'x']])));
});

// ============================================================================
// Bootstrap
// ============================================================================

test('bootstrap registers atomic + containers renderers idempotentnie', () => {
  _clearComponentRendererRegistry();
  bootstrapSdkRuntime();
  // Druga inwokacja — nie rzuca duplicate-register.
  bootstrapSdkRuntime();
  // Tag z atomic (SPACER) + tag z containers (FLEX) muszą być widoczne.
  const engine = makeEngine();
  engine.render(comp(SPACER_TAG, [[0, 'md'], [1, 'x']]));
  engine.render(
    comp(FLEX_TAG, [
      [0, 'row'], [1, 'sm'], [2, 'start'], [3, 'center'], [4, 'no_wrap'],
    ])
  );
});

// ============================================================================
// BoxStyle.shadow (key 13)
// ============================================================================

test('BoxStyle shadow=elevated → box-shadow token', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(FLEX_TAG, [
      [0, 'row'], [2, 'start'], [3, 'stretch'], [4, 'no_wrap'],
      [9, [[13, 'elevated']]],
    ])
  );
  assertEq(el.style.boxShadow, 'var(--tf-shadow-lg)');
});

test('BoxStyle shadow=accent_glow → accent halo token', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(BOX_TAG, [[6, [[13, 'accent_glow']]]])
  );
  assertEq(el.style.boxShadow, 'var(--tf-glow-accent)');
});

test('BoxStyle shadow=none → none', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BOX_TAG, [[6, [[13, 'none']]]]));
  assertEq(el.style.boxShadow, 'none');
});

test('BoxStyle unknown shadow token rejected', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(BOX_TAG, [[6, [[13, 'glow']]]])));
});

// ============================================================================
// ResponsiveRule (Flex key 10 / Stack key 6 / Box key 11)
// ============================================================================

test('Flex responsive → data-responsive + injected @container rule', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(FLEX_TAG, [
      [0, 'row'], [2, 'start'], [3, 'stretch'], [4, 'no_wrap'],
      [5, []],
      // one rule: at Px(680) stack column with lg gap
      [10, [[[0, { kind: 'px', value: 680 }], [1, 'column'], [2, 'lg']]]],
    ])
  );
  const hash = el.getAttribute('data-responsive');
  assert(hash && /^[0-9a-f]{8}$/.test(hash), 'stable 8-hex hash on data-responsive');
  const css = injectedResponsiveCss();
  assert(css.includes('@container addon (max-width: 680px)'), 'container query at 680px');
  assert(css.includes(`[data-responsive="${hash}"]`), 'scoped by hash selector');
  assert(css.includes('flex-direction:column !important'), 'direction override with !important');
  assert(css.includes('gap:var(--tf-space-lg) !important'), 'gap override with !important');
});

test('Responsive Breakpoint token maps to px scale', () => {
  setup();
  const engine = makeEngine();
  engine.render(
    comp(STACK_TAG, [
      [2, []],
      [6, [[[0, { kind: 'token', value: 'md' }], [1, 'column']]]],
    ])
  );
  assert(injectedResponsiveCss().includes('(max-width: 1024px)'), 'md → 1024px');
});

test('Responsive order + hidden target the element itself', () => {
  setup();
  const engine = makeEngine();
  engine.render(
    comp(BOX_TAG, [
      [11, [[[0, { kind: 'px', value: 460 }], [7, 2], [8, true]]]],
    ])
  );
  const css = injectedResponsiveCss();
  assert(css.includes('order:2 !important'), 'order override with !important');
  assert(css.includes('display:none !important'), 'hidden → display:none with !important');
});

test('Responsive emits smaller max_width LAST so it wins the cascade', () => {
  setup();
  const engine = makeEngine();
  // Author order deliberately narrow-first; the emitted CSS must still put the
  // wider (680) block before the narrower (460) one.
  engine.render(comp(FLEX_TAG, [
    [0, 'row'], [2, 'start'], [3, 'stretch'], [4, 'no_wrap'], [5, []],
    [10, [
      [[0, { kind: 'px', value: 460 }], [2, 'sm']],
      [[0, { kind: 'px', value: 680 }], [2, 'lg']],
    ]],
  ]));
  const css = injectedResponsiveCss();
  const i680 = css.indexOf('(max-width: 680px)');
  const i460 = css.indexOf('(max-width: 460px)');
  assert(i680 !== -1 && i460 !== -1, 'both breakpoints emitted');
  assert(i680 < i460, '680px block precedes 460px block so 460 overrides');
});

test('Responsive rules deduped by content hash (no duplicate injection)', () => {
  setup();
  const engine = makeEngine();
  const rule = [[[0, { kind: 'px', value: 680 }], [1, 'column']]];
  engine.render(comp(FLEX_TAG, [[0, 'row'], [2, 'start'], [3, 'stretch'], [4, 'no_wrap'], [5, []], [10, rule]]));
  engine.render(comp(FLEX_TAG, [[0, 'row'], [2, 'start'], [3, 'stretch'], [4, 'no_wrap'], [5, []], [10, rule]]));
  const css = injectedResponsiveCss();
  const occurrences = css.split('@container addon (max-width: 680px)').length - 1;
  assertEq(occurrences, 1);
});

test('Responsive padding EdgeValues → padding longhands', () => {
  setup();
  const engine = makeEngine();
  engine.render(
    comp(BOX_TAG, [
      [11, [[[0, { kind: 'px', value: 460 }],
        [5, [[0, { kind: 'token', value: 'sm' }], [2, { kind: 'px', value: 12 }]]]]]],
    ])
  );
  const css = injectedResponsiveCss();
  assert(css.includes('padding-top:var(--tf-space-sm) !important'), 'top from token');
  assert(css.includes('padding-bottom:12px !important'), 'bottom from px');
});

test('Responsive empty array is a no-op', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(comp(BOX_TAG, [[11, []]]));
  assert(!el.hasAttribute('data-responsive'), 'no attribute for empty rules');
  assertEq(injectedResponsiveCss(), '');
});

// ---- report ----

function reportResults() {
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
  return { pass, fail, text: lines.join('\n') };
}

if (typeof process !== 'undefined') {
  const r = reportResults();
  // eslint-disable-next-line no-console
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}

export { reportResults };
