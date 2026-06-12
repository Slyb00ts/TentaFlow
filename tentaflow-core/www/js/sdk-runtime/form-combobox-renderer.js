// =============================================================================
// File: sdk-runtime/form-combobox-renderer.js
// Description: Combobox (0x0305) + Autocomplete (0x0306) renderers. Both
// render through the <tf-combobox> web component (input + filtering popover).
// Combobox feeds the component a static options array (local filter, or a
// debounced remote 'search' emit); Autocomplete is thin — only a debounced
// 'search' emit, results are rendered by the host through a separate
// slot/Component.
//
// Wire: selection emits 'change' with SelectValue (or 'tstr' for free_input).
// Search/remote emits 'search' with `{ query }`. Raw component events are
// intercepted and re-emitted in SDK shape (the `__tfReemit` pattern, same as
// the Select renderer) so the dispatcher never sees component-internal
// payloads.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/form/selectors.rs
// Combobox + Autocomplete.
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
  if (!Array.isArray(v)) throw new TypeError(`${ctx}: expected StatePath`);
  return v;
}
function requireU8(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFn) throw new TypeError(`${ctx}: expected u8, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFF) throw new TypeError(`${ctx}: expected u8, got ${v}`);
  return v;
}
function requireU16(v, ctx) {
  if (typeof v === 'bigint') {
    if (v < 0n || v > 0xFFFFn) throw new TypeError(`${ctx}: expected u16, got ${v}`);
    return Number(v);
  }
  if (!Number.isInteger(v) || v < 0 || v > 0xFFFF) throw new TypeError(`${ctx}: expected u16, got ${v}`);
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

function applyPlaceholderReactive(el, bindRef, ctx) {
  if (bindRef == null) return;
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    if (v == null || v === '') el.removeAttribute('placeholder');
    else el.setAttribute('placeholder', String(v));
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
}

/// Applies the visible label (`label` attribute) or the required a11y label
/// (`aria-label` attribute) on a tf-combobox host, reactively.
function applyLabelOrAria(el, labelBind, component, ctx, componentName) {
  if (labelBind != null) {
    const applyLabel = () => {
      const v = resolveBindRef(labelBind, ctx.store);
      el.setAttribute('label', v == null ? '' : String(v));
    };
    applyLabel();
    ctx.registerCleanup(subscribeBindRef(labelBind, ctx.store, applyLabel));
    return;
  }
  if (component.a11y == null || component.a11y.label == null) {
    throw new TypeError(
      `${componentName} without \`label\` field requires Component.a11y.label for accessible name`
    );
  }
  const initial = resolveBindRef(component.a11y.label, ctx.store);
  if (typeof initial !== 'string' || initial.trim().length === 0) {
    throw new TypeError(
      `${componentName}.a11y.label must resolve to non-blank string at initial render`
    );
  }
  const applyAria = () => {
    const v = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof v === 'string' && v.trim().length > 0) el.setAttribute('aria-label', v);
    else el.removeAttribute('aria-label');
  };
  applyAria();
  ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAria));
}

// =============================================================================
// Combobox (0x0305)
// =============================================================================

export const COMBOBOX_TAG = 0x0305;
const COMBOBOX_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
// Default debounce for the remote_search emit (ms). The Combobox spec does
// not define one, so a UX-typical constant is used (300 ≈ usual autocomplete).
const COMBOBOX_REMOTE_DEBOUNCE_MS = 300;

function renderCombobox(component, ctx) {
  assertOnlyKnownFields(component.fields, COMBOBOX_FIELD_KEYS, 'Combobox');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'Combobox.bind_path');
  const optionsRaw = ctx.readField(component.fields, 1);
  if (!Array.isArray(optionsRaw)) {
    throw new TypeError('Combobox.options: expected Array<SelectOption>');
  }
  const options = optionsRaw.map((o, i) => parseSelectOption(o, `Combobox.options[${i}]`));
  const placeholderBind = ctx.readField(component.fields, 2);
  const labelBind = ctx.readField(component.fields, 3);
  // §5 0x0305: searchable always true — enforced on decode host-side; the
  // same value is required here for wire consistency.
  const searchable = requireBool(ctx.readField(component.fields, 4), 'Combobox.searchable');
  if (!searchable) {
    throw new TypeError('Combobox.searchable must be true (catalog §5 0x0305)');
  }
  const clearable = requireBool(ctx.readField(component.fields, 5), 'Combobox.clearable');
  const virtualize = requireBool(ctx.readField(component.fields, 6), 'Combobox.virtualize');
  const disabledBind = ctx.readField(component.fields, 7);
  const size = requireEnum(ctx.readField(component.fields, 8), INPUT_SIZES, 'Combobox.size');
  const groupsRaw = ctx.readField(component.fields, 9);
  const groups = groupsRaw == null ? null : (() => {
    if (!Array.isArray(groupsRaw)) throw new TypeError('Combobox.groups: expected Array<SelectGroup>');
    return groupsRaw.map((g, i) => parseSelectGroup(g, `Combobox.groups[${i}]`));
  })();
  const groupById = new Map();
  if (groups) {
    for (const g of groups) groupById.set(g.id, g);
    for (let i = 0; i < options.length; i++) {
      if (options[i].groupId != null && !groupById.has(options[i].groupId)) {
        throw new TypeError(`Combobox.options[${i}].group_id '${options[i].groupId}' not present in groups`);
      }
    }
  }
  const freeInput = requireBool(ctx.readField(component.fields, 10), 'Combobox.free_input');
  const minSearchChars = requireU8(ctx.readField(component.fields, 11), 'Combobox.min_search_chars');
  const remoteSearch = requireBool(ctx.readField(component.fields, 12), 'Combobox.remote_search');
  const remoteActionIdRaw = ctx.readField(component.fields, 13);
  const remoteActionId = remoteActionIdRaw == null ? null : requireString(remoteActionIdRaw, 'Combobox.remote_action_id');
  if (remoteSearch && remoteActionId == null) {
    throw new TypeError('Combobox.remote_search=true requires remote_action_id');
  }

  const el = document.createElement('tf-combobox');
  el.classList.add(`tf-combobox--size-${size}`);
  if (virtualize) el.classList.add('tf-combobox--virtualize');
  if (clearable) el.setAttribute('clearable', '');
  if (freeInput) el.setAttribute('free-input', '');
  el.setAttribute('min-chars', String(minSearchChars));

  applyLabelOrAria(el, labelBind, component, ctx, 'Combobox');
  applyPlaceholderReactive(el, placeholderBind, ctx);

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

  // Options feed: component option.value carries the SDK option index, so the
  // change interceptor can map back to the typed SelectValue. Labels,
  // descriptions and group labels are BindRefs — re-feed on any change.
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
      out.icon = renderIcon(opt.icon, `Combobox.options[${idx}].icon`);
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

  const findSelectedIdx = () => {
    let current;
    try { current = ctx.store.read(bindPath); } catch { current = undefined; }
    if (current == null) return -1;
    return options.findIndex((o) => selectValueEquals(o.value, current));
  };

  // Store → input text: the label of the matching option, otherwise the raw
  // store string (free_input). Sync goes through the component `value`
  // property — it never overwrites the input while it has focus, which
  // protects in-progress typing.
  const syncFromStore = () => {
    const sel = findSelectedIdx();
    let text;
    if (sel >= 0) {
      const v = resolveBindRef(options[sel].label, ctx.store);
      text = v == null ? '' : String(v);
    } else {
      let cur;
      try { cur = ctx.store.read(bindPath); } catch { cur = undefined; }
      text = cur == null ? '' : String(cur);
    }
    if (el.getAttribute('value') !== text) el.value = text;
  };
  syncFromStore();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, syncFromStore));

  let remoteDebounce = null;
  let lastRemoteQuery = null;

  const emitSearch = (query) => {
    const ce = new (globalThis.CustomEvent || globalThis.Event)('search', {
      bubbles: false,
      detail: { query, action_id: remoteActionId },
    });
    el.dispatchEvent(ce);
  };

  // tf-combobox 'change' carries {value, label} where value is the option
  // index (option commit), null (clear) or raw text with free=true
  // (free-input commit). Convert to the SDK SelectValue shape before the
  // dispatcher sees it — the raw event is blocked and a single synthetic
  // event tagged `__tfReemit` carries the converted payload.
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    // Native 'change' from the inner text input bubbles through too — it has
    // no detail and is not a commit; swallow it.
    if (!e.detail || typeof e.detail !== 'object') return;
    if (isDisabledFn()) return;
    let ce;
    if (e.detail.free === true && typeof e.detail.value === 'string') {
      if (!freeInput) return;
      ce = new CustomEvent('change', { bubbles: false, detail: { value: e.detail.value, kind: 'tstr' } });
    } else if (e.detail.value == null) {
      if (!clearable) return;
      ce = new CustomEvent('change', { bubbles: false, detail: { value: null, kind: null } });
    } else if (typeof e.detail.value === 'number') {
      const opt = options[e.detail.value];
      if (!opt || opt.disabled) return;
      ce = new CustomEvent('change', {
        bubbles: false,
        detail: { value: opt.value.value, kind: opt.value.tag },
      });
    } else {
      return;
    }
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  el.addEventListener('change', onChange);
  ctx.registerCleanup(() => el.removeEventListener('change', onChange));

  // tf-combobox 'input' {query} drives the remote-search debounce. The raw
  // event is blocked — the Combobox schema exposes no 'input' handler.
  const onInput = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (!e.detail || typeof e.detail.query !== 'string') return;
    if (isDisabledFn()) return;
    const q = e.detail.query;
    if (q.length < minSearchChars) {
      // Below the threshold — cancel any scheduled remote search so a stale
      // query never fires after the debounce.
      if (remoteDebounce) {
        clearTimeout(remoteDebounce);
        remoteDebounce = null;
      }
      return;
    }
    if (remoteSearch && q !== lastRemoteQuery) {
      if (remoteDebounce) clearTimeout(remoteDebounce);
      remoteDebounce = setTimeout(() => {
        lastRemoteQuery = q;
        emitSearch(q);
      }, COMBOBOX_REMOTE_DEBOUNCE_MS);
    }
  };
  el.addEventListener('input', onInput);
  ctx.registerCleanup(() => el.removeEventListener('input', onInput));

  // Cleanup pending debounce on destroy.
  ctx.registerCleanup(() => {
    if (remoteDebounce) clearTimeout(remoteDebounce);
  });

  return el;
}

// =============================================================================
// Autocomplete (0x0306)
// =============================================================================

export const AUTOCOMPLETE_TAG = 0x0306;
const AUTOCOMPLETE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6]);

function renderAutocomplete(component, ctx) {
  assertOnlyKnownFields(component.fields, AUTOCOMPLETE_FIELD_KEYS, 'Autocomplete');

  const bindPath = requirePath(ctx.readField(component.fields, 0), 'Autocomplete.bind_path');
  const remoteActionId = requireString(
    ctx.readField(component.fields, 1), 'Autocomplete.remote_action_id'
  );
  const resultTemplateIdRaw = ctx.readField(component.fields, 2);
  const resultTemplateId = resultTemplateIdRaw == null ? null : requireString(resultTemplateIdRaw, 'Autocomplete.result_template_id');
  const minSearchChars = requireU8(ctx.readField(component.fields, 3), 'Autocomplete.min_search_chars');
  const debounceMs = requireU16(ctx.readField(component.fields, 4), 'Autocomplete.debounce_ms');
  if (debounceMs === 0) throw new TypeError('Autocomplete.debounce_ms must be > 0');
  const placeholderBind = ctx.readField(component.fields, 5);
  const labelBind = ctx.readField(component.fields, 6);

  // Autocomplete is the same input primitive — a tf-combobox with no local
  // options. Results are rendered by the host through a separate slot, so the
  // component popover never opens.
  const el = document.createElement('tf-combobox');
  el.classList.add('tf-autocomplete');
  el.setAttribute('min-chars', String(minSearchChars));
  if (resultTemplateId) {
    // Host hint — which slot displays the results. The renderer does not
    // display results internally; the host re-renders the listbox through a
    // slot and links it via aria-controls (the slot id is exposed as a
    // data-attribute so the host can reach it without extra spec).
    el.setAttribute('aria-controls', `tf-autocomplete-${component.id}-${resultTemplateId}`);
    el.setAttribute('data-result-template-id', resultTemplateId);
  }

  applyLabelOrAria(el, labelBind, component, ctx, 'Autocomplete');
  applyPlaceholderReactive(el, placeholderBind, ctx);

  // Reactive value sync from the store (one-way read) via the component
  // `value` property — it skips the inner write while the input has focus.
  const apply = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    const next = v == null ? '' : String(v);
    if (el.getAttribute('value') !== next) el.value = next;
  };
  apply();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, apply));

  let debounceTimer = null;
  let lastQuery = null;

  const emitSearch = (query) => {
    el.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('search', {
        bubbles: false,
        detail: { query, action_id: remoteActionId, result_template_id: resultTemplateId },
      })
    );
  };

  // tf-combobox 'input' {query} → SDK 'input' {value, kind} re-emit (so the
  // host can react immediately, e.g. hide a stale popover) + debounced
  // 'search' emit. Write-back is wired by chunk 3.6.
  const onInput = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    if (!e.detail || typeof e.detail.query !== 'string') return;
    const q = e.detail.query;
    const ce = new CustomEvent('input', {
      bubbles: false,
      detail: { value: q, kind: 'tstr' },
    });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
    if (q.length < minSearchChars) {
      if (debounceTimer) {
        clearTimeout(debounceTimer);
        debounceTimer = null;
      }
      return;
    }
    if (q === lastQuery) return;
    if (debounceTimer) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      lastQuery = q;
      emitSearch(q);
    }, debounceMs);
  };
  el.addEventListener('input', onInput);
  ctx.registerCleanup(() => el.removeEventListener('input', onInput));

  // Native 'change' from the inner text input → SDK 'change' {value, kind}.
  const onChange = (e) => {
    if (e.__tfReemit) return;
    e.stopImmediatePropagation();
    const ce = new CustomEvent('change', {
      bubbles: false,
      detail: { value: el.value, kind: 'tstr' },
    });
    ce.__tfReemit = true;
    el.dispatchEvent(ce);
  };
  el.addEventListener('change', onChange);
  ctx.registerCleanup(() => el.removeEventListener('change', onChange));

  // Focus events do not bubble, but focusin/focusout do — translate them to
  // the SDK 'focus'/'blur' events on the host element.
  const onFocusIn = () => {
    el.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('focus', { bubbles: false, detail: null })
    );
  };
  const onFocusOut = () => {
    el.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('blur', { bubbles: false, detail: null })
    );
  };
  el.addEventListener('focusin', onFocusIn);
  el.addEventListener('focusout', onFocusOut);
  ctx.registerCleanup(() => {
    el.removeEventListener('focusin', onFocusIn);
    el.removeEventListener('focusout', onFocusOut);
  });

  ctx.registerCleanup(() => {
    if (debounceTimer) clearTimeout(debounceTimer);
  });

  return el;
}

// =============================================================================
// Registration
// =============================================================================

export function registerFormComboboxRenderers() {
  if (!lookupComponentRenderer(COMBOBOX_TAG)) {
    registerComponentRenderer(COMBOBOX_TAG, renderCombobox);
  }
  if (!lookupComponentRenderer(AUTOCOMPLETE_TAG)) {
    registerComponentRenderer(AUTOCOMPLETE_TAG, renderAutocomplete);
  }
}
