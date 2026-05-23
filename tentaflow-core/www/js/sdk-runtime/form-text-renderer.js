// =============================================================================
// Plik: sdk-runtime/form-text-renderer.js
// Opis: Rendererzy text-input form controls — Faza 6 Krok 3.3c-2:
//   - Input    (0x0301) — single-line text input z reactive bind_path,
//                          leading/trailing icons, prefix/suffix, error,
//                          disabled/readonly, label/hint, validators
//   - Textarea (0x0302) — multi-line text input z rows/autoresize/monospace
// Oba używają `bind_path: StatePath` (NIE BindRef) — wartość jest CZYTANA
// reaktywnie i każde naciśnięcie klawisza dispatchuje `input`, blur/Enter
// dispatchuje `change` z proponowaną nową wartością. Write-back przez
// optimistic patch jest doczepiany przez engine z chunka 3.6.
//
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/form/inputs.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';

// =============================================================================
// Wspólne walidatory enumów i tokenów
// =============================================================================

const INPUT_TYPES = new Set([
  'text', 'email', 'password', 'url', 'phone', 'number', 'search',
]);
// Mapowanie spec'owego InputType na natywny atrybut HTML <input type="...">.
// Spec używa "phone" (semantic token), HTML wymaga "tel".
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
// HTML autocomplete spec używa kebab-case dla compound tokenów.
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
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFF) {
    throw new TypeError(`${ctx}: expected u16, got ${v}`);
  }
  return v;
}
function requireU8(v, ctx) {
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
function assertOnlyKnownObjectKeys(obj, allowedKeys, ctx) {
  for (const k of Object.keys(obj)) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${ctx}: unexpected key '${k}'`);
    }
  }
}

// =============================================================================
// IconRef parser (catalog §1.5)
// =============================================================================

// IconRef parsing + render delegowany do shared `renderIcon` (icon-renderer.js)
// — wspiera oba warianty IconRef::Named / IconRef::Asset zgodnie z inline.rs §1.5.

// =============================================================================
// Reactive helpers
// =============================================================================

/// Reactive disabled/readonly attr binding. Zwraca getter aktualnej wartości
/// (dla on-click guard'ów). Cleanup rejestrowany w ctx.
function applyBoolAttrReactive(element, bindRef, ctx, attrName, opts = {}) {
  if (bindRef == null) return () => false;
  let active = false;
  const apply = () => {
    active = resolveBindRef(bindRef, ctx.store) === true;
    if (active) {
      element.setAttribute(attrName, '');
      if (opts.ariaName) element.setAttribute(opts.ariaName, 'true');
    } else {
      element.removeAttribute(attrName);
      if (opts.ariaName) element.removeAttribute(opts.ariaName);
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
  return () => active;
}

/// Reactive textContent na elemencie z BindRef.
function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

/// Reactive placeholder attr — placeholder przyjmuje BindRef bo może być
/// `t("placeholder.search")` (chunki 3.6+ wbiją resolver i18n; tutaj tylko
/// czytamy z BindRef jak normalna wartość).
function applyPlaceholderReactive(input, bindRef, ctx) {
  if (bindRef == null) return;
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    if (v == null || v === '') input.removeAttribute('placeholder');
    else input.setAttribute('placeholder', String(v));
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

/// Reactive error message — gdy resolved value nie-pusta, dodaje
/// `aria-invalid=true` + render `.tf-input__error` z tekstem. Gdy null/pusto
/// — usuwa node i invalid flag. Returns fn returning current error text.
function applyErrorReactive(wrapper, control, bindRef, ctx) {
  if (bindRef == null) return () => null;
  let errorEl = null;
  let currentText = null;
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    const text = typeof v === 'string' && v.length > 0 ? v : null;
    currentText = text;
    if (text == null) {
      control.removeAttribute('aria-invalid');
      wrapper.classList.remove('tf-input--invalid');
      if (errorEl != null) {
        errorEl.remove();
        errorEl = null;
      }
    } else {
      control.setAttribute('aria-invalid', 'true');
      wrapper.classList.add('tf-input--invalid');
      if (errorEl == null) {
        errorEl = document.createElement('span');
        errorEl.classList.add('tf-input__error');
        errorEl.setAttribute('role', 'alert');
        wrapper.appendChild(errorEl);
      }
      errorEl.textContent = text;
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
  return () => currentText;
}

/// Reactive value sync: store → input.value. Nie zapisuje z powrotem do
/// store'a (write-back po stronie host'a / chunk 3.6).
function applyValueReactive(input, bindPath, ctx) {
  const apply = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    // Zachowujemy "" dla undefined/null aby usunąć stale value bez zmiany
    // selection caret w trakcie typing'u (jeśli store push'uje obcą wartość
    // gdy input ma focus, value zostanie nadpisane — taki design, host jest
    // source of truth).
    const next = v == null ? '' : String(v);
    if (input.value !== next) input.value = next;
  };
  apply();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, apply));
}

// =============================================================================
// Input (0x0301)
// =============================================================================

export const INPUT_TAG = 0x0301;
// Field keys per spec (form/inputs.rs Input):
// 0=type, 1=bind_path, 2=placeholder, 3=label, 4=hint, 5=leading_icon,
// 6=trailing_icon, 7=prefix, 8=suffix, 9=validators, 10=max_length,
// 11=min_length, 12=pattern, 13=autocomplete, 14=input_mode, 15=disabled,
// 16=readonly, 17=error, 18=size.
const INPUT_FIELD_KEYS = new Set([
  0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18,
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
  // validators — szczegółowa walidacja po stronie host'a (Faza 6 trust
  // model §13.4). Tu używamy tylko do reflect'owania required→aria.
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-input');
  wrapper.classList.add(`tf-input--size-${size}`);
  wrapper.classList.add(`tf-input--type-${type}`);

  // Label nad input'em — jeśli BindRef set. Jeśli brak, a11y label
  // wymagany (jak w Toggle/Checkbox).
  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-input__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const fieldRow = document.createElement('div');
  fieldRow.classList.add('tf-input__field');

  // Prefix (BindRef text przed input'em).
  if (prefixBind != null) {
    const prefixEl = document.createElement('span');
    prefixEl.classList.add('tf-input__affix');
    prefixEl.classList.add('tf-input__affix--prefix');
    applyTextBind(prefixEl, prefixBind, ctx);
    fieldRow.appendChild(prefixEl);
  }

  if (leadingIconRaw != null) {
    const iconEl = renderIcon(leadingIconRaw, 'Input.leading_icon');
    iconEl.classList.add('tf-input__icon');
    iconEl.classList.add('tf-input__icon--leading');
    fieldRow.appendChild(iconEl);
  }

  const input = document.createElement('input');
  input.classList.add('tf-input__control');
  input.setAttribute('type', INPUT_TYPE_TO_HTML[type]);

  // Stable id pozwala <label for=...>; używamy component.id który jest
  // wymagany i unikalny w obrębie panelu.
  const inputDomId = `tf-input-${component.id}`;
  input.setAttribute('id', inputDomId);
  if (labelEl) labelEl.setAttribute('for', inputDomId);

  if (maxLength != null) input.setAttribute('maxlength', String(maxLength));
  if (minLength != null) input.setAttribute('minlength', String(minLength));
  if (pattern != null) input.setAttribute('pattern', pattern);
  if (autocomplete != null) {
    input.setAttribute('autocomplete', AUTOCOMPLETE_TO_HTML[autocomplete]);
  }
  if (inputMode != null) input.setAttribute('inputmode', inputMode);
  if (hasRequired) {
    input.setAttribute('required', '');
    input.setAttribute('aria-required', 'true');
  }

  if (labelBind == null) {
    // Brak <label> wymaga aria-label z Component.a11y i mirror'a na
    // wewnętrzny <input> (engine ustawia aria-label tylko na wrapperze
    // — tu wymagamy accessible name na faktycznym form control).
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'Input without `label` field requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'Input.a11y.label must resolve to non-blank string at initial render'
      );
    }
    const applyAriaLabel = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) {
        input.setAttribute('aria-label', v);
      } else {
        input.removeAttribute('aria-label');
      }
    };
    applyAriaLabel();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAriaLabel));
  }

  applyPlaceholderReactive(input, placeholderBind, ctx);
  const isDisabledFn = applyBoolAttrReactive(
    input, disabledBind, ctx, 'disabled', { ariaName: 'aria-disabled' }
  );
  const isReadonlyFn = applyBoolAttrReactive(
    input, readonlyBind, ctx, 'readonly', { ariaName: 'aria-readonly' }
  );

  fieldRow.appendChild(input);

  if (trailingIconRaw != null) {
    const iconEl = renderIcon(trailingIconRaw, 'Input.trailing_icon');
    iconEl.classList.add('tf-input__icon');
    iconEl.classList.add('tf-input__icon--trailing');
    fieldRow.appendChild(iconEl);
  }
  if (suffixBind != null) {
    const suffixEl = document.createElement('span');
    suffixEl.classList.add('tf-input__affix');
    suffixEl.classList.add('tf-input__affix--suffix');
    applyTextBind(suffixEl, suffixBind, ctx);
    fieldRow.appendChild(suffixEl);
  }

  wrapper.appendChild(fieldRow);

  if (hintBind != null) {
    const hintEl = document.createElement('span');
    hintEl.classList.add('tf-input__hint');
    applyTextBind(hintEl, hintBind, ctx);
    wrapper.appendChild(hintEl);
  }

  // Reactive value sync + reactive error (appendChild po hint by error
  // wisiał najniżej w kolumnie).
  applyValueReactive(input, bindPath, ctx);
  applyErrorReactive(wrapper, input, errorBind, ctx);

  // Event wiring — input/change/submit/focus/blur (spec form/inputs.rs).
  // Disabled lub readonly tłumi WSZYSTKIE eventy renderera; natywny
  // `disabled` blokuje też focus w real browser, ale programatyczne
  // listenery przeszłyby do dispatchera bez tego guard'a.
  const muted = () => isDisabledFn() || isReadonlyFn();
  const onInput = () => {
    if (muted()) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('input', {
        bubbles: false,
        detail: { value: input.value, kind: 'tstr' },
      })
    );
  };
  const onChange = () => {
    if (muted()) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: input.value, kind: 'tstr' },
      })
    );
  };
  // Submit dla single-line Input: Enter bez Shift/Alt/Ctrl/Meta. Natywne
  // `keydown` blokujemy preventDefault'em ZAWSZE (także przy disabled/
  // readonly), by formularz nadrzędny nie submittował się bez kontroli
  // host'a (Faza 6 dispatch przez handlers). Dispatch rendererowego
  // `submit` jest pomijany dla muted control.
  const onKeyDown = (e) => {
    if (e.key !== 'Enter') return;
    if (e.shiftKey || e.altKey || e.ctrlKey || e.metaKey) return;
    e.preventDefault();
    if (muted()) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('submit', {
        bubbles: false,
        detail: { value: input.value, kind: 'tstr' },
      })
    );
  };
  const onFocus = () => {
    if (muted()) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('focus', {
        bubbles: false,
        detail: null,
      })
    );
  };
  const onBlur = () => {
    if (muted()) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('blur', {
        bubbles: false,
        detail: null,
      })
    );
  };
  input.addEventListener('input', onInput);
  input.addEventListener('change', onChange);
  input.addEventListener('keydown', onKeyDown);
  input.addEventListener('focus', onFocus);
  input.addEventListener('blur', onBlur);
  ctx.registerCleanup(() => {
    input.removeEventListener('input', onInput);
    input.removeEventListener('change', onChange);
    input.removeEventListener('keydown', onKeyDown);
    input.removeEventListener('focus', onFocus);
    input.removeEventListener('blur', onBlur);
  });

  return wrapper;
}

// =============================================================================
// Textarea (0x0302)
// =============================================================================

export const TEXTAREA_TAG = 0x0302;
// Field keys per spec (form/inputs.rs Textarea):
// 0=bind_path, 1=placeholder, 2=label, 3=hint, 4=validators, 5=max_length,
// 6=min_length, 7=disabled, 8=readonly, 9=error, 10=size, 11=rows,
// 12=autoresize, 13=max_rows, 14=monospace.
const TEXTAREA_FIELD_KEYS = new Set([
  0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14,
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
  const hasRequired = validatorsRaw.some(
    (v) => v && typeof v === 'object' && v.kind === 'required'
  );
  const maxLengthRaw = ctx.readField(component.fields, 5);
  const minLengthRaw = ctx.readField(component.fields, 6);
  const disabledBind = ctx.readField(component.fields, 7);
  const readonlyBind = ctx.readField(component.fields, 8);
  const errorBind = ctx.readField(component.fields, 9);
  const size = requireEnum(
    ctx.readField(component.fields, 10), INPUT_SIZES, 'Textarea.size'
  );
  // §5 0x0302 default: rows = 3.
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
  const maxLength = maxLengthRaw == null ? null : requireU16(maxLengthRaw, 'Textarea.max_length');
  const minLength = minLengthRaw == null ? null : requireU16(minLengthRaw, 'Textarea.min_length');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-textarea');
  wrapper.classList.add(`tf-textarea--size-${size}`);
  if (monospace) wrapper.classList.add('tf-textarea--monospace');
  if (autoresize) wrapper.classList.add('tf-textarea--autoresize');

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-textarea__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const textarea = document.createElement('textarea');
  textarea.classList.add('tf-textarea__control');
  textarea.setAttribute('rows', String(rows));
  const taDomId = `tf-textarea-${component.id}`;
  textarea.setAttribute('id', taDomId);
  if (labelEl) labelEl.setAttribute('for', taDomId);
  if (maxLength != null) textarea.setAttribute('maxlength', String(maxLength));
  if (minLength != null) textarea.setAttribute('minlength', String(minLength));
  if (hasRequired) {
    textarea.setAttribute('required', '');
    textarea.setAttribute('aria-required', 'true');
  }

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'Textarea without `label` field requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'Textarea.a11y.label must resolve to non-blank string at initial render'
      );
    }
    const applyAriaLabel = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) {
        textarea.setAttribute('aria-label', v);
      } else {
        textarea.removeAttribute('aria-label');
      }
    };
    applyAriaLabel();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAriaLabel));
  }

  applyPlaceholderReactive(textarea, placeholderBind, ctx);
  const isDisabledFn = applyBoolAttrReactive(
    textarea, disabledBind, ctx, 'disabled', { ariaName: 'aria-disabled' }
  );
  const isReadonlyFn = applyBoolAttrReactive(
    textarea, readonlyBind, ctx, 'readonly', { ariaName: 'aria-readonly' }
  );

  wrapper.appendChild(textarea);

  if (hintBind != null) {
    const hintEl = document.createElement('span');
    hintEl.classList.add('tf-textarea__hint');
    applyTextBind(hintEl, hintBind, ctx);
    wrapper.appendChild(hintEl);
  }

  applyValueReactive(textarea, bindPath, ctx);
  applyErrorReactive(wrapper, textarea, errorBind, ctx);

  // Autoresize: po każdym input'cie zmieniaj height na scrollHeight,
  // clamp do max_rows (jeśli set). Bazujemy na lineHeight obliczonym z
  // computed style. W test'ach z happy-dom layout jest pseudo, więc
  // chronimy się przed NaN.
  if (autoresize) {
    const applyAutoresize = () => {
      textarea.style.height = 'auto';
      const sh = Number(textarea.scrollHeight);
      if (Number.isFinite(sh) && sh > 0) {
        if (maxRows != null) {
          const lineHeight = computeLineHeightPx(textarea);
          const cap = lineHeight * maxRows;
          textarea.style.height = `${Math.min(sh, cap)}px`;
          textarea.style.overflowY = sh > cap ? 'auto' : 'hidden';
        } else {
          textarea.style.height = `${sh}px`;
          textarea.style.overflowY = 'hidden';
        }
      }
    };
    // Initial + per-input.
    applyAutoresize();
    textarea.addEventListener('input', applyAutoresize);
    ctx.registerCleanup(() =>
      textarea.removeEventListener('input', applyAutoresize)
    );
    // Reactive: store push też musi przeliczyć wysokość.
    ctx.registerCleanup(ctx.store.subscribe(bindPath, applyAutoresize));
  }

  // Disabled lub readonly tłumi WSZYSTKIE eventy. Textarea NIE emituje
  // `submit` (Enter w textarea służy do newline — spec form/inputs.rs
  // Textarea nie deklaruje submit handler'a, tylko input/change/focus/blur).
  const muted = () => isDisabledFn() || isReadonlyFn();
  const onInput = () => {
    if (muted()) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('input', {
        bubbles: false,
        detail: { value: textarea.value, kind: 'tstr' },
      })
    );
  };
  const onChange = () => {
    if (muted()) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: textarea.value, kind: 'tstr' },
      })
    );
  };
  const onFocus = () => {
    if (muted()) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('focus', {
        bubbles: false,
        detail: null,
      })
    );
  };
  const onBlur = () => {
    if (muted()) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('blur', {
        bubbles: false,
        detail: null,
      })
    );
  };
  textarea.addEventListener('input', onInput);
  textarea.addEventListener('change', onChange);
  textarea.addEventListener('focus', onFocus);
  textarea.addEventListener('blur', onBlur);
  ctx.registerCleanup(() => {
    textarea.removeEventListener('input', onInput);
    textarea.removeEventListener('change', onChange);
    textarea.removeEventListener('focus', onFocus);
    textarea.removeEventListener('blur', onBlur);
  });

  return wrapper;
}

/// Wyciąga line-height w pikselach z computed style. Fallback 1.2 *
/// font-size, dalsza ostateczność: 20px (mid-range default browserów).
function computeLineHeightPx(element) {
  const getCs = globalThis.getComputedStyle;
  if (typeof getCs !== 'function') return 20;
  const cs = getCs(element);
  const lh = cs && cs.lineHeight;
  if (typeof lh === 'string' && lh.endsWith('px')) {
    const n = Number.parseFloat(lh);
    if (Number.isFinite(n) && n > 0) return n;
  }
  const fs = cs && cs.fontSize;
  if (typeof fs === 'string' && fs.endsWith('px')) {
    const n = Number.parseFloat(fs);
    if (Number.isFinite(n) && n > 0) return n * 1.2;
  }
  return 20;
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
