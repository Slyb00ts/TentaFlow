// =============================================================================
// Plik: sdk-runtime/form-range-numeric-renderer.js
// Opis: Renderery numerycznych form controls — chunk 3.3c-5:
//   - Slider        (0x030F) — single-handle (input type=range) + marks + show_value
//   - RangeSlider   (0x0310) — two-handle z min_separation enforcement
//   - SliderRow     (0x0311) — Slider z label + layout horizontal/compact
//   - NumericInput  (0x0312) — input type=number + min/max/step/precision + format
//   - CurrencyInput (0x0313) — NumericInput specialized z currency_code + show_symbol
//
// Wszystkie używają natywnych HTML5 inputs (range/number) — keyboard a11y
// dostarcza browser. Emit'ują `change` z value (kind='f64') i `input` z
// surowymi wartościami w trakcie przesuwania (Slider). Reactive bind_path
// read-only; write-back chunk 3.6.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/form/range.rs + form/inputs.rs
// (NumericInput/CurrencyInput).
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, formatValue } from './bind-resolver.js';

// =============================================================================
// Walidatory
// =============================================================================

const TONES = new Set(['neutral', 'primary', 'success', 'warning', 'critical', 'info', 'muted']);
const SLIDER_ROW_LAYOUTS = new Set(['horizontal', 'compact']);
const INPUT_SIZES = new Set(['sm', 'md', 'lg']);
const SLIDER_MARK_KEYS = new Set([0, 1]);
// ISO 4217: trzy wielkie litery.
const CURRENCY_CODE_RE = /^[A-Z]{3}$/;
const VALUE_FORMAT_KINDS = new Set([
  'number', 'currency', 'percent', 'bytes', 'duration',
  'date', 'time', 'datetime', 'relative', 'plain',
]);

/// Eager shape-check dla opcjonalnego ValueFormat — niezależnie od tego, czy
/// renderer wykorzystuje go w danej ścieżce. Bez tej walidacji bind-resolver
/// `formatValue` lazy-throw'a dopiero gdy badge jest renderowany, co
/// pozwala niepoprawnym konfiguracjom przejść przez render.
function assertValueFormat(fmt, ctx) {
  if (fmt == null) return;
  if (!fmt || typeof fmt !== 'object') {
    throw new TypeError(`${ctx}: ValueFormat must be object`);
  }
  if (typeof fmt.kind !== 'string' || !VALUE_FORMAT_KINDS.has(fmt.kind)) {
    throw new TypeError(`${ctx}: ValueFormat.kind must be one of ${[...VALUE_FORMAT_KINDS].join('/')}`);
  }
}

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(`${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`);
  }
  return v;
}
function requireBool(v, ctx) {
  if (typeof v !== 'boolean') throw new TypeError(`${ctx}: expected boolean, got ${typeof v}`);
  return v;
}
function requireF64(v, ctx) {
  if (typeof v !== 'number' || !Number.isFinite(v)) {
    throw new TypeError(`${ctx}: expected finite f64, got ${v}`);
  }
  return v;
}
function requireU8(v, ctx) {
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) throw new TypeError(`${ctx}: expected u8, got ${v}`);
  return v;
}
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
  return v;
}
function assertOnlyKnownFields(fields, allowedKeys, name) {
  for (const [k] of fields) {
    if (!allowedKeys.has(k)) {
      throw new TypeError(`${name}: unknown field key ${k} (allowed: ${[...allowedKeys].join(',')})`);
    }
  }
}

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

function parseSliderMark(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: SliderMark must be FieldMap`);
  const seen = new Set();
  let value, label = null;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!SLIDER_MARK_KEYS.has(k)) throw new TypeError(`${ctx}: unknown SliderMark key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) value = requireF64(v, `${ctx}.value`);
    else if (v != null) label = v;
  }
  if (value === undefined) throw new TypeError(`${ctx}: value required`);
  return { value, label };
}

/// Wspólna konfiguracja minimum/maximum/step dla wszystkich sliderów.
function parseSliderBounds(min, max, step, name) {
  if (min >= max) throw new TypeError(`${name}: min must be < max`);
  if (step <= 0) throw new TypeError(`${name}: step must be > 0`);
}

/// Format value na potrzeby badge'a "show_value". Używa formatValue z
/// bind-resolver gdy `format` jest set; inaczej trim trailing zeros.
function fmtSliderValue(value, format, locale) {
  if (format) return formatValue(value, format, locale);
  if (Number.isInteger(value)) return String(value);
  return String(value);
}

/// Tworzy <datalist> z markami, jeśli zdefiniowane. Wraca id datalist'u
/// lub null gdy brak.
function buildDatalist(marks, idPrefix, ctx) {
  if (!marks || marks.length === 0) return null;
  const dl = document.createElement('datalist');
  const dlId = `${idPrefix}-marks`;
  dl.setAttribute('id', dlId);
  for (const m of marks) {
    const opt = document.createElement('option');
    opt.setAttribute('value', String(m.value));
    if (m.label != null) {
      const lv = resolveBindRef(m.label, ctx.store);
      if (lv != null) opt.setAttribute('label', String(lv));
    }
    dl.appendChild(opt);
  }
  return { id: dlId, el: dl };
}

/// Reactive disabled na <input> — natywny disabled attr działa dla range/number.
function applyInputDisabledReactive(input, bindRef, ctx) {
  if (bindRef == null) return () => false;
  let active = false;
  const apply = () => {
    active = resolveBindRef(bindRef, ctx.store) === true;
    if (active) {
      input.setAttribute('disabled', '');
      input.setAttribute('aria-disabled', 'true');
    } else {
      input.removeAttribute('disabled');
      input.removeAttribute('aria-disabled');
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
  return () => active;
}

/// Sync `<input>.value` (string) ze store (number). Pomija nadpisanie gdy
/// input ma focus żeby nie burzyć user'owi przesuwania/wpisywania.
function applyNumericValueReactive(input, bindPath, ctx, min, max) {
  const apply = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    if (typeof v !== 'number' || !Number.isFinite(v)) {
      // Pusta wartość — pozostaw input pusty (number input akceptuje '').
      if (document.activeElement !== input) input.value = '';
      return;
    }
    // Clamp do [min, max] dla bezpieczeństwa renderowania (browser i tak
    // by clamp'ował przy validation, ale wizualnie chcemy spójność).
    let clamped = v;
    if (min != null && clamped < min) clamped = min;
    if (max != null && clamped > max) clamped = max;
    const next = String(clamped);
    if (input.value === next) return;
    if (document.activeElement === input) return;
    input.value = next;
  };
  apply();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, apply));
}

// =============================================================================
// Slider (0x030F)
// =============================================================================

export const SLIDER_TAG = 0x030F;
const SLIDER_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8]);

function renderSlider(component, ctx) {
  assertOnlyKnownFields(component.fields, SLIDER_FIELD_KEYS, 'Slider');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'Slider.bind_path');
  const min = requireF64(ctx.readField(component.fields, 1), 'Slider.min');
  const max = requireF64(ctx.readField(component.fields, 2), 'Slider.max');
  const step = requireF64(ctx.readField(component.fields, 3), 'Slider.step');
  parseSliderBounds(min, max, step, 'Slider');
  const labelBind = ctx.readField(component.fields, 4);
  const showValue = requireBool(ctx.readField(component.fields, 5), 'Slider.show_value');
  const format = ctx.readField(component.fields, 6);
  assertValueFormat(format, 'Slider.format');
  const marksRaw = ctx.readField(component.fields, 7);
  const marks = marksRaw == null ? null : (() => {
    if (!Array.isArray(marksRaw)) throw new TypeError('Slider.marks: expected Array<SliderMark>');
    return marksRaw.map((m, i) => parseSliderMark(m, `Slider.marks[${i}]`));
  })();
  const tone = requireEnum(ctx.readField(component.fields, 8), TONES, 'Slider.tone');

  // Wrapper holds tf-slider + optional label + optional value badge.
  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-slider');
  wrapper.classList.add(`tf-slider--tone-${tone}`);

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-slider__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const fieldRow = document.createElement('div');
  fieldRow.classList.add('tf-slider__field');

  const slider = document.createElement('tf-slider');
  slider.setAttribute('min', String(min));
  slider.setAttribute('max', String(max));
  slider.setAttribute('step', String(step));

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('tf-slider without label requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('tf-slider.a11y.label must resolve to non-blank string');
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) slider.setAttribute('aria-label', v);
      else slider.removeAttribute('aria-label');
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  // Reactive value sync: store -> tf-slider.value
  const applyValue = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    if (typeof v !== 'number' || !Number.isFinite(v)) return;
    let clamped = v;
    if (clamped < min) clamped = min;
    if (clamped > max) clamped = max;
    const next = String(clamped);
    if (slider.value !== next) slider.value = next;
  };
  applyValue();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, applyValue));

  fieldRow.appendChild(slider);

  let valueBadge = null;
  if (showValue) {
    valueBadge = document.createElement('span');
    valueBadge.classList.add('tf-slider__value');
    valueBadge.setAttribute('aria-live', 'polite');
    const sync = () => {
      const raw = Number.parseFloat(slider.value);
      valueBadge.textContent = Number.isFinite(raw) ? fmtSliderValue(raw, format, ctx.locale) : '';
    };
    sync();
    ctx.registerCleanup(ctx.store.subscribe(bindPath, sync));
    slider.addEventListener('input', sync);
    ctx.registerCleanup(() => slider.removeEventListener('input', sync));
    fieldRow.appendChild(valueBadge);
  }

  wrapper.appendChild(fieldRow);

  // tf-slider emits input/change with detail.value (string). Intercept,
  // convert to f64 and re-emit with SDK format on wrapper.
  const onInput = (e) => {
    e.stopPropagation();
    const v = Number.parseFloat(e.detail?.value ?? slider.value);
    if (!Number.isFinite(v)) return;
    wrapper.dispatchEvent(
      new CustomEvent('input', {
        bubbles: false,
        detail: { value: v, kind: 'f64' },
      })
    );
  };
  const onChange = (e) => {
    e.stopPropagation();
    const v = Number.parseFloat(e.detail?.value ?? slider.value);
    if (!Number.isFinite(v)) return;
    wrapper.dispatchEvent(
      new CustomEvent('change', {
        bubbles: false,
        detail: { value: v, kind: 'f64' },
      })
    );
  };
  slider.addEventListener('input', onInput);
  slider.addEventListener('change', onChange);
  ctx.registerCleanup(() => {
    slider.removeEventListener('input', onInput);
    slider.removeEventListener('change', onChange);
  });

  return wrapper;
}

/// Współdzielony builder dla Slider + SliderRow. SliderRow opakuje to
/// dodatkowym layout wrapperem.
function buildSliderUi({
  component, ctx, bindPath, min, max, step, labelBind, showValue, format, marks,
  tone, className, layout,
}) {
  const wrapper = document.createElement('div');
  wrapper.classList.add(className);
  wrapper.classList.add(`${className}--tone-${tone}`);
  if (layout) wrapper.classList.add(`${className}--layout-${layout}`);

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add(`${className}__label`);
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const fieldRow = document.createElement('div');
  fieldRow.classList.add(`${className}__field`);

  const input = document.createElement('input');
  input.setAttribute('type', 'range');
  input.classList.add(`${className}__input`);
  input.setAttribute('min', String(min));
  input.setAttribute('max', String(max));
  input.setAttribute('step', String(step));
  const inputId = `${className}-${component.id}`;
  input.setAttribute('id', inputId);
  if (labelEl) labelEl.setAttribute('for', inputId);

  // Datalist z markami (browser native).
  const datalist = buildDatalist(marks, inputId, ctx);
  if (datalist) {
    input.setAttribute('list', datalist.id);
    wrapper.appendChild(datalist.el);
  }

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(`${className} without label requires Component.a11y.label`);
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(`${className}.a11y.label must resolve to non-blank string`);
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) input.setAttribute('aria-label', v);
      else input.removeAttribute('aria-label');
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  applyNumericValueReactive(input, bindPath, ctx, min, max);
  fieldRow.appendChild(input);

  let valueBadge = null;
  if (showValue) {
    valueBadge = document.createElement('span');
    valueBadge.classList.add(`${className}__value`);
    valueBadge.setAttribute('aria-live', 'polite');
    const sync = () => {
      const raw = Number.parseFloat(input.value);
      valueBadge.textContent = Number.isFinite(raw) ? fmtSliderValue(raw, format, ctx.locale) : '';
    };
    sync();
    ctx.registerCleanup(ctx.store.subscribe(bindPath, sync));
    input.addEventListener('input', sync);
    ctx.registerCleanup(() => input.removeEventListener('input', sync));
    fieldRow.appendChild(valueBadge);
  }

  wrapper.appendChild(fieldRow);

  const onInput = () => {
    const v = Number.parseFloat(input.value);
    if (!Number.isFinite(v)) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('input', {
        bubbles: false,
        detail: { value: v, kind: 'f64' },
      })
    );
  };
  const onChange = () => {
    const v = Number.parseFloat(input.value);
    if (!Number.isFinite(v)) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: v, kind: 'f64' },
      })
    );
  };
  input.addEventListener('input', onInput);
  input.addEventListener('change', onChange);
  ctx.registerCleanup(() => {
    input.removeEventListener('input', onInput);
    input.removeEventListener('change', onChange);
  });

  return wrapper;
}

// =============================================================================
// RangeSlider (0x0310)
// =============================================================================

export const RANGE_SLIDER_TAG = 0x0310;
const RANGE_SLIDER_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

function renderRangeSlider(component, ctx) {
  assertOnlyKnownFields(component.fields, RANGE_SLIDER_FIELD_KEYS, 'RangeSlider');

  const bindPathMin = requirePath(ctx.readField(component.fields, 0), 'RangeSlider.bind_path_min');
  const bindPathMax = requirePath(ctx.readField(component.fields, 1), 'RangeSlider.bind_path_max');
  const min = requireF64(ctx.readField(component.fields, 2), 'RangeSlider.min');
  const max = requireF64(ctx.readField(component.fields, 3), 'RangeSlider.max');
  const step = requireF64(ctx.readField(component.fields, 4), 'RangeSlider.step');
  parseSliderBounds(min, max, step, 'RangeSlider');
  const labelBind = ctx.readField(component.fields, 5);
  const showValue = requireBool(ctx.readField(component.fields, 6), 'RangeSlider.show_value');
  const format = ctx.readField(component.fields, 7);
  assertValueFormat(format, 'RangeSlider.format');
  const marksRaw = ctx.readField(component.fields, 8);
  const marks = marksRaw == null ? null : (() => {
    if (!Array.isArray(marksRaw)) throw new TypeError('RangeSlider.marks: expected Array<SliderMark>');
    return marksRaw.map((m, i) => parseSliderMark(m, `RangeSlider.marks[${i}]`));
  })();
  const tone = requireEnum(ctx.readField(component.fields, 9), TONES, 'RangeSlider.tone');
  const minSeparation = requireF64(ctx.readField(component.fields, 10), 'RangeSlider.min_separation');
  if (minSeparation < 0) throw new TypeError('RangeSlider.min_separation must be >= 0');
  if (minSeparation > max - min) {
    throw new TypeError('RangeSlider.min_separation must be <= (max - min)');
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-range-slider');
  wrapper.classList.add(`tf-range-slider--tone-${tone}`);

  let labelEl = null;
  let labelDomId = null;
  if (labelBind != null) {
    labelEl = document.createElement('div');
    labelEl.classList.add('tf-range-slider__label');
    labelDomId = `tf-range-slider-${component.id}-label`;
    labelEl.setAttribute('id', labelDomId);
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const fieldRow = document.createElement('div');
  fieldRow.classList.add('tf-range-slider__field');

  const makeHandle = (suffix, ariaSuffix) => {
    const i = document.createElement('input');
    i.setAttribute('type', 'range');
    i.classList.add('tf-range-slider__input');
    i.classList.add(`tf-range-slider__input--${suffix}`);
    i.setAttribute('min', String(min));
    i.setAttribute('max', String(max));
    i.setAttribute('step', String(step));
    i.setAttribute('id', `tf-range-slider-${component.id}-${suffix}`);
    if (labelDomId) i.setAttribute('aria-labelledby', `${labelDomId} ${labelDomId}-${suffix}`);
    return i;
  };
  const minInput = makeHandle('min', 'min');
  const maxInput = makeHandle('max', 'max');

  const datalist = buildDatalist(marks, `tf-range-slider-${component.id}`, ctx);
  if (datalist) {
    minInput.setAttribute('list', datalist.id);
    maxInput.setAttribute('list', datalist.id);
    wrapper.appendChild(datalist.el);
  }

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('RangeSlider without label requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('RangeSlider.a11y.label must resolve to non-blank string');
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) {
        minInput.setAttribute('aria-label', `${v} (min)`);
        maxInput.setAttribute('aria-label', `${v} (max)`);
      } else {
        minInput.removeAttribute('aria-label');
        maxInput.removeAttribute('aria-label');
      }
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  applyNumericValueReactive(minInput, bindPathMin, ctx, min, max);
  applyNumericValueReactive(maxInput, bindPathMax, ctx, min, max);
  fieldRow.appendChild(minInput);
  fieldRow.appendChild(maxInput);

  let badgeMin = null, badgeMax = null;
  if (showValue) {
    badgeMin = document.createElement('span');
    badgeMin.classList.add('tf-range-slider__value');
    badgeMin.classList.add('tf-range-slider__value--min');
    badgeMax = document.createElement('span');
    badgeMax.classList.add('tf-range-slider__value');
    badgeMax.classList.add('tf-range-slider__value--max');
    const syncMin = () => {
      const v = Number.parseFloat(minInput.value);
      badgeMin.textContent = Number.isFinite(v) ? fmtSliderValue(v, format, ctx.locale) : '';
    };
    const syncMax = () => {
      const v = Number.parseFloat(maxInput.value);
      badgeMax.textContent = Number.isFinite(v) ? fmtSliderValue(v, format, ctx.locale) : '';
    };
    syncMin(); syncMax();
    minInput.addEventListener('input', syncMin);
    maxInput.addEventListener('input', syncMax);
    ctx.registerCleanup(() => {
      minInput.removeEventListener('input', syncMin);
      maxInput.removeEventListener('input', syncMax);
    });
    ctx.registerCleanup(ctx.store.subscribe(bindPathMin, syncMin));
    ctx.registerCleanup(ctx.store.subscribe(bindPathMax, syncMax));
    fieldRow.appendChild(badgeMin);
    fieldRow.appendChild(badgeMax);
  }

  wrapper.appendChild(fieldRow);

  // min_separation enforcement na change: jeśli range zbyt wąski, revert
  // do poprzedniej wartości tej handle.
  let lastMin = Number.parseFloat(minInput.value);
  let lastMax = Number.parseFloat(maxInput.value);
  if (!Number.isFinite(lastMin)) lastMin = min;
  if (!Number.isFinite(lastMax)) lastMax = max;
  ctx.registerCleanup(ctx.store.subscribe(bindPathMin, () => {
    const v = Number.parseFloat(minInput.value);
    if (Number.isFinite(v)) lastMin = v;
  }));
  ctx.registerCleanup(ctx.store.subscribe(bindPathMax, () => {
    const v = Number.parseFloat(maxInput.value);
    if (Number.isFinite(v)) lastMax = v;
  }));

  const validateAndEmit = (which) => {
    const lo = Number.parseFloat(minInput.value);
    const hi = Number.parseFloat(maxInput.value);
    if (!Number.isFinite(lo) || !Number.isFinite(hi)) return;
    // Separation enforcement.
    if (hi - lo < minSeparation) {
      // Revert tego handle który się ruszył.
      if (which === 'min') {
        minInput.value = String(lastMin);
      } else {
        maxInput.value = String(lastMax);
      }
      return;
    }
    // min <= max enforcement (redundant z separation>=0, ale explicit).
    if (lo > hi) {
      if (which === 'min') minInput.value = String(lastMin);
      else maxInput.value = String(lastMax);
      return;
    }
    lastMin = lo; lastMax = hi;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: { min: lo, max: hi }, kind: 'range', changed: which },
      })
    );
  };
  const onMinChange = () => validateAndEmit('min');
  const onMaxChange = () => validateAndEmit('max');
  minInput.addEventListener('change', onMinChange);
  maxInput.addEventListener('change', onMaxChange);
  ctx.registerCleanup(() => {
    minInput.removeEventListener('change', onMinChange);
    maxInput.removeEventListener('change', onMaxChange);
  });

  return wrapper;
}

// =============================================================================
// SliderRow (0x0311)
// =============================================================================

export const SLIDER_ROW_TAG = 0x0311;
const SLIDER_ROW_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8]);

function renderSliderRow(component, ctx) {
  assertOnlyKnownFields(component.fields, SLIDER_ROW_FIELD_KEYS, 'SliderRow');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'SliderRow.bind_path');
  const min = requireF64(ctx.readField(component.fields, 1), 'SliderRow.min');
  const max = requireF64(ctx.readField(component.fields, 2), 'SliderRow.max');
  const step = requireF64(ctx.readField(component.fields, 3), 'SliderRow.step');
  parseSliderBounds(min, max, step, 'SliderRow');
  const labelBind = ctx.readField(component.fields, 4);
  if (labelBind == null) throw new TypeError('SliderRow.label is required');
  const format = ctx.readField(component.fields, 5);
  assertValueFormat(format, 'SliderRow.format');
  const marksRaw = ctx.readField(component.fields, 6);
  const marks = marksRaw == null ? null : (() => {
    if (!Array.isArray(marksRaw)) throw new TypeError('SliderRow.marks: expected Array<SliderMark>');
    return marksRaw.map((m, i) => parseSliderMark(m, `SliderRow.marks[${i}]`));
  })();
  const tone = requireEnum(ctx.readField(component.fields, 7), TONES, 'SliderRow.tone');
  const layout = requireEnum(ctx.readField(component.fields, 8), SLIDER_ROW_LAYOUTS, 'SliderRow.layout');

  // SliderRow ZAWSZE pokazuje value (label + slider + value w jednej linii).
  return buildSliderUi({
    component, ctx, bindPath, min, max, step, labelBind, showValue: true, format,
    marks, tone, className: 'tf-slider-row', layout,
  });
}

// =============================================================================
// NumericInput (0x0312)
// =============================================================================

export const NUMERIC_INPUT_TAG = 0x0312;
const NUMERIC_INPUT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

function renderNumericInput(component, ctx) {
  assertOnlyKnownFields(component.fields, NUMERIC_INPUT_FIELD_KEYS, 'NumericInput');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'NumericInput.bind_path');
  const minRaw = ctx.readField(component.fields, 1);
  const maxRaw = ctx.readField(component.fields, 2);
  const step = requireF64(ctx.readField(component.fields, 3), 'NumericInput.step');
  if (step <= 0) throw new TypeError('NumericInput.step must be > 0');
  const precision = requireU8(ctx.readField(component.fields, 4), 'NumericInput.precision');
  const format = ctx.readField(component.fields, 5);
  assertValueFormat(format, 'NumericInput.format');
  const labelBind = ctx.readField(component.fields, 6);
  const hintBind = ctx.readField(component.fields, 7);
  const size = requireEnum(ctx.readField(component.fields, 8), INPUT_SIZES, 'NumericInput.size');
  const localeAware = requireBool(ctx.readField(component.fields, 9), 'NumericInput.locale_aware');
  const minVal = minRaw == null ? null : requireF64(minRaw, 'NumericInput.min');
  const maxVal = maxRaw == null ? null : requireF64(maxRaw, 'NumericInput.max');
  if (minVal != null && maxVal != null && minVal > maxVal) {
    throw new TypeError('NumericInput.min must be <= max');
  }

  return buildNumericUi({
    component, ctx, bindPath, minVal, maxVal, step, precision, format,
    labelBind, hintBind, size, localeAware,
    className: 'tf-numeric',
    extraSpec: null,
  });
}

// =============================================================================
// CurrencyInput (0x0313)
// =============================================================================

export const CURRENCY_INPUT_TAG = 0x0313;
const CURRENCY_INPUT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);

function renderCurrencyInput(component, ctx) {
  assertOnlyKnownFields(component.fields, CURRENCY_INPUT_FIELD_KEYS, 'CurrencyInput');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'CurrencyInput.bind_path');
  const currencyCode = requireString(ctx.readField(component.fields, 1), 'CurrencyInput.currency_code');
  if (!CURRENCY_CODE_RE.test(currencyCode)) {
    throw new TypeError('CurrencyInput.currency_code must be ISO 4217 (3 uppercase letters)');
  }
  const minRaw = ctx.readField(component.fields, 2);
  const maxRaw = ctx.readField(component.fields, 3);
  const stepRaw = ctx.readField(component.fields, 4);
  const precisionRaw = ctx.readField(component.fields, 5);
  const labelBind = ctx.readField(component.fields, 6);
  const hintBind = ctx.readField(component.fields, 7);
  const size = requireEnum(ctx.readField(component.fields, 8), INPUT_SIZES, 'CurrencyInput.size');
  const showSymbol = requireBool(ctx.readField(component.fields, 9), 'CurrencyInput.show_symbol');
  const localeAware = requireBool(ctx.readField(component.fields, 10), 'CurrencyInput.locale_aware');
  // §5 0x0313 defaults: step=0.01, precision=2.
  const step = stepRaw == null ? 0.01 : requireF64(stepRaw, 'CurrencyInput.step');
  if (step <= 0) throw new TypeError('CurrencyInput.step must be > 0');
  const precision = precisionRaw == null ? 2 : requireU8(precisionRaw, 'CurrencyInput.precision');
  const minVal = minRaw == null ? null : requireF64(minRaw, 'CurrencyInput.min');
  const maxVal = maxRaw == null ? null : requireF64(maxRaw, 'CurrencyInput.max');
  if (minVal != null && maxVal != null && minVal > maxVal) {
    throw new TypeError('CurrencyInput.min must be <= max');
  }

  // Currency-style format dla badge'a — Intl.NumberFormat.
  const fmtBadge = (val) => {
    if (!Number.isFinite(val)) return '';
    try {
      return new Intl.NumberFormat(ctx.locale, {
        style: 'currency',
        currency: currencyCode,
        currencyDisplay: showSymbol ? 'symbol' : 'code',
        minimumFractionDigits: precision,
        maximumFractionDigits: precision,
      }).format(val);
    } catch {
      return val.toFixed(precision);
    }
  };

  return buildNumericUi({
    component, ctx, bindPath, minVal, maxVal, step, precision, format: null,
    labelBind, hintBind, size, localeAware,
    className: 'tf-currency',
    extraSpec: { currencyCode, showSymbol, fmtBadge },
  });
}

// =============================================================================
// Shared numeric UI builder
// =============================================================================

function buildNumericUi({
  component, ctx, bindPath, minVal, maxVal, step, precision, format,
  labelBind, hintBind, size, localeAware, className, extraSpec,
}) {
  const wrapper = document.createElement('div');
  wrapper.classList.add(className);
  wrapper.classList.add(`${className}--size-${size}`);
  if (localeAware) wrapper.classList.add(`${className}--locale-aware`);

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add(`${className}__label`);
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const fieldRow = document.createElement('div');
  fieldRow.classList.add(`${className}__field`);

  // Currency: symbol prefix (jeśli show_symbol=true) na lewej.
  if (extraSpec && extraSpec.showSymbol) {
    const sym = document.createElement('span');
    sym.classList.add(`${className}__symbol`);
    sym.setAttribute('aria-hidden', 'true');
    // Bazujemy na Intl.NumberFormat dla symbolu locale-aware (np. zł vs PLN).
    try {
      const parts = new Intl.NumberFormat(ctx.locale, {
        style: 'currency', currency: extraSpec.currencyCode, currencyDisplay: 'symbol',
      }).formatToParts(0);
      const symbol = parts.find((p) => p.type === 'currency');
      sym.textContent = symbol ? symbol.value : extraSpec.currencyCode;
    } catch {
      sym.textContent = extraSpec.currencyCode;
    }
    fieldRow.appendChild(sym);
  }

  const input = document.createElement('input');
  input.setAttribute('type', 'number');
  input.classList.add(`${className}__input`);
  const inputId = `${className}-${component.id}`;
  input.setAttribute('id', inputId);
  if (labelEl) labelEl.setAttribute('for', inputId);
  if (minVal != null) input.setAttribute('min', String(minVal));
  if (maxVal != null) input.setAttribute('max', String(maxVal));
  input.setAttribute('step', String(step));
  if (precision > 0) input.setAttribute('inputmode', 'decimal');
  else input.setAttribute('inputmode', 'numeric');

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(`${className} without label requires Component.a11y.label`);
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(`${className}.a11y.label must resolve to non-blank string`);
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) input.setAttribute('aria-label', v);
      else input.removeAttribute('aria-label');
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  applyNumericValueReactive(input, bindPath, ctx, minVal, maxVal);
  fieldRow.appendChild(input);
  wrapper.appendChild(fieldRow);

  if (hintBind != null) {
    const hint = document.createElement('span');
    hint.classList.add(`${className}__hint`);
    applyTextBind(hint, hintBind, ctx);
    wrapper.appendChild(hint);
  }

  // Locale-aware/currency badge — pokazuje sformatowaną wersję wartości
  // pod input'em (read-only). Tylko gdy localeAware (NumericInput) lub
  // zawsze (CurrencyInput).
  const wantsBadge = localeAware || (extraSpec && extraSpec.fmtBadge);
  if (wantsBadge) {
    const badge = document.createElement('span');
    badge.classList.add(`${className}__formatted`);
    badge.setAttribute('aria-hidden', 'true');
    const sync = () => {
      const v = Number.parseFloat(input.value);
      if (!Number.isFinite(v)) { badge.textContent = ''; return; }
      if (extraSpec && extraSpec.fmtBadge) {
        badge.textContent = extraSpec.fmtBadge(v);
      } else if (format) {
        badge.textContent = formatValue(v, format, ctx.locale);
      } else {
        try {
          badge.textContent = new Intl.NumberFormat(ctx.locale, {
            minimumFractionDigits: precision,
            maximumFractionDigits: precision,
          }).format(v);
        } catch {
          badge.textContent = v.toFixed(precision);
        }
      }
    };
    sync();
    input.addEventListener('input', sync);
    ctx.registerCleanup(() => input.removeEventListener('input', sync));
    ctx.registerCleanup(ctx.store.subscribe(bindPath, sync));
    wrapper.appendChild(badge);
  }

  const onChange = () => {
    const v = Number.parseFloat(input.value);
    if (!Number.isFinite(v)) {
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('change', {
          bubbles: false,
          detail: { value: null, kind: null },
        })
      );
      return;
    }
    // Round na precision miejsc — defensywne; browser już clamp'uje wartość
    // do step ale precision niezawsze.
    const factor = Math.pow(10, precision);
    const rounded = Math.round(v * factor) / factor;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: extraSpec
          ? { value: rounded, kind: 'f64', currency: extraSpec.currencyCode }
          : { value: rounded, kind: 'f64' },
      })
    );
  };
  const onInput = () => {
    const v = Number.parseFloat(input.value);
    if (!Number.isFinite(v)) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('input', {
        bubbles: false,
        detail: { value: v, kind: 'f64' },
      })
    );
  };
  input.addEventListener('change', onChange);
  input.addEventListener('input', onInput);
  ctx.registerCleanup(() => {
    input.removeEventListener('change', onChange);
    input.removeEventListener('input', onInput);
  });

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormRangeNumericRenderers() {
  if (!lookupComponentRenderer(SLIDER_TAG)) registerComponentRenderer(SLIDER_TAG, renderSlider);
  if (!lookupComponentRenderer(RANGE_SLIDER_TAG)) registerComponentRenderer(RANGE_SLIDER_TAG, renderRangeSlider);
  if (!lookupComponentRenderer(SLIDER_ROW_TAG)) registerComponentRenderer(SLIDER_ROW_TAG, renderSliderRow);
  if (!lookupComponentRenderer(NUMERIC_INPUT_TAG)) registerComponentRenderer(NUMERIC_INPUT_TAG, renderNumericInput);
  if (!lookupComponentRenderer(CURRENCY_INPUT_TAG)) registerComponentRenderer(CURRENCY_INPUT_TAG, renderCurrencyInput);
}
