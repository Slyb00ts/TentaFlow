// =============================================================================
// Plik: sdk-runtime/form-select-renderer.js
// Opis: Renderer Select (0x0303) uzywajacy tf-select web component z natywnymi
// <option> children. Wartosc pisemna CZYTANA ze store (read-only) — wybor opcji
// dispatchuje `change` z `{ value, kind }` SelectValue dla write-back (chunk 3.6).
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/form/selectors.rs Select.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef } from './bind-resolver.js';

// =============================================================================
// Walidatory
// =============================================================================

const INPUT_SIZES = new Set(['sm', 'md', 'lg']);
const SELECT_VALUE_KINDS = new Set(['tstr', 'u32', 'i32', 'bool']);
const SELECT_VALUE_KEYS = new Set(['kind', 'value']);
const SELECT_OPTION_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const SELECT_GROUP_KEYS = new Set([0, 1]);

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
    case 'tstr':
      if (typeof sv.value !== 'string') throw new TypeError(`${ctx}.value must be string`);
      return { tag: 'tstr', value: sv.value };
    case 'bool':
      if (typeof sv.value !== 'boolean') throw new TypeError(`${ctx}.value must be boolean`);
      return { tag: 'bool', value: sv.value };
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
  throw new TypeError(`${ctx}.kind unsupported`);
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

function parseSelectOption(raw, ctx) {
  if (!Array.isArray(raw)) {
    throw new TypeError(`${ctx}: SelectOption must be FieldMap (Array<[u8, Value]>)`);
  }
  const seen = new Set();
  let value, label, icon = null, disabled = false, groupId = null, description = null;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) {
      throw new TypeError(`${ctx}: entry must be [u8, Value]`);
    }
    const [k, v] = entry;
    if (!SELECT_OPTION_KEYS.has(k)) {
      throw new TypeError(`${ctx}: unknown SelectOption key ${k}`);
    }
    if (seen.has(k)) throw new TypeError(`${ctx}: duplicate key ${k}`);
    seen.add(k);
    switch (k) {
      case 0: value = parseSelectValue(v, `${ctx}.value`); break;
      case 1: label = v; break;
      case 2: icon = v == null ? null : v; break;
      case 3: disabled = requireBool(v, `${ctx}.disabled`); break;
      case 4: if (v != null) { if (typeof v !== 'string') throw new TypeError(`${ctx}.group_id must be string`); groupId = v; } break;
      case 5: description = v == null ? null : v; break;
    }
  }
  if (value === undefined) throw new TypeError(`${ctx}: SelectOption.value required`);
  if (label === undefined) throw new TypeError(`${ctx}: SelectOption.label required`);
  return { value, label, icon, disabled, groupId, description };
}

function parseSelectGroup(raw, ctx) {
  if (!Array.isArray(raw)) {
    throw new TypeError(`${ctx}: SelectGroup must be FieldMap`);
  }
  const seen = new Set();
  let id, label;
  for (const entry of raw) {
    if (!Array.isArray(entry) || entry.length !== 2) {
      throw new TypeError(`${ctx}: entry must be [u8, Value]`);
    }
    const [k, v] = entry;
    if (!SELECT_GROUP_KEYS.has(k)) {
      throw new TypeError(`${ctx}: unknown SelectGroup key ${k}`);
    }
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
// Select (0x0303)
// =============================================================================

export const SELECT_TAG = 0x0303;
const SELECT_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);

function renderSelect(component, ctx) {
  assertOnlyKnownFields(component.fields, SELECT_FIELD_KEYS, 'Select');

  const bindPath = requirePath(
    ctx.readField(component.fields, 0), 'Select.bind_path'
  );
  const optionsRaw = ctx.readField(component.fields, 1);
  if (!Array.isArray(optionsRaw)) {
    throw new TypeError('Select.options: expected Array<SelectOption>');
  }
  const options = optionsRaw.map((o, i) => parseSelectOption(o, `Select.options[${i}]`));
  const placeholderBind = ctx.readField(component.fields, 2);
  const labelBind = ctx.readField(component.fields, 3);
  const searchable = requireBool(
    ctx.readField(component.fields, 4), 'Select.searchable'
  );
  const clearable = requireBool(
    ctx.readField(component.fields, 5), 'Select.clearable'
  );
  const virtualize = requireBool(
    ctx.readField(component.fields, 6), 'Select.virtualize'
  );
  const disabledBind = ctx.readField(component.fields, 7);
  const size = requireEnum(
    ctx.readField(component.fields, 8), INPUT_SIZES, 'Select.size'
  );
  const groupsRaw = ctx.readField(component.fields, 9);
  const groups = groupsRaw == null
    ? null
    : (() => {
      if (!Array.isArray(groupsRaw)) {
        throw new TypeError('Select.groups: expected Array<SelectGroup>');
      }
      return groupsRaw.map((g, i) => parseSelectGroup(g, `Select.groups[${i}]`));
    })();
  if (groups) {
    const ids = new Set(groups.map((g) => g.id));
    for (let i = 0; i < options.length; i++) {
      const opt = options[i];
      if (opt.groupId != null && !ids.has(opt.groupId)) {
        throw new TypeError(
          `Select.options[${i}].group_id '${opt.groupId}' nie ma w Select.groups`
        );
      }
    }
  }

  // Serialize SelectValue to string for native <option> value attribute.
  // SelectValue tag types are encoded as prefix to avoid collisions.
  const serializeValue = (parsed) => `${parsed.tag}:${String(parsed.value)}`;

  // Build value-to-option lookup for change event dispatch.
  const valueMap = new Map();
  for (const opt of options) {
    valueMap.set(serializeValue(opt.value), opt);
  }

  const el = document.createElement('tf-select');
  el.classList.add(`tf-select--size-${size}`);

  // Build <option> children. tf-select picks these up in connectedCallback.
  // Placeholder option (empty value, selected when no value in store).
  if (placeholderBind != null) {
    const phOpt = document.createElement('option');
    phOpt.value = '';
    phOpt.disabled = true;
    const phText = resolveBindRef(placeholderBind, ctx.store);
    phOpt.textContent = phText == null ? '' : String(phText);
    el.appendChild(phOpt);
  }

  if (groups) {
    const groupMap = new Map();
    for (const g of groups) {
      const optgroup = document.createElement('optgroup');
      const labelText = resolveBindRef(g.label, ctx.store);
      optgroup.label = labelText == null ? '' : String(labelText);
      groupMap.set(g.id, optgroup);
      el.appendChild(optgroup);
    }
    for (const opt of options) {
      const optEl = document.createElement('option');
      optEl.value = serializeValue(opt.value);
      const labelText = resolveBindRef(opt.label, ctx.store);
      optEl.textContent = labelText == null ? '' : String(labelText);
      if (opt.disabled) optEl.disabled = true;
      const container = opt.groupId != null ? groupMap.get(opt.groupId) : el;
      container.appendChild(optEl);
    }
  } else {
    for (const opt of options) {
      const optEl = document.createElement('option');
      optEl.value = serializeValue(opt.value);
      const labelText = resolveBindRef(opt.label, ctx.store);
      optEl.textContent = labelText == null ? '' : String(labelText);
      if (opt.disabled) optEl.disabled = true;
      el.appendChild(optEl);
    }
  }

  // Reactive value: sync store -> tf-select.value
  const syncValue = () => {
    let current;
    try { current = ctx.store.read(bindPath); } catch { current = undefined; }
    if (current === undefined || current === null) {
      el.value = '';
      return;
    }
    // Find option matching current store value.
    for (const opt of options) {
      if (selectValueEquals(opt.value, current)) {
        el.value = serializeValue(opt.value);
        return;
      }
    }
    el.value = '';
  };
  syncValue();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, syncValue));

  // Reactive disabled
  const isDisabledFn = (() => {
    if (disabledBind == null) return () => false;
    let active = false;
    const apply = () => {
      active = resolveBindRef(disabledBind, ctx.store) === true;
      if (active) el.setAttribute('disabled', '');
      else el.removeAttribute('disabled');
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, apply));
    return () => active;
  })();

  // Label: when the Select carries a visible label, render it (tf-select now
  // shows it above the control, matching tf-input) and use it as the accessible
  // name. Otherwise fall back to the required a11y.label as aria-label only.
  if (labelBind != null) {
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      const text = typeof v === 'string' ? v : (v == null ? '' : String(v));
      if (text) {
        el.setAttribute('label', text);
        el.setAttribute('aria-label', text);
      } else {
        el.removeAttribute('label');
        el.removeAttribute('aria-label');
      }
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));
  } else {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'Select without `label` field requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'Select.a11y.label must resolve to non-blank string at initial render'
      );
    }
    const applyAriaLabel = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) {
        el.setAttribute('aria-label', v);
      } else {
        el.removeAttribute('aria-label');
      }
    };
    applyAriaLabel();
    ctx.registerCleanup(
      subscribeBindRef(component.a11y.label, ctx.store, applyAriaLabel)
    );
  }

  // tf-select emits change with detail.value = the serialized option string
  // (e.g. "tstr:Unclassified"). We must convert it back to the SDK SelectValue
  // {value, kind} before it reaches the dispatcher — and block the raw event so
  // the dispatcher never sees the serialized string (which would be stored
  // verbatim and fail backend validation). Mirror the tf-input fix: the raw
  // event is stopImmediatePropagation'd and a single synthetic event tagged
  // `__tfReemit` carries the converted value through to the dispatcher.
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (isDisabledFn()) return;
    const selectedStr = e.detail?.value ?? el.value;
    let ce;
    if (!selectedStr) {
      // Clear / placeholder selected
      if (!clearable) return;
      ce = new CustomEvent('change', { bubbles: false, detail: { value: null, kind: null } });
    } else {
      const opt = valueMap.get(selectedStr);
      if (!opt) return;
      ce = new CustomEvent('change', {
        bubbles: false,
        detail: { value: opt.value.value, kind: opt.value.tag },
      });
    }
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  el.addEventListener('change', onChange);
  ctx.registerCleanup(() => el.removeEventListener('change', onChange));

  return el;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormSelectRenderers() {
  if (!lookupComponentRenderer(SELECT_TAG)) {
    registerComponentRenderer(SELECT_TAG, renderSelect);
  }
}
