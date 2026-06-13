// =============================================================================
// Plik: sdk-runtime/component-renderer.test.js
// Opis: Testy engine'u renderowania + 3 atomic renderers (Krok 3.3a-1).
// =============================================================================

import './_dom-test-harness.js';

import { StateStore } from './state-store.js';
import {
  ComponentRenderer,
  registerComponentRenderer,
  lookupComponentRenderer,
  _clearComponentRendererRegistry,
} from './component-renderer.js';
import {
  registerLayoutAtomicRenderers,
  DIVIDER_TAG,
  SPACER_TAG,
  TOOLTIP_TAG,
} from './layout-atomic-renderers.js';

// ---- harness ----

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

// ---- helpery ----

function makeStore() {
  return new StateStore({ addon_id: 'a', panel_id: 'p', panel_epoch: 1n });
}

function makeDispatcher() {
  const emitted = [];
  return {
    emit(evt) {
      emitted.push(evt);
    },
    emitted,
  };
}

function makeEngine(store, dispatcher) {
  return new ComponentRenderer({
    store: store || makeStore(),
    eventDispatcher: dispatcher || makeDispatcher(),
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

// Reset registry + DOM przed każdym testem.
function setup() {
  _clearComponentRendererRegistry();
  registerLayoutAtomicRenderers();
  document.body.innerHTML = '';
}

// ============================================================================
// Engine: ctor + dispatch
// ============================================================================

test('ComponentRenderer ctor requires store + dispatcher', () => {
  assertThrows(() => new ComponentRenderer({}));
  assertThrows(() => new ComponentRenderer({ store: makeStore() }));
  assertThrows(() =>
    new ComponentRenderer({ store: makeStore(), eventDispatcher: {} })
  );
});

test('ComponentRenderer.render rejects unregistered tag', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() => engine.render(comp(0xDEAD, [])));
});

test('registerComponentRenderer rejects duplicate tag', () => {
  setup();
  assertThrows(() => registerComponentRenderer(DIVIDER_TAG, () => null));
});

test('registerComponentRenderer rejects bad inputs', () => {
  setup();
  assertThrows(() => registerComponentRenderer('x', () => null));
  assertThrows(() => registerComponentRenderer(0x9999, 'not-fn'));
  assertThrows(() => registerComponentRenderer(-1, () => null));
});

// ============================================================================
// Divider (0x0108)
// ============================================================================

test('Divider renders div with semantic classes', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(DIVIDER_TAG, [
      [0, 'horizontal'],
      [1, 'subtle'],
      [2, 'md'],
    ])
  );
  assertEq(el.tagName, 'DIV');
  assert(el.classList.contains('tf-divider-rule'));
  assert(el.classList.contains('tf-divider--horizontal'));
  assert(el.classList.contains('tf-divider--subtle'));
  assert(el.classList.contains('tf-divider--spacing-md'));
  assertEq(el.getAttribute('role'), 'separator');
  assertEq(el.getAttribute('aria-orientation'), 'horizontal');
});

test('Divider vertical sets aria-orientation', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(DIVIDER_TAG, [
      [0, 'vertical'],
      [1, 'default'],
      [2, 'sm'],
    ])
  );
  assertEq(el.getAttribute('aria-orientation'), 'vertical');
});

test('Divider with literal label renders span', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(DIVIDER_TAG, [
      [0, 'horizontal'],
      [1, 'default'],
      [2, 'md'],
      [3, { kind: 'literal', value: 'OR' }],
    ])
  );
  const labelEl = el.querySelector('.tf-divider__label');
  assert(labelEl != null, 'label span exists');
  assertEq(labelEl.textContent, 'OR');
});

test('Divider with bound label updates reactively', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'sep' }], value: 'Before' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(DIVIDER_TAG, [
      [0, 'horizontal'],
      [1, 'default'],
      [2, 'md'],
      [3, { kind: 'bound', path: [{ kind: 'key', value: 'sep' }] }],
    ])
  );
  const labelEl = el.querySelector('.tf-divider__label');
  assertEq(labelEl.textContent, 'Before');
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      {
        path: [{ kind: 'key', value: 'sep' }],
        op: { kind: 'set', value: 'After' },
      },
    ],
  });
  assertEq(labelEl.textContent, 'After');
});

test('Divider rejects invalid orientation', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(DIVIDER_TAG, [
        [0, 'diagonal'],
        [1, 'default'],
        [2, 'md'],
      ])
    )
  );
});

// ============================================================================
// Spacer (0x0109)
// ============================================================================

test('Spacer renders aria-hidden div with size+axis classes', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SPACER_TAG, [
      [0, 'lg'],
      [1, 'y'],
    ])
  );
  assertEq(el.tagName, 'DIV');
  assert(el.classList.contains('tf-spacer'));
  assert(el.classList.contains('tf-spacer--size-lg'));
  assert(el.classList.contains('tf-spacer--axis-y'));
  assertEq(el.getAttribute('aria-hidden'), 'true');
});

test('Spacer rejects invalid axis', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SPACER_TAG, [
        [0, 'md'],
        [1, 'diagonal'],
      ])
    )
  );
});

// ============================================================================
// Tooltip (0x010F)
// ============================================================================

test('Tooltip aria-describedby merges with existing child describedby', () => {
  setup();
  const engine = makeEngine();
  const wrapper = engine.render(
    comp(TOOLTIP_TAG, [
      [
        0,
        comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
          a11y: { described_by: 'help-1' },
        }),
      ],
      [1, { kind: 'literal', value: 'Hi' }],
      [2, 'top'],
      [3, 160],
    ])
  );
  const child = wrapper.querySelector('.tf-spacer');
  const refs = child.getAttribute('aria-describedby').split(' ');
  assert(refs.includes('help-1'), 'preserves existing described_by');
  assert(refs.length === 2, 'adds tooltip id alongside');
});

test('Component validation rejects bad bind shape (must be object, not array)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
        bind: [{ kind: 'text', path: [{ kind: 'key', value: 'x' }] }],
      })
    )
  );
});

test('Component validation rejects duplicate FieldMap key', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(comp(SPACER_TAG, [[0, 'md'], [0, 'lg'], [1, 'x']]))
  );
});

test('Component validation rejects test_id with disallowed chars', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SPACER_TAG, [[0, 'md'], [1, 'x']], { test_id: 'Spacer!' })
    )
  );
});

test('Render cleanups are released if applyBindings throws', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'lbl' }], value: 'A' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  // applyBindings rzuci na BindSpec.list — earlier subscriptions
  // (a11y.label) muszą zostać zwolnione, inaczej store leak'uje.
  const before = store._subscribers.size;
  try {
    engine.render(
      comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
        a11y: { label: { kind: 'bound', path: [{ kind: 'key', value: 'lbl' }] } },
        bind: {
          kind: 'list',
          path: [{ kind: 'key', value: 'rows' }],
          item_template_id: 'r',
        },
      })
    );
    throw new Error('expected render to throw');
  } catch (e) {
    // Render rzucił — sprawdzamy że store nie ma stałego subscribera.
  }
  const after = store._subscribers.size;
  assertEq(after, before);
});

test('Render error-path frees already-rendered child subscriptions (no leak)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'lbl' }], value: 'A' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  // Parent renderer renders a child via ctx.renderChild (whose subscription
  // lives on the CHILD element in the WeakMap, not on the parent's local
  // cleanups), then throws. The catch must recursively destroy the rendered
  // child so its subscription is released — otherwise the store leaks it.
  const PARENT_TAG = 0xFEE0;
  registerComponentRenderer(PARENT_TAG, (component, ctx) => {
    const el = document.createElement('div');
    const child = ctx.readField(component.fields, 0);
    el.appendChild(ctx.renderChild(child));
    throw new Error('parent render blows up after child rendered');
  });

  const before = store._subscribers.size;
  const childComp = comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
    a11y: { label: { kind: 'bound', path: [{ kind: 'key', value: 'lbl' }] } },
  });
  childComp.id = 'child1';
  const parentComp = comp(PARENT_TAG, [[0, childComp]]);
  parentComp.id = 'parent1';

  assertThrows(() => engine.render(parentComp), 'parent render must throw');
  const after = store._subscribers.size;
  assertEq(after, before, 'child subscription freed on parent render error');

  // The freed subscription must not react to later patches.
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      { path: [{ kind: 'key', value: 'lbl' }], op: { kind: 'set', value: 'B' } },
    ],
  });
  // No assertion on DOM (detached) — the subscriber-count check above is the
  // hard leak guard.
});

test('Tooltip wraps child and exposes aria-describedby', () => {
  setup();
  const engine = makeEngine();
  const childComp = comp(SPACER_TAG, [
    [0, 'md'],
    [1, 'x'],
  ]);
  const wrapper = engine.render(
    comp(TOOLTIP_TAG, [
      [0, childComp],
      [1, { kind: 'literal', value: 'Hint' }],
      [2, 'top'],
      [3, 200],
    ])
  );
  assert(wrapper.classList.contains('tf-tooltip-wrapper'));
  const tooltip = wrapper.querySelector('.tf-tooltip');
  assert(tooltip != null, 'tooltip child exists');
  assertEq(tooltip.getAttribute('role'), 'tooltip');
  assertEq(tooltip.getAttribute('hidden'), '');
  assertEq(tooltip.textContent, 'Hint');
  assertEq(tooltip.style.maxWidth, '200px');
  const child = wrapper.querySelector('.tf-spacer');
  assert(child != null, 'child rendered');
  assertEq(child.getAttribute('aria-describedby'), tooltip.getAttribute('id'));
});

test('Tooltip shows on mouseenter, hides on mouseleave', () => {
  setup();
  const engine = makeEngine();
  const wrapper = engine.render(
    comp(TOOLTIP_TAG, [
      [0, comp(SPACER_TAG, [[0, 'md'], [1, 'x']])],
      [1, { kind: 'literal', value: 'Help' }],
      [2, 'right'],
      [3, 160],
    ])
  );
  const tooltip = wrapper.querySelector('.tf-tooltip');
  const child = wrapper.querySelector('.tf-spacer');
  assertEq(tooltip.hasAttribute('hidden'), true);
  child.dispatchEvent(new window.Event('mouseenter'));
  assertEq(tooltip.hasAttribute('hidden'), false);
  child.dispatchEvent(new window.Event('mouseleave'));
  assertEq(tooltip.hasAttribute('hidden'), true);
});

test('Tooltip content updates reactively via BindRef::Bound', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'msg' }], value: 'v1' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const wrapper = engine.render(
    comp(TOOLTIP_TAG, [
      [0, comp(SPACER_TAG, [[0, 'md'], [1, 'x']])],
      [1, { kind: 'bound', path: [{ kind: 'key', value: 'msg' }] }],
      [2, 'bottom'],
      [3, 160],
    ])
  );
  const tooltip = wrapper.querySelector('.tf-tooltip');
  assertEq(tooltip.textContent, 'v1');
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      {
        path: [{ kind: 'key', value: 'msg' }],
        op: { kind: 'set', value: 'v2' },
      },
    ],
  });
  assertEq(tooltip.textContent, 'v2');
});

test('Tooltip rejects bad max_width_px', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(TOOLTIP_TAG, [
        [0, comp(SPACER_TAG, [[0, 'md'], [1, 'x']])],
        [1, { kind: 'literal', value: 'x' }],
        [2, 'top'],
        [3, 99999],
      ])
    )
  );
});

// ============================================================================
// Common attributes: id, test_id, a11y, visibility
// ============================================================================

test('Engine sets data-component-id from Component.id', () => {
  setup();
  const engine = makeEngine();
  const c = comp(SPACER_TAG, [[0, 'md'], [1, 'x']]);
  c.id = 'my-spacer';
  const el = engine.render(c);
  assertEq(el.getAttribute('data-component-id'), 'my-spacer');
});

test('Engine sets data-testid from Component.test_id', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], { test_id: 'spacer-1' })
  );
  assertEq(el.getAttribute('data-testid'), 'spacer-1');
});

test('Engine applies a11y role and label_for', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      a11y: { role: 'presentation', label_for: 'other-id' },
    })
  );
  assertEq(el.getAttribute('role'), 'presentation');
  assertEq(el.getAttribute('aria-labelledby'), 'other-id');
});

test('Engine binds aria-label from BindRef and reacts to changes', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'lbl' }], value: 'A' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      a11y: {
        label: { kind: 'bound', path: [{ kind: 'key', value: 'lbl' }] },
      },
    })
  );
  assertEq(el.getAttribute('aria-label'), 'A');
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      {
        path: [{ kind: 'key', value: 'lbl' }],
        op: { kind: 'set', value: 'B' },
      },
    ],
  });
  assertEq(el.getAttribute('aria-label'), 'B');
});

test('Engine applies aria-expanded / disabled as bool BindRef', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      a11y: {
        expanded: { kind: 'literal', value: true },
        disabled: { kind: 'literal', value: false },
      },
    })
  );
  assertEq(el.getAttribute('aria-expanded'), 'true');
  assertEq(el.getAttribute('aria-disabled'), 'false');
});

test('Engine applies visibility.visible hide+aria-hidden', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'shown' }], value: false }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      visibility: {
        visible: { kind: 'bound', path: [{ kind: 'key', value: 'shown' }] },
      },
    })
  );
  assertEq(el.hasAttribute('hidden'), true);
  assertEq(el.getAttribute('aria-hidden'), 'true');
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      {
        path: [{ kind: 'key', value: 'shown' }],
        op: { kind: 'set', value: true },
      },
    ],
  });
  assertEq(el.hasAttribute('hidden'), false);
});

test('Engine applies visibility display_above/below_breakpoint as data attrs', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      visibility: {
        display_above_breakpoint: 'md',
        display_below_breakpoint: 'xl',
      },
    })
  );
  assertEq(el.getAttribute('data-visibility-above'), 'md');
  assertEq(el.getAttribute('data-visibility-below'), 'xl');
});

test('Engine honors hidden_for_assistive independently of visible', () => {
  setup();
  const engine = makeEngine();
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      visibility: {
        hidden_for_assistive: true,
        visible: { kind: 'literal', value: true },
      },
    })
  );
  assertEq(el.getAttribute('aria-hidden'), 'true');
  assertEq(el.hasAttribute('hidden'), false);
});

// ============================================================================
// Event handlers
// ============================================================================

test('Engine attaches click handler and emits via dispatcher', () => {
  setup();
  const store = makeStore();
  const dispatcher = makeDispatcher();
  const engine = makeEngine(store, dispatcher);
  const c = comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
    handlers: [['click', { kind: 'backend', operation_id: 'op1' }]],
  });
  const el = engine.render(c);
  el.dispatchEvent(new window.Event('click'));
  assertEq(dispatcher.emitted.length, 1);
  assertEq(dispatcher.emitted[0].event_kind, 'click');
  assertEq(dispatcher.emitted[0].source_id, 'cmp1');
  assertEq(dispatcher.emitted[0].addon_id, 'a');
});

test('Engine rejects unknown EventKind not in spec whitelist', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
        handlers: [['hover_dance', { kind: 'backend', operation_id: 'op' }]],
      })
    )
  );
});

test('Engine maps EventKind key_down to DOM keydown', () => {
  setup();
  const dispatcher = makeDispatcher();
  const engine = makeEngine(undefined, dispatcher);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      handlers: [['key_down', { kind: 'backend', operation_id: 'op' }]],
    })
  );
  el.dispatchEvent(new window.KeyboardEvent('keydown'));
  assertEq(dispatcher.emitted[0].event_kind, 'key_down');
});

test('Engine attaches row_click as custom event listener', () => {
  setup();
  const dispatcher = makeDispatcher();
  const engine = makeEngine(undefined, dispatcher);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      handlers: [['row_click', { kind: 'backend', operation_id: 'op' }]],
    })
  );
  el.dispatchEvent(new window.CustomEvent('row_click'));
  assertEq(dispatcher.emitted[0].event_kind, 'row_click');
});

test('Engine maps EventKind context_menu to DOM contextmenu', () => {
  setup();
  const dispatcher = makeDispatcher();
  const engine = makeEngine(undefined, dispatcher);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      handlers: [['context_menu', { kind: 'backend', operation_id: 'op' }]],
    })
  );
  el.dispatchEvent(new window.Event('contextmenu'));
  assertEq(dispatcher.emitted.length, 1);
  assertEq(dispatcher.emitted[0].event_kind, 'context_menu');
});

// ============================================================================
// BindSpec: text / attr / class_toggle / show
// ============================================================================

test('BindSpec.text writes textContent reactively with format', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'price' }], value: 19.99 }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      bind: {
        kind: 'text',
        path: [{ kind: 'key', value: 'price' }],
        format: { kind: 'currency', code: 'USD' },
      },
    })
  );
  assert(el.textContent.includes('19.99'));
  assert(el.textContent.includes('$'));
});

test('BindSpec.attr rejects on* event-handler attribute', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
        bind: {
          kind: 'attr',
          name: 'onclick',
          path: [{ kind: 'key', value: 'x' }],
        },
      })
    )
  );
});

test('BindSpec.attr rejects style attribute', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
        bind: {
          kind: 'attr',
          name: 'style',
          path: [{ kind: 'key', value: 'x' }],
        },
      })
    )
  );
});

test('BindSpec.attr URL with javascript: scheme is rejected (removed)', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: [{ kind: 'key', value: 'u' }], value: 'javascript:alert(1)' },
    ],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      bind: {
        kind: 'attr',
        name: 'href',
        path: [{ kind: 'key', value: 'u' }],
      },
    })
  );
  // Unsafe scheme → attribute usunięty defensywnie.
  assertEq(el.hasAttribute('href'), false);
});

test('BindSpec.attr rejects formaction and action (form hijack)', () => {
  setup();
  const engine = makeEngine();
  for (const name of ['formaction', 'action']) {
    assertThrows(
      () =>
        engine.render(
          comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
            bind: { kind: 'attr', name, path: [{ kind: 'key', value: 'x' }] },
          })
        ),
      `expected '${name}' to be rejected`
    );
  }
});

test('BindSpec.attr rejects srcdoc unconditionally (XSS)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
        bind: {
          kind: 'attr',
          name: 'srcdoc',
          path: [{ kind: 'key', value: 'x' }],
        },
      })
    )
  );
});

test('BindSpec.attr URL rejects whitespace-prefixed javascript: scheme', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'u' }], value: ' javascript:alert(1)' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      bind: { kind: 'attr', name: 'href', path: [{ kind: 'key', value: 'u' }] },
    })
  );
  assertEq(el.hasAttribute('href'), false);
});

test('BindSpec.attr URL rejects newline-embedded scheme', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: [{ kind: 'key', value: 'u' }], value: 'java\nscript:alert(1)' },
    ],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      bind: { kind: 'attr', name: 'href', path: [{ kind: 'key', value: 'u' }] },
    })
  );
  assertEq(el.hasAttribute('href'), false);
});

test('BindSpec.show with hidden_for_assistive preserves aria-hidden', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'shown' }], value: true }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      visibility: { hidden_for_assistive: true },
      bind: { kind: 'show', path: [{ kind: 'key', value: 'shown' }], negate: false },
    })
  );
  // visible=true ale hidden_for_assistive=true ⇒ aria-hidden zostaje.
  assertEq(el.hasAttribute('hidden'), false);
  assertEq(el.getAttribute('aria-hidden'), 'true');
});

test('Engine accepts new EventKind values like event_click and retry', () => {
  setup();
  const dispatcher = makeDispatcher();
  const engine = makeEngine(undefined, dispatcher);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      handlers: [
        ['event_click', { kind: 'backend', operation_id: 'a' }],
        ['retry', { kind: 'backend', operation_id: 'b' }],
      ],
    })
  );
  el.dispatchEvent(new window.CustomEvent('event_click'));
  el.dispatchEvent(new window.CustomEvent('retry'));
  assertEq(dispatcher.emitted.length, 2);
  assertEq(dispatcher.emitted[0].event_kind, 'event_click');
  assertEq(dispatcher.emitted[1].event_kind, 'retry');
});

test('BindSpec.attr URL with http scheme is allowed', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [
      { path: [{ kind: 'key', value: 'u' }], value: 'https://example.com/x' },
    ],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      bind: {
        kind: 'attr',
        name: 'href',
        path: [{ kind: 'key', value: 'u' }],
      },
    })
  );
  assertEq(el.getAttribute('href'), 'https://example.com/x');
});

test('BindSpec.attr sets and removes attr based on value', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'tit' }], value: 'hello' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      bind: {
        kind: 'attr',
        name: 'title',
        path: [{ kind: 'key', value: 'tit' }],
      },
    })
  );
  assertEq(el.getAttribute('title'), 'hello');
});

test('BindSpec.class_toggle adds/removes class with negate', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'on' }], value: true }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      bind: {
        kind: 'class_toggle',
        class_name: 'active',
        path: [{ kind: 'key', value: 'on' }],
        negate: false,
      },
    })
  );
  assert(el.classList.contains('active'));
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      {
        path: [{ kind: 'key', value: 'on' }],
        op: { kind: 'set', value: false },
      },
    ],
  });
  assert(!el.classList.contains('active'));
});

test('BindSpec.show toggles hidden attribute', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'shown' }], value: false }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      bind: {
        kind: 'show',
        path: [{ kind: 'key', value: 'shown' }],
        negate: false,
      },
    })
  );
  assert(el.hasAttribute('hidden'));
});

test('BindSpec.list rejects with explicit error (chunk 3.5)', () => {
  setup();
  const engine = makeEngine();
  assertThrows(() =>
    engine.render(
      comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
        bind: {
          kind: 'list',
          path: [{ kind: 'key', value: 'rows' }],
          item_template_id: 'row',
        },
      })
    )
  );
});

// ============================================================================
// Cleanup
// ============================================================================

test('destroy unsubscribes from store', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'x' }], value: 'a' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const el = engine.render(
    comp(SPACER_TAG, [[0, 'md'], [1, 'x']], {
      a11y: {
        label: { kind: 'bound', path: [{ kind: 'key', value: 'x' }] },
      },
    })
  );
  assertEq(el.getAttribute('aria-label'), 'a');
  engine.destroy(el);
  // Po destroy subskrypcja powinna być zwolniona — kolejny patch nie
  // mutuje atrybutu.
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      {
        path: [{ kind: 'key', value: 'x' }],
        op: { kind: 'set', value: 'b' },
      },
    ],
  });
  assertEq(el.getAttribute('aria-label'), 'a');
});

test('destroy on root cleans up child tooltip subscriptions', () => {
  setup();
  const store = makeStore();
  store.applySnapshot({
    entries: [{ path: [{ kind: 'key', value: 'msg' }], value: 'old' }],
    state_revision: 0,
    truncated: false,
  });
  const engine = makeEngine(store);
  const wrapper = engine.render(
    comp(TOOLTIP_TAG, [
      [0, comp(SPACER_TAG, [[0, 'md'], [1, 'x']])],
      [1, { kind: 'bound', path: [{ kind: 'key', value: 'msg' }] }],
      [2, 'top'],
      [3, 200],
    ])
  );
  const tooltip = wrapper.querySelector('.tf-tooltip');
  assertEq(tooltip.textContent, 'old');
  engine.destroy(wrapper);
  store.applyPatch({
    base_revision: 0,
    new_revision: 1,
    ops: [
      {
        path: [{ kind: 'key', value: 'msg' }],
        op: { kind: 'set', value: 'new' },
      },
    ],
  });
  assertEq(tooltip.textContent, 'old');
});

// ---- report ----

function reportResults(target) {
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
  const text = lines.join('\n');
  if (target) {
    target.textContent = text;
    target.dataset.status = fail === 0 ? 'pass' : 'fail';
  }
  return { pass, fail, text };
}

if (typeof window === 'undefined' || (typeof process !== 'undefined' && process.env && process.env.NODE_ENV !== 'browser')) {
  const r = reportResults(null);
  // eslint-disable-next-line no-console
  console.log(r.text);
  if (r.fail > 0) process.exit(1);
}

export { reportResults };
