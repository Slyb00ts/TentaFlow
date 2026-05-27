// =============================================================================
// File: sdk-runtime/action-bars-renderer.js
// Description: Renderers for action bars:
//   - ActionBar       (0x0408) — leading + trailing Buttons with divider
//   - SegmentedControl(0x0409) — uses <tf-segmented> + <option> children
//   - FilterChips     (0x040A) — uses <tf-filter-chips> with .filters property
//   - WizardFooter    (0x040B) — back/next/cancel/skip + extra (Buttons)
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/actions/bars.rs.
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

const SEGMENT_OPTION_KEYS = new Set([0, 1, 2, 3]);
const FILTER_CHIP_DEF_KEYS = new Set([0, 1, 2, 3, 4]);
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
  if (parsed.tag === 'tstr')  return typeof storeValue === 'string' && storeValue === parsed.value;
  if (parsed.tag === 'bool')  return typeof storeValue === 'boolean' && storeValue === parsed.value;
  if (parsed.tag === 'u32' || parsed.tag === 'i32') {
    if (typeof storeValue === 'number' && storeValue === parsed.value) return true;
    if (typeof storeValue === 'bigint' && storeValue === BigInt(parsed.value)) return true;
  }
  return false;
}

// =============================================================================
// ActionBar (0x0408) — div wrapper, no tf-* equivalent
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
// SegmentedControl (0x0409) — uses <tf-segmented> with <option> children
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

  const seg = document.createElement('tf-segmented');
  seg.setAttribute('size', size);
  if (fullWidth) seg.style.width = '100%';

  const optionMeta = [];
  for (let i = 0; i < options.length; i++) {
    const opt = options[i];
    if (!Array.isArray(opt)) {
      throw new TypeError(`SegmentedControl.options[${i}] must be FieldMap`);
    }
    assertOnlyKnownFieldMapKeys(opt, SEGMENT_OPTION_KEYS, `SegmentedControl.options[${i}]`);
    const optBadge = ctx.readField(opt, 3);
    if (optBadge != null) {
      // Badge gracefully skipped
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
      if (optIcon.kind === 'named') {
        throw new TypeError(
          `SegmentedControl.options[${i}]: icon-only with named icon requires label (accessible name)`
        );
      }
      if (optIcon.kind === 'asset' && (typeof optIcon.alt !== 'string' || optIcon.alt.trim().length === 0)) {
        throw new TypeError(
          `SegmentedControl.options[${i}]: icon-only with asset icon requires non-blank alt`
        );
      }
    }

    // Build <option> child for tf-segmented
    const optEl = document.createElement('option');
    optEl.setAttribute('value', String(parsedValue.value));
    seg.appendChild(optEl);

    // Reactive label text on the option element
    if (optLabel != null) {
      const applyLabel = () => {
        const v = resolveBindRef(optLabel, ctx.store);
        optEl.textContent = v == null ? '' : String(v);
      };
      applyLabel();
      ctx.registerCleanup(subscribeBindRef(optLabel, ctx.store, applyLabel));
    } else {
      optEl.textContent = '';
    }

    optionMeta.push({ parsedValue });
  }

  // Set initial value from store
  const applySelection = () => {
    let current;
    try { current = ctx.store.read(bindPath); } catch { current = undefined; }
    for (const { parsedValue } of optionMeta) {
      if (selectValueEquals(parsedValue, current)) {
        seg.setAttribute('value', String(parsedValue.value));
        return;
      }
    }
    seg.setAttribute('value', '');
  };
  applySelection();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, applySelection));

  // Forward tf-segmented 'change' event to SDK handler with SelectValue detail
  const onChange = (e) => {
    const rawVal = e.detail && e.detail.value;
    // Find matching option to get the full SelectValue kind
    for (const { parsedValue } of optionMeta) {
      if (String(parsedValue.value) === String(rawVal)) {
        seg.dispatchEvent(
          new (globalThis.CustomEvent || globalThis.Event)('sdk-change', {
            bubbles: false,
            detail: { value: parsedValue.value, kind: parsedValue.tag },
          })
        );
        return;
      }
    }
  };
  seg.addEventListener('change', onChange);
  ctx.registerCleanup(() => seg.removeEventListener('change', onChange));

  return seg;
}

// =============================================================================
// FilterChips (0x040A) — uses <tf-filter-chips> with .filters property
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

  const el = document.createElement('tf-filter-chips');
  el.setAttribute('mode', mode);

  // Parse chip definitions
  const seenIds = new Set();
  const chipDefs = [];
  const labelBinds = [];
  const countPaths = [];
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
      // Badge gracefully skipped
    }
    const chipIcon = ctx.readField(chip, 2);
    const countPathRaw = ctx.readField(chip, 4);
    const countPath = countPathRaw == null
      ? null
      : requirePath(countPathRaw, `FilterChips.chips[${i}].count_path`);

    // Resolve initial label
    const initialLabel = resolveBindRef(chipLabel, ctx.store);
    const iconName = (chipIcon && chipIcon.kind === 'named') ? chipIcon.name : undefined;

    chipDefs.push({
      id: chipId,
      label: initialLabel == null ? '' : String(initialLabel),
      icon: iconName,
      count: null,
      active: false,
    });
    labelBinds.push(chipLabel);
    countPaths.push(countPath);
  }

  // Build and set filters
  const rebuildFilters = () => {
    let raw;
    try { raw = ctx.store.read(selectedIdsPath); } catch { raw = undefined; }
    const selectedSet = new Set();
    if (Array.isArray(raw)) {
      for (const id of raw) {
        if (typeof id === 'string') selectedSet.add(id);
      }
    }
    for (let i = 0; i < chipDefs.length; i++) {
      chipDefs[i].active = selectedSet.has(chipDefs[i].id);
      // Resolve current label
      const lbl = resolveBindRef(labelBinds[i], ctx.store);
      chipDefs[i].label = lbl == null ? '' : String(lbl);
      // Resolve count
      if (countPaths[i] != null) {
        try {
          const v = ctx.store.read(countPaths[i]);
          chipDefs[i].count = v == null ? null : v;
        } catch {
          chipDefs[i].count = null;
        }
      }
    }
    el.filters = chipDefs.map(d => ({ ...d }));
  };
  rebuildFilters();

  // Subscribe to selection path changes
  ctx.registerCleanup(ctx.store.subscribe(selectedIdsPath, rebuildFilters));
  // Subscribe to label binds
  for (let i = 0; i < labelBinds.length; i++) {
    ctx.registerCleanup(subscribeBindRef(labelBinds[i], ctx.store, rebuildFilters));
  }
  // Subscribe to count paths
  for (const cp of countPaths) {
    if (cp != null) {
      ctx.registerCleanup(ctx.store.subscribe(cp, rebuildFilters));
    }
  }

  // Forward tf-filter-chips change event with chip_id
  const onChange = (e) => {
    const chipId = e.detail && e.detail.id;
    if (chipId != null) {
      el.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('sdk-change', {
          bubbles: false,
          detail: { chip_id: chipId },
        })
      );
    }
  };
  el.addEventListener('change', onChange);
  ctx.registerCleanup(() => el.removeEventListener('change', onChange));

  return el;
}

// =============================================================================
// WizardFooter (0x040B) — div wrapper, no tf-* equivalent
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
// Registration
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
