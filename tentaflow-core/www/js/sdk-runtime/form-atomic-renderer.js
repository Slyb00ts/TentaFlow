// =============================================================================
// Plik: sdk-runtime/form-atomic-renderer.js
// Opis: Rendererzy atomic form controls — Faza 6 Krok 3.3c-1:
//   - Toggle   (0x030A) — switch on/off z reactive bind_path
//   - Checkbox (0x030B) — checkbox z opcjonalnym indeterminate
//   - Radio    (0x030C) — single radio button z SelectValue
// Wszystkie używają `bind_path: StatePath` (NIE BindRef) — wartość jest
// CZYTANA reaktywnie i klik dispatchuje `change` z proponowaną nową
// wartością (chunk 3.6 wpina write-back przez optimistic patch).
//
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/form/atomic.rs`.
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

/// Parsuje SelectValue (tagged union) zgodnie ze spec'em:
/// `{ kind: "tstr"|"u32"|"i32"|"bool", value }`. Zwraca `{ tag, value }`.
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
      if (!Number.isInteger(sv.value) || sv.value < 0 || sv.value > 0xFFFFFFFF) {
        throw new TypeError(`${ctx}.value must be u32`);
      }
      return { tag: 'u32', value: sv.value };
    }
    case 'i32': {
      if (!Number.isInteger(sv.value) || sv.value < -0x80000000 || sv.value > 0x7FFFFFFF) {
        throw new TypeError(`${ctx}.value must be i32`);
      }
      return { tag: 'i32', value: sv.value };
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

/// Helper: reactive disabled BindRef → sets disabled attr + aria-disabled.
function applyDisabledReactive(element, disabledBind, ctx) {
  if (disabledBind == null) return () => false;
  let isDisabled = false;
  const apply = () => {
    isDisabled = resolveBindRef(disabledBind, ctx.store) === true;
    if (isDisabled) {
      element.setAttribute('disabled', '');
      element.setAttribute('aria-disabled', 'true');
    } else {
      element.removeAttribute('disabled');
      element.removeAttribute('aria-disabled');
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, apply));
  return () => isDisabled;
}

/// Helper: reactive textContent na elemencie z BindRef.
function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
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
  const labelBind = ctx.readField(component.fields, 1);  // Option<BindRef>
  const hintBind = ctx.readField(component.fields, 2);   // Option<BindRef>
  const size = requireEnum(
    ctx.readField(component.fields, 3),
    TOGGLE_SIZES,
    'Toggle.size'
  );
  // §5 0x030A: tone default = primary.
  const toneRaw = ctx.readField(component.fields, 4);
  const tone = toneRaw === undefined ? 'primary' : requireEnum(toneRaw, TONES, 'Toggle.tone');
  const disabledBind = ctx.readField(component.fields, 5);
  const labelPosition = requireEnum(
    ctx.readField(component.fields, 6),
    TOGGLE_POSITIONS,
    'Toggle.label_position'
  );

  // <label> wrap żeby klik na tekst-label przełącza switch (semantic).
  const wrapper = document.createElement('label');
  wrapper.classList.add('tf-toggle');
  wrapper.classList.add(`tf-toggle--size-${size}`);
  wrapper.classList.add(`tf-toggle--tone-${tone}`);
  wrapper.classList.add(`tf-toggle--label-${labelPosition}`);

  const switchEl = document.createElement('button');
  switchEl.setAttribute('type', 'button');
  switchEl.setAttribute('role', 'switch');
  switchEl.classList.add('tf-toggle__switch');
  const knob = document.createElement('span');
  knob.classList.add('tf-toggle__knob');
  knob.setAttribute('aria-hidden', 'true');
  switchEl.appendChild(knob);

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

  // Layout zależy od label_position.
  if (labelPosition === 'leading' && labelEl) wrapper.appendChild(labelEl);
  wrapper.appendChild(switchEl);
  if (labelPosition === 'trailing' && labelEl) wrapper.appendChild(labelEl);
  if (hintEl) wrapper.appendChild(hintEl);

  // Bez label nie ma accessible name dla switch'a — wymóg a11y. Engine
  // `applyAccessibility` ustawi aria-label na ROOT wrapper (<label>), ale
  // to nie wystarczy dla <button role=switch> w środku; trzeba zsynchroni-
  // zować aria-label na switch button explicitly.
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
        switchEl.setAttribute('aria-label', v);
      } else {
        switchEl.removeAttribute('aria-label');
      }
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, apply));
  }

  const isDisabledFn = applyDisabledReactive(switchEl, disabledBind, ctx);

  // Reactive checked state — czytamy boolean ze store'a pod bind_path.
  const applyChecked = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    const checked = v === true;
    switchEl.setAttribute('aria-checked', checked ? 'true' : 'false');
    if (checked) {
      switchEl.classList.add('tf-toggle__switch--on');
    } else {
      switchEl.classList.remove('tf-toggle__switch--on');
    }
  };
  applyChecked();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, applyChecked));

  const onClick = (e) => {
    if (isDisabledFn()) {
      e.preventDefault();
      return;
    }
    e.stopPropagation();
    const current = ctx.store.read(bindPath) === true;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: !current, kind: 'bool' },
      })
    );
  };
  switchEl.addEventListener('click', onClick);
  ctx.registerCleanup(() => switchEl.removeEventListener('click', onClick));
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

  const wrapper = document.createElement('label');
  wrapper.classList.add('tf-checkbox');
  wrapper.classList.add(`tf-checkbox--size-${size}`);

  const box = document.createElement('input');
  box.setAttribute('type', 'checkbox');
  box.classList.add('tf-checkbox__input');

  const labelEl = labelBind != null ? document.createElement('span') : null;
  if (labelEl) {
    labelEl.classList.add('tf-checkbox__label');
    applyTextBind(labelEl, labelBind, ctx);
  }
  const hintEl = hintBind != null ? document.createElement('span') : null;
  if (hintEl) {
    hintEl.classList.add('tf-checkbox__hint');
    applyTextBind(hintEl, hintBind, ctx);
  }

  wrapper.appendChild(box);
  if (labelEl) wrapper.appendChild(labelEl);
  if (hintEl) wrapper.appendChild(hintEl);

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
        box.setAttribute('aria-label', v);
      } else {
        box.removeAttribute('aria-label');
      }
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, apply));
  }

  const isDisabledFn = applyDisabledReactive(box, disabledBind, ctx);

  // Reactive checked.
  const applyChecked = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    box.checked = v === true;
  };
  applyChecked();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, applyChecked));

  // Reactive indeterminate (Option<BindRef>).
  if (indeterminateBind != null) {
    const apply = () => {
      const v = resolveBindRef(indeterminateBind, ctx.store);
      box.indeterminate = v === true;
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(indeterminateBind, ctx.store, apply));
  }

  const onChange = (e) => {
    if (isDisabledFn()) {
      e.preventDefault();
      return;
    }
    e.stopPropagation();
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: box.checked, kind: 'bool' },
      })
    );
  };
  box.addEventListener('change', onChange);
  ctx.registerCleanup(() => box.removeEventListener('change', onChange));
  return wrapper;
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

  const wrapper = document.createElement('label');
  wrapper.classList.add('tf-radio');

  const radio = document.createElement('input');
  radio.setAttribute('type', 'radio');
  radio.classList.add('tf-radio__input');
  // `name` ustawimy z bind_path tak, żeby grupa radio'sów na tym samym
  // bind_path miała eksklusywne zachowanie HTML natywne. Path
  // serializujemy jako string deterministyczny.
  radio.setAttribute('name', `tf-radio-${pathToName(bindPath)}`);

  const labelEl = document.createElement('span');
  labelEl.classList.add('tf-radio__label');
  applyTextBind(labelEl, labelBind, ctx);

  let hintEl = null;
  if (hintBind != null) {
    hintEl = document.createElement('span');
    hintEl.classList.add('tf-radio__hint');
    applyTextBind(hintEl, hintBind, ctx);
  }

  wrapper.appendChild(radio);
  wrapper.appendChild(labelEl);
  if (hintEl) wrapper.appendChild(hintEl);

  const isDisabledFn = applyDisabledReactive(radio, disabledBind, ctx);

  // Reactive checked — porównujemy store value do SelectValue.
  const applyChecked = () => {
    let current;
    try { current = ctx.store.read(bindPath); } catch { current = undefined; }
    radio.checked = selectValueEquals(parsedValue, current);
  };
  applyChecked();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, applyChecked));

  const onChange = (e) => {
    if (isDisabledFn()) {
      e.preventDefault();
      return;
    }
    e.stopPropagation();
    if (radio.checked) {
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('change', {
          bubbles: false,
          detail: { value: parsedValue.value, kind: parsedValue.tag },
        })
      );
    }
  };
  radio.addEventListener('change', onChange);
  ctx.registerCleanup(() => radio.removeEventListener('change', onChange));
  return wrapper;
}

/// Deterministyczna, collision-free serializacja StatePath do `name=...`
/// HTML attribute. Konwencja niewidoczna na wire — używana tylko do
/// grupowania DOM radio. Bazujemy na `JSON.stringify([[kind,value]...])`
/// → UTF-8 → base64 (URL-safe, bez padding) — JSON wprowadza explicite
/// separator między kind a value oraz między segmentami, więc kolizja
/// `'a__i_1'` (key) ↔ `[key('a'), index(1)]` jest niemożliwa.
function pathToName(path) {
  // base64-bez-padding z JSON-serializacji daje url-safe deterministyczne
  // ID bez separator-collision'a. Wartości path są ograniczone (key
  // strings + u32), więc length jest sensowny.
  const json = JSON.stringify(path.map((s) => [s.kind, s.value]));
  // btoa wymaga 8-bit binary; URI-encode dla bezpiecznych unicode.
  const utf8 = unescape(encodeURIComponent(json));
  // eslint-disable-next-line no-undef
  const b64 = (typeof btoa === 'function' ? btoa : (s) => Buffer.from(s, 'binary').toString('base64'))(utf8);
  // Strip padding + URL-unsafe chars dla name= compat.
  return b64.replace(/=+$/, '').replace(/\+/g, '-').replace(/\//g, '_');
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
