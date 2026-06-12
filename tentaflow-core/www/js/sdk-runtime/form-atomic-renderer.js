// =============================================================================
// Plik: sdk-runtime/form-atomic-renderer.js
// Opis: Rendererzy atomic form controls uzywajace tf-toggle, tf-checkbox i
// tf-radio web components:
//   - Toggle   (0x030A) — tf-toggle switch on/off z reactive bind_path
//   - Checkbox (0x030B) — tf-checkbox z opcjonalnym indeterminate
//   - Radio    (0x030C) — tf-radio z SelectValue
// Bind_path: StatePath (read-only reactive), write-back chunk 3.6.
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/form/atomic.rs
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

const TONES = new Set([
  'neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted',
]);
const TOGGLE_SIZES = new Set(['sm', 'md', 'lg']);
const TOGGLE_POSITIONS = new Set(['leading', 'trailing']);
const CHECKBOX_SIZES = new Set(['sm', 'md', 'lg']);
const SELECT_VALUE_KEYS = new Set(['kind', 'value']);
const SELECT_VALUE_KINDS = new Set(['tstr', 'u32', 'i32', 'bool']);

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(
      `${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`
    );
  }
  return v;
}
function requireBool(v, ctx) {
  if (typeof v !== 'boolean') {
    throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
  }
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') {
    throw new TypeError(`${ctx}: expected string, got ${typeof v}`);
  }
  return v;
}
function requirePath(v, ctx) {
  if (!Array.isArray(v)) {
    throw new TypeError(`${ctx}: expected StatePath (Array<PathSegment>)`);
  }
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(
        `${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`
      );
    }
  }
}
function assertOnlyKnownObjectKeys(obj, allowedKeys, ctx) {
  for (const k of Object.keys(obj)) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${ctx}: unexpected key '${k}'`);
    }
  }
}

function parseSelectValue(sv, ctx) {
  if (!sv || typeof sv !== 'object') {
    throw new TypeError(`${ctx}: SelectValue must be object`);
  }
  assertOnlyKnownObjectKeys(sv, SELECT_VALUE_KEYS, ctx);
  if (!SELECT_VALUE_KINDS.has(sv.kind)) {
    throw new TypeError(`${ctx}.kind unsupported: ${sv.kind}`);
  }
  switch (sv.kind) {
    case 'tstr': return { tag: 'tstr', value: requireString(sv.value, `${ctx}.value`) };
    case 'bool': return { tag: 'bool', value: requireBool(sv.value, `${ctx}.value`) };
    case 'u32': {
      let value = sv.value;
      if (typeof value === 'bigint') {
        if (value < 0n || value > 0xFFFFFFFFn) throw new TypeError(`${ctx}.value must be u32`);
        value = Number(value);
      } else if (!Number.isInteger(value) || value < 0 || value > 0xFFFFFFFF) {
        throw new TypeError(`${ctx}.value must be u32`);
      }
      return { tag: 'u32', value };
    }
    case 'i32': {
      let value = sv.value;
      if (typeof value === 'bigint') {
        if (value < -0x80000000n || value > 0x7FFFFFFFn) throw new TypeError(`${ctx}.value must be i32`);
        value = Number(value);
      } else if (!Number.isInteger(value) || value < -0x80000000 || value > 0x7FFFFFFF) {
        throw new TypeError(`${ctx}.value must be i32`);
      }
      return { tag: 'i32', value };
    }
  }
}

function selectValueEquals(parsed, storeValue) {
  if (parsed.tag === 'tstr') return typeof storeValue === 'string' && storeValue === parsed.value;
  if (parsed.tag === 'bool') return typeof storeValue === 'boolean' && storeValue === parsed.value;
  if (parsed.tag === 'u32' || parsed.tag === 'i32') {
    if (typeof storeValue === 'number' && storeValue === parsed.value) return true;
    if (typeof storeValue === 'bigint' && storeValue === BigInt(parsed.value)) return true;
  }
  return false;
}

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

function applyDisabledReactive(element, disabledBind, ctx) {
  if (disabledBind == null) return () => false;
  let isDisabled = false;
  const apply = () => {
    isDisabled = resolveBindRef(disabledBind, ctx.store) === true;
    if (isDisabled) element.setAttribute('disabled', '');
    else element.removeAttribute('disabled');
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, apply));
  return () => isDisabled;
}

// =============================================================================
// Toggle (0x030A)
// =============================================================================

export const TOGGLE_TAG = 0x030A;
const TOGGLE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

function renderToggle(component, ctx) {
  assertOnlyKnownFields(component.fields, TOGGLE_FIELD_KEYS, 'Toggle');
  const bindPath = requirePath(
    ctx.readField(component.fields, 0),
    'Toggle.bind_path'
  );
  const labelBind = ctx.readField(component.fields, 1);
  const hintBind = ctx.readField(component.fields, 2);
  const size = requireEnum(
    ctx.readField(component.fields, 3),
    TOGGLE_SIZES,
    'Toggle.size'
  );
  const toneRaw = ctx.readField(component.fields, 4);
  const tone = toneRaw === undefined ? 'primary' : requireEnum(toneRaw, TONES, 'Toggle.tone');
  const disabledBind = ctx.readField(component.fields, 5);
  const labelPosition = requireEnum(
    ctx.readField(component.fields, 6),
    TOGGLE_POSITIONS,
    'Toggle.label_position'
  );

  const wrapper = document.createElement('label');
  wrapper.classList.add('tf-toggle');
  wrapper.classList.add(`tf-toggle--size-${size}`);
  wrapper.classList.add(`tf-toggle--tone-${tone}`);
  wrapper.classList.add(`tf-toggle--label-${labelPosition}`);

  const toggle = document.createElement('tf-toggle');

  const labelEl = labelBind != null ? document.createElement('span') : null;
  if (labelEl) {
    labelEl.classList.add('tf-toggle__label');
    applyTextBind(labelEl, labelBind, ctx);
  }
  const hintEl = hintBind != null ? document.createElement('span') : null;
  if (hintEl) {
    hintEl.classList.add('tf-toggle__hint');
    applyTextBind(hintEl, hintBind, ctx);
  }

  if (labelPosition === 'leading' && labelEl) wrapper.appendChild(labelEl);
  wrapper.appendChild(toggle);
  if (labelPosition === 'trailing' && labelEl) wrapper.appendChild(labelEl);
  if (hintEl) wrapper.appendChild(hintEl);

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'Toggle without `label` field requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'Toggle.a11y.label must resolve to non-blank string at initial render'
      );
    }
    const apply = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) {
        toggle.setAttribute('aria-label', v);
      } else {
        toggle.removeAttribute('aria-label');
      }
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, apply));
  }

  const isDisabledFn = applyDisabledReactive(toggle, disabledBind, ctx);

  // Reactive checked state: store -> tf-toggle checked attribute
  const applyChecked = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    toggle.checked = v === true;
  };
  applyChecked();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, applyChecked));

  // tf-toggle emits 'change' with detail.checked (bubbles:true). Intercept
  // and re-emit on wrapper with SDK { value, kind } shape. Propagation is
  // stopped BEFORE the disabled check so the raw component event (with its
  // { checked } detail) never reaches the wrapper, even on a muted control.
  const onChange = (e) => {
    e.stopPropagation();
    if (isDisabledFn()) {
      e.preventDefault();
      return;
    }
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: e.detail.checked, kind: 'bool' },
      })
    );
  };
  toggle.addEventListener('change', onChange);
  ctx.registerCleanup(() => toggle.removeEventListener('change', onChange));
  return wrapper;
}

// =============================================================================
// Checkbox (0x030B)
// =============================================================================

export const CHECKBOX_TAG = 0x030B;
const CHECKBOX_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);

function renderCheckbox(component, ctx) {
  assertOnlyKnownFields(component.fields, CHECKBOX_FIELD_KEYS, 'Checkbox');
  const bindPath = requirePath(
    ctx.readField(component.fields, 0),
    'Checkbox.bind_path'
  );
  const labelBind = ctx.readField(component.fields, 1);
  const hintBind = ctx.readField(component.fields, 2);
  const indeterminateBind = ctx.readField(component.fields, 3);
  const disabledBind = ctx.readField(component.fields, 4);
  const size = requireEnum(
    ctx.readField(component.fields, 5),
    CHECKBOX_SIZES,
    'Checkbox.size'
  );

  const el = document.createElement('tf-checkbox');
  el.classList.add(`tf-checkbox--size-${size}`);

  // Reactive label attr
  if (labelBind != null) {
    const apply = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      const s = v == null ? '' : String(v);
      if (s) el.setAttribute('label', s);
      else el.removeAttribute('label');
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, apply));
  }

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'Checkbox without `label` field requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'Checkbox.a11y.label must resolve to non-blank string at initial render'
      );
    }
    const apply = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) {
        el.setAttribute('aria-label', v);
      } else {
        el.removeAttribute('aria-label');
      }
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, apply));
  }

  const isDisabledFn = applyDisabledReactive(el, disabledBind, ctx);

  // Reactive checked: store -> tf-checkbox.checked
  const applyChecked = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    el.checked = v === true;
  };
  applyChecked();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, applyChecked));

  // Reactive indeterminate
  if (indeterminateBind != null) {
    const apply = () => {
      const v = resolveBindRef(indeterminateBind, ctx.store);
      el.indeterminate = v === true;
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(indeterminateBind, ctx.store, apply));
  }

  // tf-checkbox emits 'change' with detail.checked (bubbles:true). The SDK
  // re-emit is dispatched on the SAME element the listener is attached to, so
  // it MUST carry the `__tfReemit` guard (select-renderer pattern) — without
  // it the listener consumes its own synthetic event and recurses forever.
  // The raw component event is stopImmediatePropagation'd so the dispatcher
  // (registered after us on this element) only ever sees the SDK shape.
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (isDisabledFn()) {
      e.preventDefault();
      return;
    }
    const ce = new CustomEvent('change', {
      bubbles: false,
      detail: { value: e.detail.checked, kind: 'bool' },
    });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  el.addEventListener('change', onChange);
  ctx.registerCleanup(() => el.removeEventListener('change', onChange));
  return el;
}

// =============================================================================
// Radio (0x030C)
// =============================================================================

export const RADIO_TAG = 0x030C;
const RADIO_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderRadio(component, ctx) {
  assertOnlyKnownFields(component.fields, RADIO_FIELD_KEYS, 'Radio');
  const bindPath = requirePath(
    ctx.readField(component.fields, 0),
    'Radio.bind_path'
  );
  const valueRaw = ctx.readField(component.fields, 1);
  if (valueRaw == null) {
    throw new TypeError('Radio.value is required (SelectValue)');
  }
  const parsedValue = parseSelectValue(valueRaw, 'Radio.value');
  const labelBind = ctx.readField(component.fields, 2);
  if (labelBind == null) {
    throw new TypeError('Radio.label is required (BindRef)');
  }
  const hintBind = ctx.readField(component.fields, 3);
  const disabledBind = ctx.readField(component.fields, 4);

  const el = document.createElement('tf-radio');
  el.setAttribute('value', String(parsedValue.value));

  // Reactive label attr
  const applyLabel = () => {
    const v = resolveBindRef(labelBind, ctx.store);
    const s = v == null ? '' : String(v);
    if (s) el.setAttribute('label', s);
    else el.removeAttribute('label');
  };
  applyLabel();
  ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));

  const isDisabledFn = applyDisabledReactive(el, disabledBind, ctx);

  // Reactive checked — tf-radio reads checked state from parent tf-radio-group
  // via closest('tf-radio-group').value. For standalone usage we track a CSS
  // class to enable visual feedback.
  const applyChecked = () => {
    let current;
    try { current = ctx.store.read(bindPath); } catch { current = undefined; }
    const checked = selectValueEquals(parsedValue, current);
    el.classList.toggle('tf-radio--checked', checked);
  };
  applyChecked();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, applyChecked));

  // tf-radio delegates clicks to parent tf-radio-group, which emits 'change'
  // with detail.value. For SDK integration we listen on the radio element
  // itself and dispatch with SDK { value, kind } format.
  const onClick = (e) => {
    if (isDisabledFn()) return;
    e.stopPropagation();
    el.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: parsedValue.value, kind: parsedValue.tag },
      })
    );
  };
  el.addEventListener('click', onClick);
  ctx.registerCleanup(() => el.removeEventListener('click', onClick));
  return el;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormAtomicRenderers() {
  if (!lookupComponentRenderer(TOGGLE_TAG)) {
    registerComponentRenderer(TOGGLE_TAG, renderToggle);
  }
  if (!lookupComponentRenderer(CHECKBOX_TAG)) {
    registerComponentRenderer(CHECKBOX_TAG, renderCheckbox);
  }
  if (!lookupComponentRenderer(RADIO_TAG)) {
    registerComponentRenderer(RADIO_TAG, renderRadio);
  }
}
