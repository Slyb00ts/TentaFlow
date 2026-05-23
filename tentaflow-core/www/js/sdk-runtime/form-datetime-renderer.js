// =============================================================================
// Plik: sdk-runtime/form-datetime-renderer.js
// Opis: Renderery datetime pickerów — chunk 3.3c-4:
//   - DatePicker     (0x0314) — input type=date + presets + min/max + disabled_dates
//   - DateRangePicker (0x0315) — 2x date input + range presets + max_range_days
//   - TimePicker     (0x0316) — input type=time + step (step_minutes * 60)
//   - DateTimePicker (0x0317) — input type=datetime-local + min/max + timezone
//
// Wszystkie używają natywnych HTML5 pickerów (browser zapewnia UI/keyboard/locale),
// emit'ują `change` z reactive bind_path (jednokierunkowy read; write-back chunk 3.6).
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/form/datetime.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

const DATE_STYLES = new Set(['short', 'medium', 'long', 'full']);
const TIME_STYLES = new Set(['short', 'medium', 'long']);
const TIME_PRECISIONS = new Set(['minute', 'second']);
const DAY_OF_WEEK = new Set(['sunday', 'monday']);
const DATE_PRESET_KIND = new Set(['today', 'yesterday', 'last_7_days', 'last_30_days', 'this_month', 'last_month', 'custom']);
// ISO 8601 calendar date (YYYY-MM-DD) — restrictive validator dla min/max/disabled_dates.
const ISO_DATE_RE = /^\d{4}-\d{2}-\d{2}$/;
// ISO 8601 local datetime (YYYY-MM-DDTHH:MM lub z sekundami).
const ISO_LOCAL_DT_RE = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}(:\d{2})?$/;
// IANA timezone basic check (Region/City form). Nie waliduje pełnej listy —
// tylko podstawowy shape.
const TZ_RE = /^[A-Za-z_]+(\/[A-Za-z_0-9+-]+)+$|^UTC$/;
const PRESET_KEYS = new Set([0, 1, 2]);
const RANGE_PRESET_KEYS = new Set([0, 1, 2]);
const RANGE_INNER_KEYS = new Set([0, 1]);

function requireEnum(v, set, ctx) {
  if (typeof v !== 'string' || !set.has(v)) {
    throw new TypeError(`${ctx}: expected one of ${[...set].join('/')}, got ${JSON.stringify(v)}`);
  }
  return v;
}
function requireString(v, ctx) {
  if (typeof v !== 'string') throw new TypeError(`${ctx}: expected string`);
  return v;
}
function requireIsoDate(v, ctx) {
  if (typeof v !== 'string' || !ISO_DATE_RE.test(v)) {
    throw new TypeError(`${ctx}: expected ISO date YYYY-MM-DD, got ${JSON.stringify(v)}`);
  }
  return v;
}
function requireIsoLocalDt(v, ctx) {
  if (typeof v !== 'string' || !ISO_LOCAL_DT_RE.test(v)) {
    throw new TypeError(`${ctx}: expected ISO local datetime YYYY-MM-DDTHH:MM[:SS], got ${JSON.stringify(v)}`);
  }
  return v;
}
function requireTimezone(v, ctx) {
  if (typeof v !== 'string' || !TZ_RE.test(v)) {
    throw new TypeError(`${ctx}: expected IANA timezone (Region/City or UTC), got ${JSON.stringify(v)}`);
  }
  return v;
}
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
  return v;
}
function requireU16(v, ctx) {
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFF) {
    throw new TypeError(`${ctx}: expected u16, got ${v}`);
  }
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

/// Sync `<input>.value` z store (one-way read). Pomija nadpisanie gdy input
/// jest focusowany żeby nie burzyć user typing'u (browser native picker
/// otwarty).
function applyValueReactive(input, bindPath, ctx, validator) {
  const apply = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    const next = v == null ? '' : String(v);
    if (input.value === next) return;
    // Jeśli walidator i wartość nie jest pusta i nie pasuje formatowi —
    // ignorujemy zamiast wstawiać śmieci do natywnego picker'a.
    if (next !== '' && validator && !validator(next)) return;
    if (document.activeElement === input) return;
    input.value = next;
  };
  apply();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, apply));
}

// =============================================================================
// DatePreset parsing (tagged union per inline.rs DatePresetResolve)
// =============================================================================

function parseDatePresetResolve(raw, ctx) {
  if (!raw || typeof raw !== 'object') throw new TypeError(`${ctx}: DatePresetResolve must be object`);
  if (!DATE_PRESET_KIND.has(raw.kind)) {
    throw new TypeError(`${ctx}.kind unsupported: ${raw.kind}`);
  }
  if (raw.kind === 'custom') {
    if (!Number.isInteger(raw.offset_days)) {
      throw new TypeError(`${ctx}.custom.offset_days must be i32`);
    }
    return { kind: 'custom', offset_days: raw.offset_days };
  }
  // Inne warianty mają tylko `kind`.
  for (const k of Object.keys(raw)) {
    if (k !== 'kind') throw new TypeError(`${ctx}: unexpected key '${k}' for kind=${raw.kind}`);
  }
  return { kind: raw.kind };
}

function parseDatePreset(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: DatePreset must be FieldMap`);
  const seen = new Set();
  let id, label, resolve;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!PRESET_KEYS.has(k)) throw new TypeError(`${ctx}: unknown DatePreset key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) id = requireString(v, `${ctx}.id`);
    else if (k === 1) label = v;
    else resolve = parseDatePresetResolve(v, `${ctx}.resolve`);
  }
  if (id === undefined) throw new TypeError(`${ctx}: id required`);
  if (label === undefined) throw new TypeError(`${ctx}: label required`);
  if (resolve === undefined) throw new TypeError(`${ctx}: resolve required`);
  return { id, label, resolve };
}

function parseRangePreset(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: RangePreset must be FieldMap`);
  const seen = new Set();
  let id, label, range;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!RANGE_PRESET_KEYS.has(k)) throw new TypeError(`${ctx}: unknown RangePreset key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) id = requireString(v, `${ctx}.id`);
    else if (k === 1) label = v;
    else {
      // RangePresetRange FieldMap [[0=from_offset_days, 1=to_offset_days]]
      if (!Array.isArray(v)) throw new TypeError(`${ctx}.range must be FieldMap`);
      const innerSeen = new Set();
      let fro, too;
      for (const ie of v) {
        if (!Array.isArray(ie) || ie.length !== 2) throw new TypeError(`${ctx}.range: entry [u8, Value]`);
        const [ik, iv] = ie;
        if (!RANGE_INNER_KEYS.has(ik)) throw new TypeError(`${ctx}.range: unknown key ${ik}`);
        if (innerSeen.has(ik)) throw new TypeError(`${ctx}.range: duplicate key ${ik}`);
        innerSeen.add(ik);
        if (!Number.isInteger(iv) || iv < -0x80000000 || iv > 0x7FFFFFFF) {
          throw new TypeError(`${ctx}.range[${ik}] must be i32`);
        }
        if (ik === 0) fro = iv; else too = iv;
      }
      if (fro === undefined) throw new TypeError(`${ctx}.range.from_offset_days required`);
      if (too === undefined) throw new TypeError(`${ctx}.range.to_offset_days required`);
      range = { from_offset_days: fro, to_offset_days: too };
    }
  }
  if (id === undefined) throw new TypeError(`${ctx}: id required`);
  if (label === undefined) throw new TypeError(`${ctx}: label required`);
  if (range === undefined) throw new TypeError(`${ctx}: range required`);
  return { id, label, range };
}

/// Wylicza datę dla preset'u DatePresetResolve. Zwraca ISO YYYY-MM-DD.
function resolveDatePreset(resolve) {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const fmt = (d) => {
    const y = d.getFullYear();
    const m = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    return `${y}-${m}-${day}`;
  };
  const shift = (d, days) => {
    const out = new Date(d);
    out.setDate(out.getDate() + days);
    return out;
  };
  switch (resolve.kind) {
    case 'today': return fmt(today);
    case 'yesterday': return fmt(shift(today, -1));
    case 'last_7_days': return fmt(shift(today, -7));
    case 'last_30_days': return fmt(shift(today, -30));
    case 'this_month': {
      const d = new Date(today.getFullYear(), today.getMonth(), 1);
      return fmt(d);
    }
    case 'last_month': {
      const d = new Date(today.getFullYear(), today.getMonth() - 1, 1);
      return fmt(d);
    }
    case 'custom': return fmt(shift(today, resolve.offset_days));
  }
  throw new TypeError(`unsupported DatePresetResolve.kind: ${resolve.kind}`);
}

function resolveRangePreset(range) {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const shift = (days) => {
    const d = new Date(today);
    d.setDate(d.getDate() + days);
    return d;
  };
  const fmt = (d) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
  return { from: fmt(shift(range.from_offset_days)), to: fmt(shift(range.to_offset_days)) };
}

// =============================================================================
// DatePicker (0x0314)
// =============================================================================

export const DATE_PICKER_TAG = 0x0314;
const DATE_PICKER_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

function renderDatePicker(component, ctx) {
  assertOnlyKnownFields(component.fields, DATE_PICKER_FIELD_KEYS, 'DatePicker');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'DatePicker.bind_path');
  const labelBind = ctx.readField(component.fields, 1);
  const minDateRaw = ctx.readField(component.fields, 2);
  const maxDateRaw = ctx.readField(component.fields, 3);
  const localeRaw = ctx.readField(component.fields, 4);
  const format = requireEnum(ctx.readField(component.fields, 5), DATE_STYLES, 'DatePicker.format');
  const fdow = requireEnum(ctx.readField(component.fields, 6), DAY_OF_WEEK, 'DatePicker.first_day_of_week');
  const disabledDatesRaw = ctx.readField(component.fields, 7);
  const presetsRaw = ctx.readField(component.fields, 8);
  const placeholderBind = ctx.readField(component.fields, 9);

  const minDate = minDateRaw == null ? null : requireIsoDate(minDateRaw, 'DatePicker.min_date');
  const maxDate = maxDateRaw == null ? null : requireIsoDate(maxDateRaw, 'DatePicker.max_date');
  if (minDate != null && maxDate != null && minDate > maxDate) {
    throw new TypeError('DatePicker.min_date must be <= max_date');
  }
  const locale = localeRaw == null ? null : requireString(localeRaw, 'DatePicker.locale');
  const disabledDates = disabledDatesRaw == null ? null : (() => {
    if (!Array.isArray(disabledDatesRaw)) throw new TypeError('DatePicker.disabled_dates: expected Array<string>');
    return disabledDatesRaw.map((d, i) => requireIsoDate(d, `DatePicker.disabled_dates[${i}]`));
  })();
  const disabledSet = disabledDates ? new Set(disabledDates) : null;
  const presets = presetsRaw == null ? null : (() => {
    if (!Array.isArray(presetsRaw)) throw new TypeError('DatePicker.presets: expected Array<DatePreset>');
    return presetsRaw.map((p, i) => parseDatePreset(p, `DatePicker.presets[${i}]`));
  })();

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-datepicker');
  wrapper.classList.add(`tf-datepicker--format-${format}`);
  wrapper.setAttribute('data-first-day-of-week', fdow);

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-datepicker__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const input = document.createElement('input');
  input.setAttribute('type', 'date');
  input.classList.add('tf-datepicker__input');
  const inputId = `tf-datepicker-${component.id}`;
  input.setAttribute('id', inputId);
  if (labelEl) labelEl.setAttribute('for', inputId);
  if (minDate) input.setAttribute('min', minDate);
  if (maxDate) input.setAttribute('max', maxDate);
  if (locale) input.setAttribute('lang', locale);

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('DatePicker without `label` field requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('DatePicker.a11y.label must resolve to non-blank string');
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) input.setAttribute('aria-label', v);
      else input.removeAttribute('aria-label');
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  applyPlaceholderReactive(input, placeholderBind, ctx);
  applyValueReactive(input, bindPath, ctx, (s) => ISO_DATE_RE.test(s));
  // lastValid musi tropić każdą valid wartość — initial po reactive sync
  // oraz wszystkie późniejsze (preset, store push), inaczej revert wraca
  // do pustego stringu zamiast poprzedniej dobrej daty.
  let lastValid = input.value && ISO_DATE_RE.test(input.value) ? input.value : '';
  ctx.registerCleanup(ctx.store.subscribe(bindPath, () => {
    if (input.value && ISO_DATE_RE.test(input.value)) lastValid = input.value;
  }));
  wrapper.appendChild(input);

  if (presets) {
    const presetBar = document.createElement('div');
    presetBar.classList.add('tf-datepicker__presets');
    for (const p of presets) {
      const btn = document.createElement('button');
      btn.setAttribute('type', 'button');
      btn.classList.add('tf-datepicker__preset');
      btn.setAttribute('data-preset-id', p.id);
      applyTextBind(btn, p.label, ctx);
      const onClick = (e) => {
        e.preventDefault();
        const v = resolveDatePreset(p.resolve);
        // Walidacja zakresu min/max + disabled_dates.
        if (minDate != null && v < minDate) return;
        if (maxDate != null && v > maxDate) return;
        if (disabledSet && disabledSet.has(v)) return;
        input.value = v;
        lastValid = v;
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('change', {
            bubbles: false,
            detail: { value: v, kind: 'tstr', preset_id: p.id },
          })
        );
      };
      btn.addEventListener('click', onClick);
      ctx.registerCleanup(() => btn.removeEventListener('click', onClick));
      presetBar.appendChild(btn);
    }
    wrapper.appendChild(presetBar);
  }

  // Change event — wybór z natywnego picker'a. Waliduj disabled_dates;
  // jeśli user wybrał disabled date, revert do poprzedniej `lastValid`
  // (śledzony przez preset / store sync / poprzedni change).
  const onChange = () => {
    const v = input.value;
    if (v && disabledSet && disabledSet.has(v)) {
      input.value = lastValid;
      return;
    }
    lastValid = v;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: v || null, kind: v ? 'tstr' : null },
      })
    );
  };
  input.addEventListener('change', onChange);
  ctx.registerCleanup(() => input.removeEventListener('change', onChange));

  return wrapper;
}

// =============================================================================
// DateRangePicker (0x0315)
// =============================================================================

export const DATE_RANGE_PICKER_TAG = 0x0315;
const DATE_RANGE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);

function renderDateRangePicker(component, ctx) {
  assertOnlyKnownFields(component.fields, DATE_RANGE_FIELD_KEYS, 'DateRangePicker');

  const fromPath = requirePath(ctx.readField(component.fields, 0), 'DateRangePicker.from_path');
  const toPath = requirePath(ctx.readField(component.fields, 1), 'DateRangePicker.to_path');
  const labelBind = ctx.readField(component.fields, 2);
  const minDateRaw = ctx.readField(component.fields, 3);
  const maxDateRaw = ctx.readField(component.fields, 4);
  const localeRaw = ctx.readField(component.fields, 5);
  const format = requireEnum(ctx.readField(component.fields, 6), DATE_STYLES, 'DateRangePicker.format');
  const fdow = requireEnum(ctx.readField(component.fields, 7), DAY_OF_WEEK, 'DateRangePicker.first_day_of_week');
  const disabledDatesRaw = ctx.readField(component.fields, 8);
  const presetsRaw = ctx.readField(component.fields, 9);
  const placeholderFromBind = ctx.readField(component.fields, 10);
  const placeholderToBind = ctx.readField(component.fields, 11);
  const maxRangeDaysRaw = ctx.readField(component.fields, 12);

  const minDate = minDateRaw == null ? null : requireIsoDate(minDateRaw, 'DateRangePicker.min_date');
  const maxDate = maxDateRaw == null ? null : requireIsoDate(maxDateRaw, 'DateRangePicker.max_date');
  if (minDate != null && maxDate != null && minDate > maxDate) {
    throw new TypeError('DateRangePicker.min_date must be <= max_date');
  }
  const locale = localeRaw == null ? null : requireString(localeRaw, 'DateRangePicker.locale');
  const disabledDates = disabledDatesRaw == null ? null : (() => {
    if (!Array.isArray(disabledDatesRaw)) throw new TypeError('DateRangePicker.disabled_dates: expected Array<string>');
    return disabledDatesRaw.map((d, i) => requireIsoDate(d, `DateRangePicker.disabled_dates[${i}]`));
  })();
  const disabledSet = disabledDates ? new Set(disabledDates) : null;
  const presets = presetsRaw == null ? null : (() => {
    if (!Array.isArray(presetsRaw)) throw new TypeError('DateRangePicker.presets: expected Array<RangePreset>');
    return presetsRaw.map((p, i) => parseRangePreset(p, `DateRangePicker.presets[${i}]`));
  })();
  const maxRangeDays = maxRangeDaysRaw == null ? null : requireU16(maxRangeDaysRaw, 'DateRangePicker.max_range_days');
  if (maxRangeDays != null && maxRangeDays === 0) {
    throw new TypeError('DateRangePicker.max_range_days must be > 0 if set');
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-daterange');
  wrapper.classList.add(`tf-daterange--format-${format}`);
  wrapper.setAttribute('data-first-day-of-week', fdow);

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('div');
    labelEl.classList.add('tf-daterange__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const fieldsRow = document.createElement('div');
  fieldsRow.classList.add('tf-daterange__fields');

  const makeInput = (id, bindPath, placeholderBind) => {
    const inp = document.createElement('input');
    inp.setAttribute('type', 'date');
    inp.classList.add('tf-daterange__input');
    inp.setAttribute('id', id);
    if (minDate) inp.setAttribute('min', minDate);
    if (maxDate) inp.setAttribute('max', maxDate);
    if (locale) inp.setAttribute('lang', locale);
    applyPlaceholderReactive(inp, placeholderBind, ctx);
    applyValueReactive(inp, bindPath, ctx, (s) => ISO_DATE_RE.test(s));
    return inp;
  };
  const fromInput = makeInput(`tf-daterange-${component.id}-from`, fromPath, placeholderFromBind);
  const toInput = makeInput(`tf-daterange-${component.id}-to`, toPath, placeholderToBind);
  const dash = document.createElement('span');
  dash.classList.add('tf-daterange__dash');
  dash.setAttribute('aria-hidden', 'true');
  dash.textContent = '–';
  fieldsRow.appendChild(fromInput);
  fieldsRow.appendChild(dash);
  fieldsRow.appendChild(toInput);

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('DateRangePicker without `label` requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('DateRangePicker.a11y.label must resolve to non-blank string');
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) {
        fromInput.setAttribute('aria-label', `${v} (from)`);
        toInput.setAttribute('aria-label', `${v} (to)`);
      } else {
        fromInput.removeAttribute('aria-label');
        toInput.removeAttribute('aria-label');
      }
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  wrapper.appendChild(fieldsRow);

  // lastFrom/lastTo śledzą każdą valid wartość — initial po sync,
  // preset + manual change.
  let lastFrom = fromInput.value && ISO_DATE_RE.test(fromInput.value) ? fromInput.value : '';
  let lastTo = toInput.value && ISO_DATE_RE.test(toInput.value) ? toInput.value : '';
  ctx.registerCleanup(ctx.store.subscribe(fromPath, () => {
    if (fromInput.value && ISO_DATE_RE.test(fromInput.value)) lastFrom = fromInput.value;
  }));
  ctx.registerCleanup(ctx.store.subscribe(toPath, () => {
    if (toInput.value && ISO_DATE_RE.test(toInput.value)) lastTo = toInput.value;
  }));

  if (presets) {
    const bar = document.createElement('div');
    bar.classList.add('tf-daterange__presets');
    for (const p of presets) {
      const btn = document.createElement('button');
      btn.setAttribute('type', 'button');
      btn.classList.add('tf-daterange__preset');
      btn.setAttribute('data-preset-id', p.id);
      applyTextBind(btn, p.label, ctx);
      const onClick = (e) => {
        e.preventDefault();
        const r = resolveRangePreset(p.range);
        // from > to: odrzucamy preset zamiast emitować niepoprawny range.
        if (r.from > r.to) return;
        if (minDate != null && (r.from < minDate || r.to < minDate)) return;
        if (maxDate != null && (r.from > maxDate || r.to > maxDate)) return;
        if (disabledSet && (disabledSet.has(r.from) || disabledSet.has(r.to))) return;
        if (maxRangeDays != null) {
          const span = daysBetween(r.from, r.to);
          if (span > maxRangeDays) return;
        }
        fromInput.value = r.from;
        toInput.value = r.to;
        lastFrom = r.from;
        lastTo = r.to;
        wrapper.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('change', {
            bubbles: false,
            detail: {
              value: { from: r.from, to: r.to },
              kind: 'range',
              preset_id: p.id,
            },
          })
        );
      };
      btn.addEventListener('click', onClick);
      ctx.registerCleanup(() => btn.removeEventListener('click', onClick));
      bar.appendChild(btn);
    }
    wrapper.appendChild(bar);
  }

  const validateAndEmit = (changedKind) => {
    const f = fromInput.value;
    const t = toInput.value;
    // Disabled date check.
    if ((f && disabledSet && disabledSet.has(f)) || (t && disabledSet && disabledSet.has(t))) {
      fromInput.value = lastFrom; toInput.value = lastTo;
      return;
    }
    // from <= to enforcement.
    if (f && t && f > t) {
      fromInput.value = lastFrom; toInput.value = lastTo;
      return;
    }
    // max_range_days enforcement.
    if (maxRangeDays != null && f && t) {
      if (daysBetween(f, t) > maxRangeDays) {
        fromInput.value = lastFrom; toInput.value = lastTo;
        return;
      }
    }
    lastFrom = f; lastTo = t;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: {
          value: { from: f || null, to: t || null },
          kind: 'range',
          changed: changedKind,
        },
      })
    );
  };
  const onFromChange = () => validateAndEmit('from');
  const onToChange = () => validateAndEmit('to');
  fromInput.addEventListener('change', onFromChange);
  toInput.addEventListener('change', onToChange);
  ctx.registerCleanup(() => {
    fromInput.removeEventListener('change', onFromChange);
    toInput.removeEventListener('change', onToChange);
  });

  return wrapper;
}

/// Liczba dni między dwoma ISO YYYY-MM-DD (inclusive both ends), zakładając
/// `from <= to`. Używane do max_range_days enforcement.
function daysBetween(fromIso, toIso) {
  const a = new Date(`${fromIso}T00:00:00Z`).getTime();
  const b = new Date(`${toIso}T00:00:00Z`).getTime();
  const ms = b - a;
  return Math.floor(ms / 86400000) + 1;
}

// =============================================================================
// TimePicker (0x0316)
// =============================================================================

export const TIME_PICKER_TAG = 0x0316;
const TIME_PICKER_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);
// ISO time HH:MM or HH:MM:SS.
const ISO_TIME_RE = /^\d{2}:\d{2}(:\d{2})?$/;

function renderTimePicker(component, ctx) {
  assertOnlyKnownFields(component.fields, TIME_PICKER_FIELD_KEYS, 'TimePicker');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'TimePicker.bind_path');
  const precision = requireEnum(ctx.readField(component.fields, 1), TIME_PRECISIONS, 'TimePicker.precision');
  const format = requireEnum(ctx.readField(component.fields, 2), TIME_STYLES, 'TimePicker.format');
  const stepMinutes = requireU16(ctx.readField(component.fields, 3), 'TimePicker.step_minutes');
  if (stepMinutes === 0) throw new TypeError('TimePicker.step_minutes must be > 0');
  const labelBind = ctx.readField(component.fields, 4);

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-timepicker');
  wrapper.classList.add(`tf-timepicker--format-${format}`);
  wrapper.classList.add(`tf-timepicker--precision-${precision}`);

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-timepicker__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const input = document.createElement('input');
  input.setAttribute('type', 'time');
  input.classList.add('tf-timepicker__input');
  const inputId = `tf-timepicker-${component.id}`;
  input.setAttribute('id', inputId);
  if (labelEl) labelEl.setAttribute('for', inputId);
  // step w sekundach. Dla precision=second min step 1s nawet gdy stepMinutes
  // by sugerował coarser; spec'owy stepMinutes=1 + precision=second to
  // step=60 sekund (czyli skok 1 min). Trzymamy się literalnego mnożenia.
  const stepSeconds = stepMinutes * 60;
  input.setAttribute('step', String(stepSeconds));

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('TimePicker without `label` requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('TimePicker.a11y.label must resolve to non-blank string');
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) input.setAttribute('aria-label', v);
      else input.removeAttribute('aria-label');
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  applyValueReactive(input, bindPath, ctx, (s) => ISO_TIME_RE.test(s));
  wrapper.appendChild(input);

  const onChange = () => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: input.value || null, kind: input.value ? 'tstr' : null },
      })
    );
  };
  input.addEventListener('change', onChange);
  ctx.registerCleanup(() => input.removeEventListener('change', onChange));

  return wrapper;
}

// =============================================================================
// DateTimePicker (0x0317)
// =============================================================================

export const DATE_TIME_PICKER_TAG = 0x0317;
const DATE_TIME_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);

function renderDateTimePicker(component, ctx) {
  assertOnlyKnownFields(component.fields, DATE_TIME_FIELD_KEYS, 'DateTimePicker');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'DateTimePicker.bind_path');
  const labelBind = ctx.readField(component.fields, 1);
  const minDtRaw = ctx.readField(component.fields, 2);
  const maxDtRaw = ctx.readField(component.fields, 3);
  const dateFormat = requireEnum(ctx.readField(component.fields, 4), DATE_STYLES, 'DateTimePicker.date_format');
  const timeFormat = requireEnum(ctx.readField(component.fields, 5), TIME_STYLES, 'DateTimePicker.time_format');
  const precision = requireEnum(ctx.readField(component.fields, 6), TIME_PRECISIONS, 'DateTimePicker.time_precision');
  const stepMinutes = requireU16(ctx.readField(component.fields, 7), 'DateTimePicker.step_minutes');
  if (stepMinutes === 0) throw new TypeError('DateTimePicker.step_minutes must be > 0');
  const localeRaw = ctx.readField(component.fields, 8);
  const fdow = requireEnum(ctx.readField(component.fields, 9), DAY_OF_WEEK, 'DateTimePicker.first_day_of_week');
  const placeholderBind = ctx.readField(component.fields, 10);
  const timezoneRaw = ctx.readField(component.fields, 11);

  const minDt = minDtRaw == null ? null : requireIsoLocalDt(minDtRaw, 'DateTimePicker.min_datetime');
  const maxDt = maxDtRaw == null ? null : requireIsoLocalDt(maxDtRaw, 'DateTimePicker.max_datetime');
  if (minDt != null && maxDt != null && minDt > maxDt) {
    throw new TypeError('DateTimePicker.min_datetime must be <= max_datetime');
  }
  const locale = localeRaw == null ? null : requireString(localeRaw, 'DateTimePicker.locale');
  const timezone = timezoneRaw == null ? null : requireTimezone(timezoneRaw, 'DateTimePicker.timezone');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-datetimepicker');
  wrapper.classList.add(`tf-datetimepicker--date-${dateFormat}`);
  wrapper.classList.add(`tf-datetimepicker--time-${timeFormat}`);
  wrapper.classList.add(`tf-datetimepicker--precision-${precision}`);
  wrapper.setAttribute('data-first-day-of-week', fdow);
  if (timezone) wrapper.setAttribute('data-timezone', timezone);

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-datetimepicker__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const input = document.createElement('input');
  input.setAttribute('type', 'datetime-local');
  input.classList.add('tf-datetimepicker__input');
  const inputId = `tf-datetimepicker-${component.id}`;
  input.setAttribute('id', inputId);
  if (labelEl) labelEl.setAttribute('for', inputId);
  if (minDt) input.setAttribute('min', minDt);
  if (maxDt) input.setAttribute('max', maxDt);
  if (locale) input.setAttribute('lang', locale);
  input.setAttribute('step', String(stepMinutes * 60));

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError('DateTimePicker without `label` requires Component.a11y.label');
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError('DateTimePicker.a11y.label must resolve to non-blank string');
    }
    const applyAria = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) input.setAttribute('aria-label', v);
      else input.removeAttribute('aria-label');
    };
    applyAria();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
  }

  applyPlaceholderReactive(input, placeholderBind, ctx);
  applyValueReactive(input, bindPath, ctx, (s) => ISO_LOCAL_DT_RE.test(s));
  wrapper.appendChild(input);

  const onChange = () => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: {
          value: input.value || null,
          kind: input.value ? 'tstr' : null,
          timezone,
        },
      })
    );
  };
  input.addEventListener('change', onChange);
  ctx.registerCleanup(() => input.removeEventListener('change', onChange));

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormDatetimeRenderers() {
  if (!lookupComponentRenderer(DATE_PICKER_TAG)) {
    registerComponentRenderer(DATE_PICKER_TAG, renderDatePicker);
  }
  if (!lookupComponentRenderer(DATE_RANGE_PICKER_TAG)) {
    registerComponentRenderer(DATE_RANGE_PICKER_TAG, renderDateRangePicker);
  }
  if (!lookupComponentRenderer(TIME_PICKER_TAG)) {
    registerComponentRenderer(TIME_PICKER_TAG, renderTimePicker);
  }
  if (!lookupComponentRenderer(DATE_TIME_PICKER_TAG)) {
    registerComponentRenderer(DATE_TIME_PICKER_TAG, renderDateTimePicker);
  }
}
