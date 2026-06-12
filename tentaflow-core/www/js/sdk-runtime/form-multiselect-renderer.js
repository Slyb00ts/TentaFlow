// =============================================================================
// File: sdk-runtime/form-multiselect-renderer.js
// Description: MultiSelect (0x0304) renderer — renders through the
// <tf-multiselect> web component (chips trigger + multiselectable listbox
// popover, optional search/select-all/clear/max-selections handled by the
// component).
//
// selected_path in the store is an ARRAY of selected values (each element is
// either a SelectValue {kind, value} or a raw primitive). The renderer is
// read-only — toggle/clear emits 'change' with
// `{ value: SelectValue[], kind: 'array' }`; write-back is wired by chunk
// 3.6. Component option values carry SDK option indices so the change
// interceptor can map back to typed SelectValues.
//
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/form/selectors.rs` MultiSelect.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';
import { renderIcon } from './icon-renderer.js';

// =============================================================================
// Validators
// =============================================================================

const INPUT_SIZES = new Set(['sm', 'md', 'lg']);
const SELECT_VALUE_KINDS = new Set(['tstr', 'u32', 'i32', 'bool']);
const SELECT_VALUE_KEYS = new Set(['kind', 'value']);
const SELECT_OPTION_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const SELECT_GROUP_KEYS = new Set([0, 1]);

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
function requirePath(v, ctx) {
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath (Array<PathSegment>)`);
  return v;
}
function requireU32(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFFFFFn) {
      throw new TypeError(`${ctx}: expected u32, got ${v}`);
    }
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFFFFFF) {
    throw new TypeError(`${ctx}: expected u32, got ${v}`);
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
function assertOnlyKnownObjectKeys(obj, allowedKeys, ctx) {
  for (const k of Object.keys(obj)) {
    if (!allowedKeys.has(k)) throw new TypeError(`${ctx}: unexpected key '${k}'`);
  }
}

function parseSelectValue(sv, ctx) {
  if (!sv || typeof sv !== 'object') throw new TypeError(`${ctx}: SelectValue must be object`);
  assertOnlyKnownObjectKeys(sv, SELECT_VALUE_KEYS, ctx);
  if (!SELECT_VALUE_KINDS.has(sv.kind)) throw new TypeError(`${ctx}.kind unsupported: ${sv.kind}`);
  switch (sv.kind) {
    case 'tstr':
      if (typeof sv.value !== 'string') throw new TypeError(`${ctx}.value must be string`);
      return { tag: 'tstr', value: sv.value };
    case 'bool':
      if (typeof sv.value !== 'boolean') throw new TypeError(`${ctx}.value must be boolean`);
      return { tag: 'bool', value: sv.value };
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
  throw new TypeError(`${ctx}.kind unsupported`);
}

/// Compares a parsed SelectValue to a store value (number or bigint for ints).
function selectValueEquals(parsed, storeValue) {
  if (parsed.tag === 'tstr') return typeof storeValue === 'string' && storeValue === parsed.value;
  if (parsed.tag === 'bool') return typeof storeValue === 'boolean' && storeValue === parsed.value;
  if (parsed.tag === 'u32' || parsed.tag === 'i32') {
    if (typeof storeValue === 'number' && storeValue === parsed.value) return true;
    if (typeof storeValue === 'bigint' && storeValue === BigInt(parsed.value)) return true;
  }
  return false;
}

/// Checks whether a SelectValue is in the selected list (raw store array).
function isOptionSelected(parsed, storeArray) {
  if (!Array.isArray(storeArray)) return false;
  return storeArray.some((sv) => {
    // Accept both `SelectValue` (tagged object) and raw values — a capable
    // host may write either shape. Normalize to primitive comparison.
    if (sv && typeof sv === 'object' && 'kind' in sv && 'value' in sv) {
      return parsed.tag === sv.kind && selectValueEquals(parsed, sv.value);
    }
    return selectValueEquals(parsed, sv);
  });
}

function parseSelectOption(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: SelectOption must be FieldMap`);
  const seen = new Set();
  let value, label, icon = null, disabled = false, groupId = null, description = null;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!SELECT_OPTION_KEYS.has(k)) throw new TypeError(`${ctx}: unknown SelectOption key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: value = parseSelectValue(v, `${ctx}.value`); break;
      case 1: label = v; break;
      case 2: icon = v == null ? null : v; break;
      case 3: disabled = requireBool(v, `${ctx}.disabled`); break;
      case 4: if (v != null) {
        if (typeof v !== 'string') throw new TypeError(`${ctx}.group_id must be string`);
        groupId = v;
      } break;
      case 5: description = v == null ? null : v; break;
    }
  }
  if (value === undefined) throw new TypeError(`${ctx}: SelectOption.value required`);
  if (label === undefined) throw new TypeError(`${ctx}: SelectOption.label required`);
  return { value, label, icon, disabled, groupId, description };
}

function parseSelectGroup(raw, ctx) {
  if (!Array.isArray(raw)) throw new TypeError(`${ctx}: SelectGroup must be FieldMap`);
  const seen = new Set();
  let id, label;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) throw new TypeError(`${ctx}: entry [u8, Value]`);
    const [k, v] = entry;
    if (!SELECT_GROUP_KEYS.has(k)) throw new TypeError(`${ctx}: unknown SelectGroup key ${k}`);
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    if (k === 0) {
      if (typeof v !== 'string') throw new TypeError(`${ctx}.id must be string`);
      id = v;
    } else { label = v; }
  }
  if (id === undefined) throw new TypeError(`${ctx}: SelectGroup.id required`);
  if (label === undefined) throw new TypeError(`${ctx}: SelectGroup.label required`);
  return { id, label };
}

// =============================================================================
// MultiSelect (0x0304)
// =============================================================================

export const MULTISELECT_TAG = 0x0304;
const MULTISELECT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);

function renderMultiSelect(component, ctx) {
  assertOnlyKnownFields(component.fields, MULTISELECT_FIELD_KEYS, 'MultiSelect');

  const selectedPath = requirePath(
    ctx.readField(component.fields, 0), 'MultiSelect.selected_path'
  );
  const optionsRaw = ctx.readField(component.fields, 1);
  if (!Array.isArray(optionsRaw)) {
    throw new TypeError('MultiSelect.options: expected Array<SelectOption>');
  }
  const options = optionsRaw.map((o, i) => parseSelectOption(o, `MultiSelect.options[${i}]`));
  const placeholderBind = ctx.readField(component.fields, 2);
  const labelBind = ctx.readField(component.fields, 3);
  const searchable = requireBool(ctx.readField(component.fields, 4), 'MultiSelect.searchable');
  const clearable = requireBool(ctx.readField(component.fields, 5), 'MultiSelect.clearable');
  const virtualize = requireBool(ctx.readField(component.fields, 6), 'MultiSelect.virtualize');
  const disabledBind = ctx.readField(component.fields, 7);
  const size = requireEnum(ctx.readField(component.fields, 8), INPUT_SIZES, 'MultiSelect.size');
  const groupsRaw = ctx.readField(component.fields, 9);
  const groups = groupsRaw == null ? null : (() => {
    if (!Array.isArray(groupsRaw)) {
      throw new TypeError('MultiSelect.groups: expected Array<SelectGroup>');
    }
    return groupsRaw.map((g, i) => parseSelectGroup(g, `MultiSelect.groups[${i}]`));
  })();
  const groupById = new Map();
  if (groups) {
    for (const g of groups) groupById.set(g.id, g);
    for (let i = 0; i < options.length; i++) {
      const opt = options[i];
      if (opt.groupId != null && !groupById.has(opt.groupId)) {
        throw new TypeError(`MultiSelect.options[${i}].group_id '${opt.groupId}' not present in groups`);
      }
    }
  }
  const maxSelectionsRaw = ctx.readField(component.fields, 10);
  const maxSelections = maxSelectionsRaw == null ? null : requireU32(maxSelectionsRaw, 'MultiSelect.max_selections');
  if (maxSelections != null && maxSelections === 0) {
    throw new TypeError('MultiSelect.max_selections must be > 0 if set');
  }
  const showSelectAll = requireBool(ctx.readField(component.fields, 11), 'MultiSelect.show_select_all');

  const el = document.createElement('tf-multiselect');
  el.classList.add(`tf-multiselect--size-${size}`);
  if (virtualize) el.classList.add('tf-multiselect--virtualize');
  if (clearable) el.setAttribute('clearable', '');
  if (showSelectAll) el.setAttribute('select-all', '');
  if (maxSelections != null) el.setAttribute('max-selections', String(maxSelections));
  if (!searchable) el.setAttribute('no-search', '');

  // Label: visible label → `label` attribute; otherwise the required
  // a11y.label is mirrored as `aria-label` (the component copies it onto the
  // focusable trigger).
  if (labelBind != null) {
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      el.setAttribute('label', v == null ? '' : String(v));
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));
  } else {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'MultiSelect without `label` field requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'MultiSelect.a11y.label must resolve to non-blank string at initial render'
      );
    }
    const applyAriaLabel = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) el.setAttribute('aria-label', v);
      else el.removeAttribute('aria-label');
    };
    applyAriaLabel();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAriaLabel));
  }

  // Reactive placeholder.
  if (placeholderBind != null) {
    const applyPlaceholder = () => {
      const v = resolveBindRef(placeholderBind, ctx.store);
      if (v == null || v === '') el.removeAttribute('placeholder');
      else el.setAttribute('placeholder', String(v));
    };
    applyPlaceholder();
    ctx.registerCleanup(subscribeBindRef(placeholderBind, ctx.store, applyPlaceholder));
  }

  // Reactive disabled.
  let disabledActive = false;
  const isDisabledFn = (() => {
    if (disabledBind == null) return () => false;
    const apply = () => {
      disabledActive = resolveBindRef(disabledBind, ctx.store) === true;
      el.disabled = disabledActive;
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, apply));
    return () => disabledActive;
  })();

  // Options feed: component option.value carries the SDK option index so the
  // change interceptor maps back to typed SelectValues. Labels, descriptions
  // and group labels are BindRefs — re-feed the component on any change.
  const buildComponentOptions = () => options.map((opt, idx) => {
    const labelText = resolveBindRef(opt.label, ctx.store);
    const out = {
      value: idx,
      label: labelText == null ? '' : String(labelText),
      disabled: opt.disabled,
    };
    if (opt.description != null) {
      const d = resolveBindRef(opt.description, ctx.store);
      out.description = d == null ? '' : String(d);
    }
    if (opt.icon != null) {
      out.icon = renderIcon(opt.icon, `MultiSelect.options[${idx}].icon`);
    }
    if (opt.groupId != null) {
      const g = resolveBindRef(groupById.get(opt.groupId).label, ctx.store);
      out.group = g == null ? '' : String(g);
    }
    return out;
  });
  const refreshOptions = () => { el.options = buildComponentOptions(); };
  refreshOptions();
  for (const opt of options) {
    ctx.registerCleanup(subscribeBindRef(opt.label, ctx.store, refreshOptions));
    if (opt.description != null) {
      ctx.registerCleanup(subscribeBindRef(opt.description, ctx.store, refreshOptions));
    }
  }
  if (groups) {
    for (const g of groups) {
      ctx.registerCleanup(subscribeBindRef(g.label, ctx.store, refreshOptions));
    }
  }

  // Store → component selection (the store is the source of truth).
  const readSelected = () => {
    let arr;
    try { arr = ctx.store.read(selectedPath); } catch { arr = undefined; }
    return Array.isArray(arr) ? arr : [];
  };
  const selectedIndices = () => {
    const sel = readSelected();
    if (sel.length === 0) return [];
    const out = [];
    for (let i = 0; i < options.length; i++) {
      if (isOptionSelected(options[i].value, sel)) out.push(i);
    }
    return out;
  };
  const syncFromStore = () => { el.value = selectedIndices(); };
  syncFromStore();
  ctx.registerCleanup(ctx.store.subscribe(selectedPath, syncFromStore));

  // tf-multiselect 'change' carries {value: number[]} (option indices).
  // Convert to the SDK `{ value: SelectValue[], kind: 'array' }` shape —
  // the raw event is blocked and a single synthetic event tagged
  // `__tfReemit` carries the converted payload to the dispatcher.
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (!e.detail || !Array.isArray(e.detail.value)) return;
    if (isDisabledFn()) {
      // The component is read-only while disabled — discard the optimistic
      // component-side mutation and restore the store selection.
      syncFromStore();
      return;
    }
    const detailArr = [];
    for (const idx of e.detail.value) {
      const opt = options[idx];
      if (!opt) return;
      detailArr.push({ kind: opt.value.tag, value: opt.value.value });
    }
    const ce = new CustomEvent('change', {
      bubbles: false,
      detail: { value: detailArr, kind: 'array' },
    });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  el.addEventListener('change', onChange);
  ctx.registerCleanup(() => el.removeEventListener('change', onChange));

  return el;
}

// =============================================================================
// Registration
// =============================================================================

export function registerFormMultiSelectRenderers() {
  if (!lookupComponentRenderer(MULTISELECT_TAG)) {
    registerComponentRenderer(MULTISELECT_TAG, renderMultiSelect);
  }
}
