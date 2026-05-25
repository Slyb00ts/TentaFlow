// =============================================================================
// Plik: sdk-runtime/action-bars-renderer.js
// Opis: Rendererzy bar'ów akcji — Faza 6 Krok 3.3b-6:
//   - ActionBar       (0x0408) — leading + trailing Buttons z dividerem
//   - SegmentedControl(0x0409) — toggle z multi-option
//   - FilterChips     (0x040A) — wybór multi/single chip'ów z liczbami
//   - WizardFooter    (0x040B) — back/next/cancel/skip + extra (Buttons)
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/actions/bars.rs`.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';
import { BUTTON_TAG } from './action-button-renderer.js';

const SEGMENT_SIZES = new Set(['sm', 'md', 'lg']);
const FILTER_CHIPS_MODES = new Set(['single', 'multi']);

// SegmentOption: 0=value(SelectValue), 1=label(BindRef), 2=icon(IconRef), 3=badge(InlineBadge)
const SEGMENT_OPTION_KEYS = new Set([0, 1, 2, 3]);
// FilterChipDef: 0=id, 1=label(BindRef), 2=icon(IconRef), 3=badge(InlineBadge), 4=count_path(StatePath)
const FILTER_CHIP_DEF_KEYS = new Set([0, 1, 2, 3, 4]);
const SELECT_VALUE_KEYS = new Set(['kind', 'value']);
// Wire shape per spec `inline.rs` SelectValue::encode — kindy są `tstr`,
// `u32`, `i32`, `bool`. NIE `uint`/`int`.
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
function requireArray(v, ctx) {
  if (!Array.isArray(v)) {
    throw new TypeError(`${ctx}: expected Array, got ${typeof v}`);
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
function assertOnlyKnownFieldMapKeys(fields, allowedKeys, ctx) {
  if (!Array.isArray(fields)) throw new TypeError(`${ctx}: expected FieldMap`);
  for (const entry of fields) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry must be [u8, Value]`);
    if (!allowedKeys.has(entry[0])) {
      throw new TypeError(`${ctx}: unexpected key ${entry[0]}`);
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
function assertButtonChild(c, ctx) {
  if (!c || typeof c !== 'object' || c.tag !== BUTTON_TAG) {
    throw new TypeError(
      `${ctx} must be Button (tag 0x0401), got 0x${
        (c && c.tag != null ? c.tag : 0).toString(16).padStart(4, '0')
      }`
    );
  }
}

/// Parsuje `SelectValue` (tagged union) do prymitywu JS przydatnego do
/// porównań equality. Mirror Rust `SelectValue::{Text,UInt,Int,Bool}`.
function parseSelectValue(sv, ctx) {
  if (!sv || typeof sv !== 'object') {
    throw new TypeError(`${ctx}: SelectValue must be object`);
  }
  assertOnlyKnownObjectKeys(sv, SELECT_VALUE_KEYS, ctx);
  if (!SELECT_VALUE_KINDS.has(sv.kind)) {
    throw new TypeError(`${ctx}.kind unsupported: ${sv.kind}`);
  }
  switch (sv.kind) {
    case 'tstr':
      return { tag: 'tstr', value: requireString(sv.value, `${ctx}.value`) };
    case 'bool':
      return { tag: 'bool', value: requireBool(sv.value, `${ctx}.value`) };
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
  // Porównanie heterogeniczne — addon-side store może trzymać liczbę,
  // string lub bool. Bool i numeryczne porównywane są jako JS typeof,
  // string jako prosta równość.
  if (parsed.tag === 'tstr')  return typeof storeValue === 'string' && storeValue === parsed.value;
  if (parsed.tag === 'bool')  return typeof storeValue === 'boolean' && storeValue === parsed.value;
  if (parsed.tag === 'u32' || parsed.tag === 'i32') {
    if (typeof storeValue === 'number' && storeValue === parsed.value) return true;
    if (typeof storeValue === 'bigint' && storeValue === BigInt(parsed.value)) return true;
  }
  return false;
}

// =============================================================================
// ActionBar (0x0408)
// =============================================================================

export const ACTION_BAR_TAG = 0x0408;
const ACTION_BAR_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderActionBar(component, ctx) {
  assertOnlyKnownFields(component.fields, ACTION_BAR_FIELD_KEYS, 'ActionBar');
  const leadingRaw = ctx.readField(component.fields, 0);
  if (leadingRaw === undefined) {
    throw new TypeError('ActionBar.leading_actions is required');
  }
  const leading = requireArray(leadingRaw, 'ActionBar.leading_actions');
  const trailingRaw = ctx.readField(component.fields, 1);
  if (trailingRaw === undefined) {
    throw new TypeError('ActionBar.trailing_actions is required');
  }
  const trailing = requireArray(trailingRaw, 'ActionBar.trailing_actions');
  const dividerBetweenRaw = ctx.readField(component.fields, 2);
  if (dividerBetweenRaw === undefined) {
    throw new TypeError('ActionBar.divider_between is required');
  }
  const dividerBetween = requireBool(dividerBetweenRaw, 'ActionBar.divider_between');
  const stickyRaw = ctx.readField(component.fields, 3);
  if (stickyRaw === undefined) {
    throw new TypeError('ActionBar.sticky is required');
  }
  const sticky = requireBool(stickyRaw, 'ActionBar.sticky');

  for (let i = 0; i < leading.length; i++) {
    assertButtonChild(leading[i], `ActionBar.leading_actions[${i}]`);
  }
  for (let i = 0; i < trailing.length; i++) {
    assertButtonChild(trailing[i], `ActionBar.trailing_actions[${i}]`);
  }

  const bar = document.createElement('div');
  bar.classList.add('tf-action-bar');
  if (sticky) bar.classList.add('tf-action-bar--sticky');
  bar.setAttribute('role', 'toolbar');

  const leadingEl = document.createElement('div');
  leadingEl.classList.add('tf-action-bar__leading');
  for (const b of leading) leadingEl.appendChild(ctx.renderChild(b));
  bar.appendChild(leadingEl);

  if (dividerBetween) {
    const div = document.createElement('div');
    div.classList.add('tf-action-bar__divider');
    div.setAttribute('aria-hidden', 'true');
    bar.appendChild(div);
  }

  const trailingEl = document.createElement('div');
  trailingEl.classList.add('tf-action-bar__trailing');
  for (const b of trailing) trailingEl.appendChild(ctx.renderChild(b));
  bar.appendChild(trailingEl);

  return bar;
}

// =============================================================================
// SegmentedControl (0x0409)
// =============================================================================

export const SEGMENTED_CONTROL_TAG = 0x0409;
const SEGMENTED_CONTROL_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderSegmentedControl(component, ctx) {
  assertOnlyKnownFields(component.fields, SEGMENTED_CONTROL_FIELD_KEYS, 'SegmentedControl');
  const bindPath = requirePath(
    ctx.readField(component.fields, 0),
    'SegmentedControl.bind_path'
  );
  const optionsRaw = ctx.readField(component.fields, 1);
  if (optionsRaw === undefined) {
    throw new TypeError('SegmentedControl.options is required');
  }
  const options = requireArray(optionsRaw, 'SegmentedControl.options');
  if (options.length === 0) {
    throw new TypeError('SegmentedControl.options must be non-empty');
  }
  const size = requireEnum(
    ctx.readField(component.fields, 2),
    SEGMENT_SIZES,
    'SegmentedControl.size'
  );
  const fullWidthRaw = ctx.readField(component.fields, 3);
  if (fullWidthRaw === undefined) {
    throw new TypeError('SegmentedControl.full_width is required');
  }
  const fullWidth = requireBool(fullWidthRaw, 'SegmentedControl.full_width');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-segmented');
  wrapper.classList.add(`tf-segmented--size-${size}`);
  if (fullWidth) wrapper.classList.add('tf-segmented--full-width');
  wrapper.setAttribute('role', 'radiogroup');

  // SegmentOption: 0=value(SelectValue), 1=label(BindRef), 2=icon(IconRef), 3=badge(InlineBadge)
  const optionMeta = [];
  for (let i = 0; i < options.length; i++) {
    const opt = options[i];
    if (!Array.isArray(opt)) {
      throw new TypeError(`SegmentedControl.options[${i}] must be FieldMap`);
    }
    assertOnlyKnownFieldMapKeys(opt, SEGMENT_OPTION_KEYS, `SegmentedControl.options[${i}]`);
    const optBadge = ctx.readField(opt, 3);
    if (optBadge != null) {
      // Icon/badge gracefully skipped
    }
    const optValue = ctx.readField(opt, 0);
    if (optValue == null) {
      throw new TypeError(`SegmentedControl.options[${i}].value is required`);
    }
    const parsedValue = parseSelectValue(optValue, `SegmentedControl.options[${i}].value`);
    const optLabel = ctx.readField(opt, 1);
    const optIcon = ctx.readField(opt, 2);
    if (optLabel == null && optIcon == null) {
      throw new TypeError(
        `SegmentedControl.options[${i}]: at least one of label / icon required`
      );
    }
    if (optLabel == null) {
      // Icon-only segment: named icon ma aria-hidden=true, więc bez
      // label'a button radio nie ma accessible name. Spec dopuszcza
      // icon-only TYLKO gdy icon to asset z `alt`. Tu odrzucamy named
      // icon bez label'a — addon musi dodać label albo zaspecować asset
      // icon z `alt`.
      if (optIcon.kind === 'named') {
        throw new TypeError(
          `SegmentedControl.options[${i}]: icon-only with named icon requires label (accessible name)`
        );
      }
      // Asset icon bez `alt` → renderIcon ustawi aria-hidden, segment też
      // anonymous. Wymagamy alt.
      if (optIcon.kind === 'asset' && (typeof optIcon.alt !== 'string' || optIcon.alt.trim().length === 0)) {
        throw new TypeError(
          `SegmentedControl.options[${i}]: icon-only with asset icon requires non-blank alt`
        );
      }
    }
    const btn = document.createElement('button');
    btn.setAttribute('type', 'button');
    btn.setAttribute('role', 'radio');
    btn.classList.add('tf-segmented__option');

    if (optIcon != null) {
      const iconEl = renderIcon(optIcon, `SegmentedControl.options[${i}].icon`);
      iconEl.classList.add('tf-segmented__option-icon');
      btn.appendChild(iconEl);
    }
    if (optLabel != null) {
      const labelEl = document.createElement('span');
      labelEl.classList.add('tf-segmented__option-label');
      btn.appendChild(labelEl);
      const apply = () => {
        const v = resolveBindRef(optLabel, ctx.store);
        labelEl.textContent = v == null ? '' : String(v);
      };
      apply();
      ctx.registerCleanup(subscribeBindRef(optLabel, ctx.store, apply));
    }

    const onClick = () => {
      // Klik dispatchuje 'change' z detail.value = wartość prymitywna.
      // Addon obsługuje to przez handler `change` i wpisuje do store'a
      // (chunk 3.6 wprowadzi automatic two-way write-back).
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('change', {
          bubbles: false,
          detail: { value: parsedValue.value, kind: parsedValue.tag },
        })
      );
    };
    btn.addEventListener('click', onClick);
    ctx.registerCleanup(() => btn.removeEventListener('click', onClick));
    wrapper.appendChild(btn);
    optionMeta.push({ btn, parsedValue });
  }

  // Reactive selection: czytamy current store value pod bind_path i
  // zaznaczamy odpowiedni segment.
  const applySelection = () => {
    let current;
    try {
      current = ctx.store.read(bindPath);
    } catch {
      current = undefined;
    }
    for (const { btn, parsedValue } of optionMeta) {
      const selected = selectValueEquals(parsedValue, current);
      btn.setAttribute('aria-checked', selected ? 'true' : 'false');
      btn.setAttribute('tabindex', selected ? '0' : '-1');
      if (selected) {
        btn.classList.add('tf-segmented__option--selected');
      } else {
        btn.classList.remove('tf-segmented__option--selected');
      }
    }
  };
  applySelection();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, applySelection));

  return wrapper;
}

// =============================================================================
// FilterChips (0x040A)
// =============================================================================

export const FILTER_CHIPS_TAG = 0x040A;
const FILTER_CHIPS_FIELD_KEYS = new Set([0, 1, 2, 3]);

function renderFilterChips(component, ctx) {
  assertOnlyKnownFields(component.fields, FILTER_CHIPS_FIELD_KEYS, 'FilterChips');
  const chipsRaw = ctx.readField(component.fields, 0);
  if (chipsRaw === undefined) {
    throw new TypeError('FilterChips.chips is required');
  }
  const chips = requireArray(chipsRaw, 'FilterChips.chips');
  const selectedIdsPath = requirePath(
    ctx.readField(component.fields, 1),
    'FilterChips.selected_ids'
  );
  const mode = requireEnum(
    ctx.readField(component.fields, 2),
    FILTER_CHIPS_MODES,
    'FilterChips.mode'
  );
  const clearableRaw = ctx.readField(component.fields, 3);
  if (clearableRaw === undefined) {
    throw new TypeError('FilterChips.clearable is required');
  }
  const clearable = requireBool(clearableRaw, 'FilterChips.clearable');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-filter-chips');
  wrapper.classList.add(`tf-filter-chips--mode-${mode}`);
  wrapper.setAttribute('role', mode === 'single' ? 'radiogroup' : 'group');

  // FilterChipDef: 0=id, 1=label(BindRef), 2=icon(IconRef), 3=badge(InlineBadge), 4=count_path(StatePath)
  const seenIds = new Set();
  const chipMeta = [];
  for (let i = 0; i < chips.length; i++) {
    const chip = chips[i];
    if (!Array.isArray(chip)) {
      throw new TypeError(`FilterChips.chips[${i}] must be FieldMap`);
    }
    assertOnlyKnownFieldMapKeys(chip, FILTER_CHIP_DEF_KEYS, `FilterChips.chips[${i}]`);
    const chipId = requireString(ctx.readField(chip, 0), `FilterChips.chips[${i}].id`);
    if (chipId.length === 0) {
      throw new TypeError(`FilterChips.chips[${i}].id must be non-empty`);
    }
    if (seenIds.has(chipId)) {
      throw new TypeError(`FilterChips.chips: duplicate id '${chipId}'`);
    }
    seenIds.add(chipId);
    const chipLabel = ctx.readField(chip, 1);
    if (chipLabel == null) {
      throw new TypeError(`FilterChips.chips[${i}].label must be BindRef`);
    }
    const chipBadge = ctx.readField(chip, 3);
    if (chipBadge != null) {
      // Icon/badge gracefully skipped
    }
    const countPathRaw = ctx.readField(chip, 4);
    const countPath = countPathRaw == null
      ? null
      : requirePath(countPathRaw, `FilterChips.chips[${i}].count_path`);

    const btn = document.createElement('button');
    btn.setAttribute('type', 'button');
    btn.setAttribute('role', mode === 'single' ? 'radio' : 'checkbox');
    btn.classList.add('tf-filter-chips__chip');
    btn.setAttribute('data-chip-id', chipId);

    const chipIcon = ctx.readField(chip, 2);
    if (chipIcon != null) {
      const iconEl = renderIcon(chipIcon, `FilterChips.chips[${i}].icon`);
      iconEl.classList.add('tf-filter-chips__chip-icon');
      btn.appendChild(iconEl);
    }
    const labelEl = document.createElement('span');
    labelEl.classList.add('tf-filter-chips__chip-label');
    btn.appendChild(labelEl);
    const applyLabel = () => {
      const v = resolveBindRef(chipLabel, ctx.store);
      labelEl.textContent = v == null ? '' : String(v);
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(chipLabel, ctx.store, applyLabel));

    // Optional count_path — reactive number obok label'u.
    if (countPath != null) {
      const countEl = document.createElement('span');
      countEl.classList.add('tf-filter-chips__chip-count');
      btn.appendChild(countEl);
      const applyCount = () => {
        const v = ctx.store.read(countPath);
        if (v == null) countEl.textContent = '';
        else countEl.textContent = String(v);
      };
      applyCount();
      ctx.registerCleanup(ctx.store.subscribe(countPath, applyCount));
    }

    const onClick = () => {
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('change', {
          bubbles: false,
          detail: { chip_id: chipId },
        })
      );
    };
    btn.addEventListener('click', onClick);
    ctx.registerCleanup(() => btn.removeEventListener('click', onClick));
    wrapper.appendChild(btn);
    chipMeta.push({ chipId, btn });
  }

  if (clearable) {
    const clearBtn = document.createElement('button');
    clearBtn.setAttribute('type', 'button');
    clearBtn.classList.add('tf-filter-chips__clear');
    clearBtn.setAttribute('aria-label', 'Wyczyść filtry');
    clearBtn.textContent = '×';
    const onClear = () => {
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('clear', {
          bubbles: false,
          detail: {},
        })
      );
    };
    clearBtn.addEventListener('click', onClear);
    ctx.registerCleanup(() => clearBtn.removeEventListener('click', onClear));
    wrapper.appendChild(clearBtn);
  }

  // Reactive selection — selected_ids store value powinno być array string'ów.
  const applySelection = () => {
    let raw;
    try { raw = ctx.store.read(selectedIdsPath); } catch { raw = undefined; }
    const selectedSet = new Set();
    if (Array.isArray(raw)) {
      for (const id of raw) {
        if (typeof id === 'string') selectedSet.add(id);
      }
    }
    if (mode === 'single' && selectedSet.size > 1) {
      // eslint-disable-next-line no-console
      console.warn(
        `[filter-chips] mode='single' but selected_ids has ${selectedSet.size} entries`
      );
    }
    for (const { chipId, btn } of chipMeta) {
      const selected = selectedSet.has(chipId);
      // ARIA: zarówno role=radio (single) jak i role=checkbox (multi)
      // używają `aria-checked`. `aria-pressed` jest zarezerwowane dla
      // role=button — łamałoby to ARIA contract na checkbox/radio.
      btn.setAttribute('aria-checked', selected ? 'true' : 'false');
      if (selected) {
        btn.classList.add('tf-filter-chips__chip--selected');
      } else {
        btn.classList.remove('tf-filter-chips__chip--selected');
      }
    }
  };
  applySelection();
  ctx.registerCleanup(ctx.store.subscribe(selectedIdsPath, applySelection));

  return wrapper;
}

// =============================================================================
// WizardFooter (0x040B)
// =============================================================================

export const WIZARD_FOOTER_TAG = 0x040B;
const WIZARD_FOOTER_FIELD_KEYS = new Set([0, 1, 2, 3, 4]);

function renderWizardFooter(component, ctx) {
  assertOnlyKnownFields(component.fields, WIZARD_FOOTER_FIELD_KEYS, 'WizardFooter');
  const backRaw = ctx.readField(component.fields, 0);
  const nextRaw = ctx.readField(component.fields, 1);
  const cancelRaw = ctx.readField(component.fields, 2);
  const skipRaw = ctx.readField(component.fields, 3);
  const extraRaw = ctx.readField(component.fields, 4);
  if (extraRaw === undefined) {
    throw new TypeError('WizardFooter.extra_actions is required');
  }
  const extra = requireArray(extraRaw, 'WizardFooter.extra_actions');

  if (backRaw != null)   assertButtonChild(backRaw, 'WizardFooter.back_action');
  if (nextRaw != null)   assertButtonChild(nextRaw, 'WizardFooter.next_action');
  if (cancelRaw != null) assertButtonChild(cancelRaw, 'WizardFooter.cancel_action');
  if (skipRaw != null)   assertButtonChild(skipRaw, 'WizardFooter.skip_action');
  for (let i = 0; i < extra.length; i++) {
    assertButtonChild(extra[i], `WizardFooter.extra_actions[${i}]`);
  }

  const footer = document.createElement('div');
  footer.classList.add('tf-wizard-footer');
  footer.setAttribute('role', 'toolbar');

  const leftSlot = document.createElement('div');
  leftSlot.classList.add('tf-wizard-footer__left');
  footer.appendChild(leftSlot);
  if (backRaw != null) {
    const el = ctx.renderChild(backRaw);
    el.classList.add('tf-wizard-footer__back');
    leftSlot.appendChild(el);
  }
  if (cancelRaw != null) {
    const el = ctx.renderChild(cancelRaw);
    el.classList.add('tf-wizard-footer__cancel');
    leftSlot.appendChild(el);
  }

  const centerSlot = document.createElement('div');
  centerSlot.classList.add('tf-wizard-footer__center');
  footer.appendChild(centerSlot);
  for (const b of extra) {
    centerSlot.appendChild(ctx.renderChild(b));
  }

  const rightSlot = document.createElement('div');
  rightSlot.classList.add('tf-wizard-footer__right');
  footer.appendChild(rightSlot);
  if (skipRaw != null) {
    const el = ctx.renderChild(skipRaw);
    el.classList.add('tf-wizard-footer__skip');
    rightSlot.appendChild(el);
  }
  if (nextRaw != null) {
    const el = ctx.renderChild(nextRaw);
    el.classList.add('tf-wizard-footer__next');
    rightSlot.appendChild(el);
  }
  return footer;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerActionBarsRenderers() {
  if (!lookupComponentRenderer(ACTION_BAR_TAG)) {
    registerComponentRenderer(ACTION_BAR_TAG, renderActionBar);
  }
  if (!lookupComponentRenderer(SEGMENTED_CONTROL_TAG)) {
    registerComponentRenderer(SEGMENTED_CONTROL_TAG, renderSegmentedControl);
  }
  if (!lookupComponentRenderer(FILTER_CHIPS_TAG)) {
    registerComponentRenderer(FILTER_CHIPS_TAG, renderFilterChips);
  }
  if (!lookupComponentRenderer(WIZARD_FOOTER_TAG)) {
    registerComponentRenderer(WIZARD_FOOTER_TAG, renderWizardFooter);
  }
}
