// =============================================================================
// Plik: sdk-runtime/form-combobox-renderer.js
// Opis: Renderery Combobox (0x0305) + Autocomplete (0x0306) — chunk
// 3.3c-3c. Oba używają input'a jako primary control (vs Select gdzie był
// trigger-button); Combobox dodatkowo trzyma static options array z
// filtrem lokalnym (lub remote_search emit'em), Autocomplete jest cienki
// — tylko emit debounced 'search' event, results renderuje host przez
// osobny slot/Component.
//
// Wire: combobox role=combobox + aria-expanded + aria-controls +
// aria-activedescendant. Selection emit'uje 'change' z SelectValue (lub
// 'tstr' dla free_input). Search/remote emit'uje 'search' z `{ query }`.
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
// Walidatory
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

// =============================================================================
// Combobox (0x0305)
// =============================================================================

export const COMBOBOX_TAG = 0x0305;
const COMBOBOX_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]);
// Default debounce dla remote_search emit'a (ms). Spec'u dla Combobox nie
// definiuje, więc trzymamy stałą zgodną z UX (300 = ~typowy autocomplete).
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
  // §5 0x0305: searchable always true — egzekwowane na decode po stronie
  // host'a; tu wymagamy tej samej wartości dla spójności wire'a.
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
  if (groups) {
    const ids = new Set(groups.map((g) => g.id));
    for (let i = 0; i < options.length; i++) {
      if (options[i].groupId != null && !ids.has(options[i].groupId)) {
        throw new TypeError(`Combobox.options[${i}].group_id '${options[i].groupId}' nie ma w groups`);
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-combobox');
  wrapper.classList.add(`tf-combobox--size-${size}`);
  if (virtualize) wrapper.classList.add('tf-combobox--virtualize');

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-combobox__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  // Field row: input + (clear?) + caret.
  const fieldRow = document.createElement('div');
  fieldRow.classList.add('tf-combobox__field');

  const input = document.createElement('input');
  input.setAttribute('type', 'text');
  input.setAttribute('role', 'combobox');
  input.setAttribute('aria-haspopup', 'listbox');
  input.setAttribute('aria-expanded', 'false');
  input.setAttribute('aria-autocomplete', 'list');
  input.classList.add('tf-combobox__input');
  const inputId = `tf-combobox-${component.id}`;
  input.setAttribute('id', inputId);
  if (labelEl) labelEl.setAttribute('for', inputId);

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'Combobox without `label` field requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'Combobox.a11y.label must resolve to non-blank string at initial render'
      );
    }
    const applyAriaLabel = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) input.setAttribute('aria-label', v);
      else input.removeAttribute('aria-label');
    };
    applyAriaLabel();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAriaLabel));
  }

  applyPlaceholderReactive(input, placeholderBind, ctx);
  fieldRow.appendChild(input);

  let clearButton = null;
  if (clearable) {
    clearButton = document.createElement('button');
    clearButton.setAttribute('type', 'button');
    clearButton.classList.add('tf-combobox__clear');
    clearButton.setAttribute('aria-label', 'Clear selection');
    clearButton.textContent = '×';
    clearButton.hidden = true;
    fieldRow.appendChild(clearButton);
  }

  const caret = document.createElement('span');
  caret.classList.add('tf-combobox__caret');
  caret.setAttribute('aria-hidden', 'true');
  caret.textContent = '▾';
  fieldRow.appendChild(caret);

  wrapper.appendChild(fieldRow);

  // Disabled handling — natywny `disabled` na <input> wystarcza; clear też.
  let disabledActive = false;
  const isDisabledFn = (() => {
    if (disabledBind == null) return () => false;
    const apply = () => {
      disabledActive = resolveBindRef(disabledBind, ctx.store) === true;
      if (disabledActive) {
        input.setAttribute('disabled', '');
        input.setAttribute('aria-disabled', 'true');
        if (clearButton) clearButton.disabled = true;
      } else {
        input.removeAttribute('disabled');
        input.removeAttribute('aria-disabled');
        if (clearButton) clearButton.disabled = false;
      }
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, apply));
    return () => disabledActive;
  })();

  // Popover + listbox.
  const popover = document.createElement('div');
  popover.classList.add('tf-combobox__popover');
  popover.hidden = true;
  const popoverId = `${inputId}-popover`;
  popover.setAttribute('id', popoverId);
  input.setAttribute('aria-controls', popoverId);

  const listbox = document.createElement('ul');
  listbox.setAttribute('role', 'listbox');
  listbox.classList.add('tf-combobox__listbox');
  popover.appendChild(listbox);
  wrapper.appendChild(popover);

  // Option DOM build.
  const optionNodes = [];
  const renderOption = (opt, idx, container) => {
    const li = document.createElement('li');
    li.setAttribute('role', 'option');
    li.classList.add('tf-combobox__option');
    li.setAttribute('id', `${inputId}-opt-${idx}`);
    if (opt.disabled) {
      li.setAttribute('aria-disabled', 'true');
      li.classList.add('tf-combobox__option--disabled');
    }
    if (opt.icon) {
      const iconEl = renderIcon(opt.icon, `Combobox.options[${idx}].icon`);
      iconEl.classList.add('tf-combobox__option-icon');
      li.appendChild(iconEl);
    }
    const lblEl = document.createElement('span');
    lblEl.classList.add('tf-combobox__option-label');
    applyTextBind(lblEl, opt.label, ctx);
    li.appendChild(lblEl);
    if (opt.description) {
      const descEl = document.createElement('span');
      descEl.classList.add('tf-combobox__option-description');
      applyTextBind(descEl, opt.description, ctx);
      li.appendChild(descEl);
    }
    container.appendChild(li);
    optionNodes.push({ el: li, opt, idx, visible: true });
  };

  if (groups) {
    const groupContainers = new Map();
    for (const g of groups) {
      const block = document.createElement('li');
      block.setAttribute('role', 'group');
      block.setAttribute('aria-labelledby', `${inputId}-grp-${g.id}`);
      block.classList.add('tf-combobox__group');
      const header = document.createElement('div');
      header.classList.add('tf-combobox__group-header');
      header.setAttribute('id', `${inputId}-grp-${g.id}`);
      applyTextBind(header, g.label, ctx);
      block.appendChild(header);
      const inner = document.createElement('ul');
      inner.setAttribute('role', 'presentation');
      inner.classList.add('tf-combobox__group-list');
      block.appendChild(inner);
      listbox.appendChild(block);
      groupContainers.set(g.id, { block, inner });
    }
    options.forEach((opt, idx) => {
      const c = opt.groupId != null ? groupContainers.get(opt.groupId).inner : listbox;
      renderOption(opt, idx, c);
    });
  } else {
    options.forEach((opt, idx) => renderOption(opt, idx, listbox));
  }

  // ---- state ----
  let activeIdx = -1;
  let isOpen = false;
  let remoteDebounce = null;
  let lastRemoteQuery = null;

  const findSelectedIdx = () => {
    let current;
    try { current = ctx.store.read(bindPath); } catch { current = undefined; }
    if (current == null) return -1;
    return options.findIndex((o) => selectValueEquals(o.value, current));
  };

  /// Wpisana wartość w input — pokazujemy label wybranej opcji jeśli value
  /// matchuje opcję, inaczej raw string ze store'a (dla free_input).
  /// Gdy popover jest otwarty (typing in progress), NIE nadpisujemy
  /// input.value — chronimy typing user'a niezależnie od ścieżki sel/raw.
  const syncInputFromStore = () => {
    const sel = findSelectedIdx();
    if (!isOpen) {
      if (sel >= 0) {
        const v = resolveBindRef(options[sel].label, ctx.store);
        const next = v == null ? '' : String(v);
        if (input.value !== next) input.value = next;
      } else {
        let cur;
        try { cur = ctx.store.read(bindPath); } catch { cur = undefined; }
        const next = cur == null ? '' : String(cur);
        if (input.value !== next) input.value = next;
      }
    }
    if (clearButton) {
      const hasValue = (input.value && input.value.length > 0) || sel >= 0;
      clearButton.hidden = !hasValue;
    }
  };
  syncInputFromStore();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, syncInputFromStore));

  const refreshAriaSelected = () => {
    const sel = findSelectedIdx();
    for (const n of optionNodes) {
      if (n.idx === sel) n.el.setAttribute('aria-selected', 'true');
      else n.el.removeAttribute('aria-selected');
    }
  };
  refreshAriaSelected();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, refreshAriaSelected));

  const visibleNodes = () => optionNodes.filter((n) => n.visible && !n.opt.disabled);

  const setActive = (idx) => {
    activeIdx = idx;
    for (const n of optionNodes) n.el.classList.remove('tf-combobox__option--active');
    if (idx < 0) {
      input.removeAttribute('aria-activedescendant');
      return;
    }
    const n = optionNodes[idx];
    n.el.classList.add('tf-combobox__option--active');
    input.setAttribute('aria-activedescendant', n.el.id);
    if (typeof n.el.scrollIntoView === 'function') {
      try { n.el.scrollIntoView({ block: 'nearest' }); } catch {}
    }
  };
  const moveActive = (dir) => {
    const vis = visibleNodes();
    if (vis.length === 0) return;
    let curPos = vis.findIndex((n) => n.idx === activeIdx);
    let nextPos = curPos < 0 ? (dir > 0 ? 0 : vis.length - 1) : (curPos + dir + vis.length) % vis.length;
    setActive(vis[nextPos].idx);
  };
  const moveActiveTo = (pos) => {
    const vis = visibleNodes();
    if (vis.length === 0) return;
    setActive(vis[Math.max(0, Math.min(pos, vis.length - 1))].idx);
  };

  const applySearch = (query) => {
    const q = query.trim().toLowerCase();
    for (const n of optionNodes) {
      let labelText = '';
      try {
        const lv = resolveBindRef(n.opt.label, ctx.store);
        labelText = lv == null ? '' : String(lv).toLowerCase();
      } catch {}
      const matches = q.length === 0 ? true : labelText.includes(q);
      n.visible = matches;
      n.el.hidden = !matches;
    }
    if (groups) {
      for (const g of groups) {
        const visible = optionNodes.some((n) => n.visible && n.opt.groupId === g.id);
        const headerEl = document.getElementById(`${inputId}-grp-${g.id}`);
        if (headerEl && headerEl.parentElement) headerEl.parentElement.hidden = !visible;
      }
    }
    const cur = optionNodes[activeIdx];
    if (!cur || !cur.visible || cur.opt.disabled) {
      const vis = visibleNodes();
      setActive(vis.length > 0 ? vis[0].idx : -1);
    }
  };

  const emitChange = (detail) => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail,
      })
    );
  };

  const emitSearch = (query) => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('search', {
        bubbles: false,
        detail: { query, action_id: remoteActionId },
      })
    );
  };

  /// Spec'owy gate: minimum N chars przed otwarciem popover'a.
  const canOpenForQuery = (query) => {
    return (query || '').length >= minSearchChars;
  };

  const open = () => {
    if (isOpen || isDisabledFn()) return;
    if (!canOpenForQuery(input.value)) return;
    isOpen = true;
    popover.hidden = false;
    input.setAttribute('aria-expanded', 'true');
    wrapper.classList.add('tf-combobox--open');
    const vis = visibleNodes();
    setActive(vis.length > 0 ? vis[0].idx : -1);
  };

  const close = () => {
    if (!isOpen) return;
    isOpen = false;
    popover.hidden = true;
    input.setAttribute('aria-expanded', 'false');
    wrapper.classList.remove('tf-combobox--open');
    input.removeAttribute('aria-activedescendant');
    activeIdx = -1;
    for (const n of optionNodes) n.el.classList.remove('tf-combobox__option--active');
  };

  const commitOption = (idx) => {
    if (idx < 0) return;
    if (isDisabledFn()) { close(); return; }
    const opt = optionNodes[idx]?.opt;
    if (!opt || opt.disabled) return;
    emitChange({ value: opt.value.value, kind: opt.value.tag });
    close();
  };

  /// Commit raw text — tylko gdy free_input=true. Inaczej ignorujemy
  /// (Enter w polu bez aktywnej opcji = no-op).
  const commitFreeText = (text) => {
    if (!freeInput) return;
    if (isDisabledFn()) return;
    emitChange({ value: text, kind: 'tstr' });
    close();
  };

  // ---- event wiring ----
  const onInput = () => {
    if (isDisabledFn()) return;
    const q = input.value;
    if (q.length >= minSearchChars) {
      applySearch(q);
      if (!isOpen) open();
    } else {
      // Poniżej progu — zamknij popover ORAZ skasuj zaplanowane remote
      // search'e, żeby stary query nie poleciał po debounce'ie.
      if (isOpen) close();
      if (remoteDebounce) {
        clearTimeout(remoteDebounce);
        remoteDebounce = null;
      }
    }
    if (clearButton) clearButton.hidden = q.length === 0 && findSelectedIdx() < 0;
    if (remoteSearch && q.length >= minSearchChars && q !== lastRemoteQuery) {
      if (remoteDebounce) clearTimeout(remoteDebounce);
      remoteDebounce = setTimeout(() => {
        lastRemoteQuery = q;
        emitSearch(q);
      }, COMBOBOX_REMOTE_DEBOUNCE_MS);
    }
  };
  input.addEventListener('input', onInput);
  ctx.registerCleanup(() => input.removeEventListener('input', onInput));

  const onKeyDown = (e) => {
    if (isDisabledFn()) return;
    switch (e.key) {
      case 'ArrowDown':
        e.preventDefault();
        if (!isOpen) open(); else moveActive(1);
        return;
      case 'ArrowUp':
        e.preventDefault();
        if (!isOpen) open(); else moveActive(-1);
        return;
      case 'Home':
        if (!isOpen) return;
        e.preventDefault();
        moveActiveTo(0);
        return;
      case 'End':
        if (!isOpen) return;
        e.preventDefault();
        moveActiveTo(Number.MAX_SAFE_INTEGER);
        return;
      case 'Enter':
        e.preventDefault();
        if (isOpen && activeIdx >= 0) {
          commitOption(activeIdx);
        } else if (freeInput && input.value.length > 0) {
          commitFreeText(input.value);
        }
        return;
      case 'Escape':
        if (isOpen) {
          e.preventDefault();
          close();
        }
        return;
      case 'Tab':
        if (isOpen) close();
        return;
    }
  };
  input.addEventListener('keydown', onKeyDown);
  ctx.registerCleanup(() => input.removeEventListener('keydown', onKeyDown));

  const onFocus = () => {
    if (isDisabledFn()) return;
    if (canOpenForQuery(input.value)) open();
  };
  input.addEventListener('focus', onFocus);
  ctx.registerCleanup(() => input.removeEventListener('focus', onFocus));

  if (clearButton) {
    const onClear = (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (isDisabledFn()) return;
      input.value = '';
      emitChange({ value: null, kind: null });
      close();
    };
    clearButton.addEventListener('click', onClear);
    ctx.registerCleanup(() => clearButton.removeEventListener('click', onClear));
  }

  // Caret click też otwiera/zamyka (jak dropdown).
  const onCaretClick = (e) => {
    if (isDisabledFn()) return;
    e.preventDefault();
    if (isOpen) close(); else {
      try { input.focus(); } catch {}
      open();
    }
  };
  caret.addEventListener('click', onCaretClick);
  ctx.registerCleanup(() => caret.removeEventListener('click', onCaretClick));

  const onListboxMouseDown = (e) => {
    const li = e.target.closest('li[role="option"]');
    if (!li) return;
    e.preventDefault();
    const node = optionNodes.find((n) => n.el === li);
    if (!node) return;
    commitOption(node.idx);
  };
  listbox.addEventListener('mousedown', onListboxMouseDown);
  ctx.registerCleanup(() => listbox.removeEventListener('mousedown', onListboxMouseDown));

  const onDocClick = (e) => {
    if (!isOpen) return;
    if (wrapper.contains(e.target)) return;
    close();
  };
  document.addEventListener('click', onDocClick);
  ctx.registerCleanup(() => document.removeEventListener('click', onDocClick));

  // Cleanup pending debounce na destroy.
  ctx.registerCleanup(() => {
    if (remoteDebounce) clearTimeout(remoteDebounce);
  });

  return wrapper;
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-autocomplete');

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-autocomplete__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  const input = document.createElement('input');
  input.setAttribute('type', 'text');
  input.setAttribute('role', 'combobox');
  input.setAttribute('aria-autocomplete', 'list');
  input.setAttribute('aria-expanded', 'false');
  input.classList.add('tf-autocomplete__input');
  const inputId = `tf-autocomplete-${component.id}`;
  input.setAttribute('id', inputId);
  if (labelEl) labelEl.setAttribute('for', inputId);
  if (resultTemplateId) {
    // Wskazówka dla host'a — który slot wyświetli wyniki. Renderer nie
    // wyświetla wyników wewnętrznie; host re-renderuje listbox przez slot
    // i powiązuje go z input'em przez aria-controls (ustawiamy id slotu
    // jako data-attribute żeby host mógł sięgnąć bez extra spec'u).
    input.setAttribute('aria-controls', `${inputId}-${resultTemplateId}`);
    input.setAttribute('data-result-template-id', resultTemplateId);
  }

  if (labelBind == null) {
    if (component.a11y == null || component.a11y.label == null) {
      throw new TypeError(
        'Autocomplete without `label` field requires Component.a11y.label for accessible name'
      );
    }
    const initial = resolveBindRef(component.a11y.label, ctx.store);
    if (typeof initial !== 'string' || initial.trim().length === 0) {
      throw new TypeError(
        'Autocomplete.a11y.label must resolve to non-blank string at initial render'
      );
    }
    const applyAriaLabel = () => {
      const v = resolveBindRef(component.a11y.label, ctx.store);
      if (typeof v === 'string' && v.trim().length > 0) input.setAttribute('aria-label', v);
      else input.removeAttribute('aria-label');
    };
    applyAriaLabel();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAriaLabel));
  }

  applyPlaceholderReactive(input, placeholderBind, ctx);

  // Reactive value sync ze store (one-way read).
  const apply = () => {
    let v;
    try { v = ctx.store.read(bindPath); } catch { v = undefined; }
    const next = v == null ? '' : String(v);
    if (input.value !== next) input.value = next;
  };
  apply();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, apply));

  wrapper.appendChild(input);

  let debounceTimer = null;
  let lastQuery = null;

  const emitSearch = (query) => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('search', {
        bubbles: false,
        detail: { query, action_id: remoteActionId, result_template_id: resultTemplateId },
      })
    );
  };

  const onInput = () => {
    const q = input.value;
    // input.value też emit'uje 'input' wrapperowy dla host'a — chunk 3.6
    // potem podpina write-back; tu raw text idzie też przez `input` event'a
    // żeby host mógł od razu reagować (np. ukryć stary popover).
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('input', {
        bubbles: false,
        detail: { value: q, kind: 'tstr' },
      })
    );
    if (q.length < minSearchChars) {
      input.setAttribute('aria-expanded', 'false');
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
      input.setAttribute('aria-expanded', 'true');
      emitSearch(q);
    }, debounceMs);
  };
  input.addEventListener('input', onInput);
  ctx.registerCleanup(() => input.removeEventListener('input', onInput));

  const onChange = () => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: input.value, kind: 'tstr' },
      })
    );
  };
  input.addEventListener('change', onChange);
  ctx.registerCleanup(() => input.removeEventListener('change', onChange));

  const onBlur = () => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('blur', {
        bubbles: false,
        detail: null,
      })
    );
  };
  input.addEventListener('blur', onBlur);
  ctx.registerCleanup(() => input.removeEventListener('blur', onBlur));

  const onFocus = () => {
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('focus', {
        bubbles: false,
        detail: null,
      })
    );
  };
  input.addEventListener('focus', onFocus);
  ctx.registerCleanup(() => input.removeEventListener('focus', onFocus));

  ctx.registerCleanup(() => {
    if (debounceTimer) clearTimeout(debounceTimer);
  });

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormComboboxRenderers() {
  if (!lookupComponentRenderer(COMBOBOX_TAG)) {
    registerComponentRenderer(COMBOBOX_TAG, renderCombobox);
  }
  if (!lookupComponentRenderer(AUTOCOMPLETE_TAG)) {
    registerComponentRenderer(AUTOCOMPLETE_TAG, renderAutocomplete);
  }
}
