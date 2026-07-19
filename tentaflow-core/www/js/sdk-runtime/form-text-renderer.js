// =============================================================================
// Plik: sdk-runtime/form-text-renderer.js
// Opis: Rendererzy text-input form controls — Input (0x0301) i Textarea (0x0302)
// uzywajace tf-input i tf-textarea web components. Bind_path: StatePath
// (read-only reactive) — write-back przez host dispatch (chunk 3.6).
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/form/inputs.rs
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

// =============================================================================
// Walidatory enumow i tokenow
// =============================================================================

const INPUT_TYPES = new Set([
  'text', 'email', 'password', 'url', 'phone', 'number', 'search',
]);
const INPUT_TYPE_TO_HTML = Object.freeze({
  text: 'text',
  email: 'email',
  password: 'password',
  url: 'url',
  phone: 'tel',
  number: 'number',
  search: 'search',
});
const AUTOCOMPLETE_HINTS = new Set([
  'off', 'on', 'name', 'email', 'username',
  'current_password', 'new_password', 'one_time_code',
  'tel', 'url', 'street_address', 'postal_code',
]);
const AUTOCOMPLETE_TO_HTML = Object.freeze({
  off: 'off',
  on: 'on',
  name: 'name',
  email: 'email',
  username: 'username',
  current_password: 'current-password',
  new_password: 'new-password',
  one_time_code: 'one-time-code',
  tel: 'tel',
  url: 'url',
  street_address: 'street-address',
  postal_code: 'postal-code',
});
const INPUT_MODES = new Set([
  'none', 'text', 'tel', 'url', 'email', 'numeric', 'decimal', 'search',
]);
const INPUT_SIZES = new Set(['sm', 'md', 'lg']);
const INPUT_VARIANTS = new Set(['outlined', 'ghost']);
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
function requirePath(v, ctx) {
  if (!Array.isArray(v)) {
    throw new TypeError(`${ctx}: expected StatePath (Array<PathSegment>)`);
  }
  return v;
}
function requireU16(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFn) {
      throw new TypeError(`${ctx}: expected u16, got ${v}`);
    }
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFF) {
    throw new TypeError(`${ctx}: expected u16, got ${v}`);
  }
  return v;
}
function requireU8(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFn) {
      throw new TypeError(`${ctx}: expected u8, got ${v}`);
    }
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) {
    throw new TypeError(`${ctx}: expected u8, got ${v}`);
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

// =============================================================================
// Reactive helpers
// =============================================================================

function applyAttrReactive(el, attrName, bindRef, ctx) {
  if (bindRef == null) return;
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    const s = v == null ? '' : String(v);
    if (s) el.setAttribute(attrName, s);
    else el.removeAttribute(attrName);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

function applyBoolAttrReactive(el, bindRef, ctx, attrName) {
  if (bindRef == null) return () => false;
  let active = false;
  const apply = () => {
    active = resolveBindRef(bindRef, ctx.store) === true;
    if (active) el.setAttribute(attrName, '');
    else el.removeAttribute(attrName);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
  return () => active;
}

function applyValueReactive(el, bindPath, ctx) {
  const apply = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    const next = v == null ? '' : String(v);
    if (el.value !== next) el.value = next;
  };
  apply();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, apply));
}

function applyAriaLabelFromA11y(el, component, ctx) {
  if (component.a11y == null || component.a11y.label == null) {
    throw new TypeError(
      `${el.tagName} without \`label\` field requires Component.a11y.label for accessible name`
    );
  }
  const initial = resolveBindRef(component.a11y.label, ctx.store);
  if (typeof initial !== 'string' || initial.trim().length === 0) {
    throw new TypeError(
      `${el.tagName}.a11y.label must resolve to non-blank string at initial render`
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

// =============================================================================
// Input (0x0301)
// =============================================================================

export const INPUT_TAG = 0x0301;
const INPUT_FIELD_KEYS = new Set([
  0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19,
]);

function renderInput(component, ctx) {
  assertOnlyKnownFields(component.fields, INPUT_FIELD_KEYS, 'Input');

  const type = requireEnum(
    ctx.readField(component.fields, 0), INPUT_TYPES, 'Input.type'
  );
  const bindPath = requirePath(
    ctx.readField(component.fields, 1), 'Input.bind_path'
  );
  const placeholderBind = ctx.readField(component.fields, 2);
  const labelBind = ctx.readField(component.fields, 3);
  const hintBind = ctx.readField(component.fields, 4);
  const leadingIconRaw = ctx.readField(component.fields, 5);
  const trailingIconRaw = ctx.readField(component.fields, 6);
  const prefixBind = ctx.readField(component.fields, 7);
  const suffixBind = ctx.readField(component.fields, 8);
  const validatorsRaw = ctx.readField(component.fields, 9);
  if (!Array.isArray(validatorsRaw)) {
    throw new TypeError('Input.validators: expected Array<ValidationRule>');
  }
  const hasRequired = validatorsRaw.some(
    (v) => v && typeof v === 'object' && v.kind === 'required'
  );
  const maxLengthRaw = ctx.readField(component.fields, 10);
  const minLengthRaw = ctx.readField(component.fields, 11);
  const patternRaw = ctx.readField(component.fields, 12);
  const autocompleteRaw = ctx.readField(component.fields, 13);
  const inputModeRaw = ctx.readField(component.fields, 14);
  const disabledBind = ctx.readField(component.fields, 15);
  const readonlyBind = ctx.readField(component.fields, 16);
  const errorBind = ctx.readField(component.fields, 17);
  const size = requireEnum(
    ctx.readField(component.fields, 18), INPUT_SIZES, 'Input.size'
  );
  const variantRaw = ctx.readField(component.fields, 19);
  const variant = variantRaw == null
    ? 'outlined'
    : requireEnum(variantRaw, INPUT_VARIANTS, 'Input.variant');

  const maxLength = maxLengthRaw == null ? null : requireU16(maxLengthRaw, 'Input.max_length');
  const minLength = minLengthRaw == null ? null : requireU16(minLengthRaw, 'Input.min_length');
  const pattern = patternRaw == null ? null : (() => {
    if (typeof patternRaw !== 'string') {
      throw new TypeError('Input.pattern: expected string');
    }
    return patternRaw;
  })();
  const autocomplete = autocompleteRaw == null
    ? null : requireEnum(autocompleteRaw, AUTOCOMPLETE_HINTS, 'Input.autocomplete');
  const inputMode = inputModeRaw == null
    ? null : requireEnum(inputModeRaw, INPUT_MODES, 'Input.input_mode');

  const el = document.createElement('tf-input');
  el.classList.add(`tf-input--size-${size}`);
  el.classList.add(`tf-input--type-${type}`);
  el.classList.add(`tf-input--variant-${variant}`);
  el.setAttribute('type', INPUT_TYPE_TO_HTML[type]);

  if (maxLength != null) el.setAttribute('maxlength', String(maxLength));
  if (minLength != null) el.setAttribute('minlength', String(minLength));
  if (pattern != null) el.setAttribute('pattern', pattern);
  if (autocomplete != null) el.setAttribute('autocomplete', AUTOCOMPLETE_TO_HTML[autocomplete]);
  if (inputMode != null) el.setAttribute('inputmode', inputMode);
  if (hasRequired) el.setAttribute('required', '');

  // tf-input icon support. Named IconRef → component attribute (leading via
  // "icon", trailing via "trailing-icon"). Asset/non-named IconRefs have no
  // attribute path, so they are not surfaced here (parity with action-link-fab
  // where only named icons map to the tf-button icon attribute).
  if (leadingIconRaw != null && typeof leadingIconRaw === 'object' && leadingIconRaw.name) {
    el.setAttribute('icon', leadingIconRaw.name);
  }
  if (trailingIconRaw != null && typeof trailingIconRaw === 'object' && trailingIconRaw.name) {
    el.setAttribute('trailing-icon', trailingIconRaw.name);
  }

  applyAttrReactive(el, 'label', labelBind, ctx);
  applyAttrReactive(el, 'placeholder', placeholderBind, ctx);
  applyAttrReactive(el, 'hint', hintBind, ctx);
  applyAttrReactive(el, 'prefix', prefixBind, ctx);
  applyAttrReactive(el, 'suffix', suffixBind, ctx);

  if (errorBind != null) {
    const applyError = () => {
      const v = resolveBindRef(errorBind, ctx.store);
      const text = typeof v === 'string' && v.length > 0 ? v : null;
      if (text) el.setAttribute('error', text);
      else el.removeAttribute('error');
    };
    applyError();
    ctx.registerCleanup(subscribeBindRef(errorBind, ctx.store, applyError));
  }

  const isDisabledFn = applyBoolAttrReactive(el, disabledBind, ctx, 'disabled');

  const isReadonlyFn = (() => {
    if (readonlyBind == null) return () => false;
    let active = false;
    const apply = () => {
      active = resolveBindRef(readonlyBind, ctx.store) === true;
      el.toggleAttribute('readonly', active);
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(readonlyBind, ctx.store, apply));
    return () => active;
  })();

  if (labelBind == null) applyAriaLabelFromA11y(el, component, ctx);

  applyValueReactive(el, bindPath, ctx);

  // tf-input emits native input/change with detail.value (bubbles:true), and
  // the inner native control's own input/change bubble to the host too. The
  // engine's applyEventHandlers listens on the host for these. We intercept to
  // add the SDK `kind` field and suppress disabled/readonly controls — but the
  // raw events carry no `{value, kind}` (a native InputEvent.detail is the
  // number 0), so if they reached the dispatcher they'd send an EMPTY value and
  // clobber the field. We therefore stopImmediatePropagation on every raw event
  // (blocking the dispatcher listener registered after us) and re-emit a single
  // synthetic event tagged `__tfReemit` that we let pass straight through.
  const muted = () => isDisabledFn() || isReadonlyFn();

  const reemit = (name) => {
    const ce = new CustomEvent(name, {
      bubbles: false,
      detail: { value: el.value, kind: 'tstr' },
    });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };

  // Both the component CustomEvent ({ value }) and the inner native control's
  // own bubbled event reach the host per action. Every raw event is blocked,
  // but only the component one (string detail.value) re-emits — otherwise the
  // dispatcher would receive two SDK events per keystroke.
  const onInput = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (muted()) return;
    if (!e.detail || typeof e.detail.value !== 'string') return;
    reemit('input');
  };
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (muted()) return;
    if (!e.detail || typeof e.detail.value !== 'string') return;
    reemit('change');
  };
  const onKeyDown = (e) => {
    if (e.key !== 'Enter') return;
    if (e.shiftKey || e.altKey || e.ctrlKey || e.metaKey) return;
    e.preventDefault();
    if (muted()) return;
    reemit('submit');
  };
  // Native focus/blur don't bubble, so the host listens to focusin/focusout
  // (which do) and re-emits the SDK focus/blur names the dispatcher expects.
  // Re-emitting under a DIFFERENT name than the raw event makes recursion
  // impossible; the `__tfReemit` tag keeps the renderer convention uniform.
  const reemitFocusEdge = (name) => {
    if (muted()) return;
    const ce = new CustomEvent(name, { bubbles: false, detail: null });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  const onFocusIn = () => reemitFocusEdge('focus');
  const onFocusOut = () => reemitFocusEdge('blur');
  el.addEventListener('input', onInput);
  el.addEventListener('change', onChange);
  el.addEventListener('keydown', onKeyDown);
  el.addEventListener('focusin', onFocusIn);
  el.addEventListener('focusout', onFocusOut);
  ctx.registerCleanup(() => {
    el.removeEventListener('input', onInput);
    el.removeEventListener('change', onChange);
    el.removeEventListener('keydown', onKeyDown);
    el.removeEventListener('focusin', onFocusIn);
    el.removeEventListener('focusout', onFocusOut);
  });

  return el;
}

// =============================================================================
// Textarea (0x0302)
// =============================================================================

export const TEXTAREA_TAG = 0x0302;
const TEXTAREA_FIELD_KEYS = new Set([
  0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
]);

function renderTextarea(component, ctx) {
  assertOnlyKnownFields(component.fields, TEXTAREA_FIELD_KEYS, 'Textarea');

  const bindPath = requirePath(
    ctx.readField(component.fields, 0), 'Textarea.bind_path'
  );
  const placeholderBind = ctx.readField(component.fields, 1);
  const labelBind = ctx.readField(component.fields, 2);
  const hintBind = ctx.readField(component.fields, 3);
  const validatorsRaw = ctx.readField(component.fields, 4);
  if (!Array.isArray(validatorsRaw)) {
    throw new TypeError('Textarea.validators: expected Array<ValidationRule>');
  }
  const maxLengthRaw = ctx.readField(component.fields, 5);
  const minLengthRaw = ctx.readField(component.fields, 6);
  const disabledBind = ctx.readField(component.fields, 7);
  const readonlyBind = ctx.readField(component.fields, 8);
  const errorBind = ctx.readField(component.fields, 9);
  const size = requireEnum(
    ctx.readField(component.fields, 10), INPUT_SIZES, 'Textarea.size'
  );
  const rowsRaw = ctx.readField(component.fields, 11);
  const rows = rowsRaw === undefined ? 3 : requireU8(rowsRaw, 'Textarea.rows');
  if (rows === 0) throw new TypeError('Textarea.rows must be >= 1');
  const autoresize = requireBool(
    ctx.readField(component.fields, 12), 'Textarea.autoresize'
  );
  const maxRowsRaw = ctx.readField(component.fields, 13);
  const maxRows = maxRowsRaw == null ? null : requireU8(maxRowsRaw, 'Textarea.max_rows');
  if (maxRows != null && maxRows < rows) {
    throw new TypeError('Textarea.max_rows must be >= rows');
  }
  const monospace = requireBool(
    ctx.readField(component.fields, 14), 'Textarea.monospace'
  );
  const variantRaw = ctx.readField(component.fields, 15);
  const variant = variantRaw == null
    ? 'outlined'
    : requireEnum(variantRaw, INPUT_VARIANTS, 'Textarea.variant');
  const maxLength = maxLengthRaw == null ? null : requireU16(maxLengthRaw, 'Textarea.max_length');
  const minLength = minLengthRaw == null ? null : requireU16(minLengthRaw, 'Textarea.min_length');

  const el = document.createElement('tf-textarea');
  el.classList.add(`tf-textarea--size-${size}`);
  el.classList.add(`tf-textarea--variant-${variant}`);
  if (monospace) el.classList.add('tf-textarea--monospace');

  el.setAttribute('rows', String(rows));
  if (autoresize) el.setAttribute('autogrow', '');
  if (maxLength != null) el.setAttribute('maxlength', String(maxLength));

  applyAttrReactive(el, 'label', labelBind, ctx);
  applyAttrReactive(el, 'placeholder', placeholderBind, ctx);
  applyAttrReactive(el, 'hint', hintBind, ctx);

  if (errorBind != null) {
    const applyError = () => {
      const v = resolveBindRef(errorBind, ctx.store);
      const text = typeof v === 'string' && v.length > 0 ? v : null;
      if (text) el.setAttribute('error', text);
      else el.removeAttribute('error');
    };
    applyError();
    ctx.registerCleanup(subscribeBindRef(errorBind, ctx.store, applyError));
  }

  const isDisabledFn = applyBoolAttrReactive(el, disabledBind, ctx, 'disabled');

  const isReadonlyFn = (() => {
    if (readonlyBind == null) return () => false;
    let active = false;
    const apply = () => {
      active = resolveBindRef(readonlyBind, ctx.store) === true;
      el.toggleAttribute('readonly', active);
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(readonlyBind, ctx.store, apply));
    return () => active;
  })();

  if (labelBind == null) applyAriaLabelFromA11y(el, component, ctx);

  applyValueReactive(el, bindPath, ctx);

  // tf-textarea emits native input/change with detail.value (bubbles:true), and
  // the inner native control's events bubble to the host too. Raw events carry
  // no {value, kind}, so we stopImmediatePropagation them (blocking the empty
  // emit) and re-emit a single synthetic tagged event the dispatcher consumes.
  const muted = () => isDisabledFn() || isReadonlyFn();

  const reemit = (name) => {
    const ce = new CustomEvent(name, {
      bubbles: false,
      detail: { value: el.value, kind: 'tstr' },
    });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };

  // Same dedupe as Input: only the component CustomEvent (string detail.value)
  // re-emits; the inner native control's bubbled event is blocked silently.
  const onInput = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (muted()) return;
    if (!e.detail || typeof e.detail.value !== 'string') return;
    reemit('input');
  };
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (muted()) return;
    if (!e.detail || typeof e.detail.value !== 'string') return;
    reemit('change');
  };
  // focusin/focusout bubble (native focus/blur don't); re-emitting under a
  // different name than the raw event makes recursion impossible.
  const reemitFocusEdge = (name) => {
    if (muted()) return;
    const ce = new CustomEvent(name, { bubbles: false, detail: null });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  const onFocusIn = () => reemitFocusEdge('focus');
  const onFocusOut = () => reemitFocusEdge('blur');
  el.addEventListener('input', onInput);
  el.addEventListener('change', onChange);
  el.addEventListener('focusin', onFocusIn);
  el.addEventListener('focusout', onFocusOut);
  ctx.registerCleanup(() => {
    el.removeEventListener('input', onInput);
    el.removeEventListener('change', onChange);
    el.removeEventListener('focusin', onFocusIn);
    el.removeEventListener('focusout', onFocusOut);
  });

  return el;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormTextRenderers() {
  if (!lookupComponentRenderer(INPUT_TAG)) {
    registerComponentRenderer(INPUT_TAG, renderInput);
  }
  if (!lookupComponentRenderer(TEXTAREA_TAG)) {
    registerComponentRenderer(TEXTAREA_TAG, renderTextarea);
  }
}
