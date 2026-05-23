// =============================================================================
// Plik: sdk-runtime/form-select-renderer.js
// Opis: Renderer Select (0x0303) — single-value dropdown z popover'em,
// WAI-ARIA combobox pattern (role=combobox+listbox+option, aria-expanded,
// aria-controls, aria-activedescendant), pełna keyboard nav (Up/Down/
// Home/End/Enter/Escape/Tab), opcjonalny inline search filter, grupy
// (SelectGroup), clearable button, reactive disabled.
//
// Wartość pisemna jest CZYTANA ze store'a (read-only) — wybór opcji
// dispatchuje `change` z `{ value, kind }` SelectValue dla write-back
// przez host (chunk 3.6).
//
// Spec ref: `tentaflow-sdk-spec/src/protocol/ui/form/selectors.rs` Select.
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

/// Parsuje SelectValue tagged union zgodnie ze spec §1.5.
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

function createOptionIcon(iconRef, ctx) {
  // Delegujemy do shared `renderIcon` (icon-renderer.js) — pełna obsługa
  // IconRef::Named/IconRef::Asset zgodnie ze spec inline.rs §1.5.
  // `ctx` to per-pole nazwa do błędów (np. 'Select.options[0].icon').
  return renderIcon(iconRef, ctx);
}

// =============================================================================
// Reactive helpers
// =============================================================================

function applyDisabledReactive(element, bindRef, ctx) {
  if (bindRef == null) return () => false;
  let active = false;
  const apply = () => {
    active = resolveBindRef(bindRef, ctx.store) === true;
    if (active) {
      element.setAttribute('disabled', '');
      element.setAttribute('aria-disabled', 'true');
    } else {
      element.removeAttribute('disabled');
      element.removeAttribute('aria-disabled');
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
  return () => active;
}

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
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
  // virtualize: hint dla renderera — przy małej liście (<100) nie ma sensu;
  // zawsze renderujemy ALL options (DOM jest stabilny per opcja). Zaznaczamy
  // klasą `.tf-select--virtualize` żeby CSS mógł limitować popover height.
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

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-select');
  wrapper.classList.add(`tf-select--size-${size}`);
  if (virtualize) wrapper.classList.add('tf-select--virtualize');

  let labelEl = null;
  if (labelBind != null) {
    labelEl = document.createElement('label');
    labelEl.classList.add('tf-select__label');
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  // Trigger button — WAI-ARIA combobox pattern requires role=combobox on
  // the focusable element. Button hosting popover daje fixed focus target.
  const trigger = document.createElement('button');
  trigger.setAttribute('type', 'button');
  trigger.setAttribute('role', 'combobox');
  trigger.setAttribute('aria-haspopup', 'listbox');
  trigger.setAttribute('aria-expanded', 'false');
  trigger.classList.add('tf-select__trigger');
  const triggerId = `tf-select-${component.id}`;
  trigger.setAttribute('id', triggerId);
  if (labelEl) labelEl.setAttribute('for', triggerId);

  if (labelBind == null) {
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
        trigger.setAttribute('aria-label', v);
      } else {
        trigger.removeAttribute('aria-label');
      }
    };
    applyAriaLabel();
    ctx.registerCleanup(
      subscribeBindRef(component.a11y.label, ctx.store, applyAriaLabel)
    );
  }

  // Aktualny tekst wybranej opcji (lub placeholder).
  const triggerLabel = document.createElement('span');
  triggerLabel.classList.add('tf-select__trigger-label');
  trigger.appendChild(triggerLabel);

  // Clear button (jeśli clearable + jest wybrana wartość).
  let clearButton = null;
  if (clearable) {
    clearButton = document.createElement('button');
    clearButton.setAttribute('type', 'button');
    clearButton.classList.add('tf-select__clear');
    clearButton.setAttribute('aria-label', 'Clear selection');
    clearButton.textContent = '×';
    clearButton.hidden = true;
    trigger.appendChild(clearButton);
  }

  // Caret indicator (chevron).
  const caret = document.createElement('span');
  caret.classList.add('tf-select__caret');
  caret.setAttribute('aria-hidden', 'true');
  caret.textContent = '▾';
  trigger.appendChild(caret);

  const isDisabledFn = applyDisabledReactive(trigger, disabledBind, ctx);

  wrapper.appendChild(trigger);

  // Popover — listbox + opcjonalny search input.
  const popover = document.createElement('div');
  popover.classList.add('tf-select__popover');
  popover.hidden = true;
  const popoverId = `${triggerId}-popover`;
  popover.setAttribute('id', popoverId);
  trigger.setAttribute('aria-controls', popoverId);

  let searchInput = null;
  let searchQuery = '';
  if (searchable) {
    searchInput = document.createElement('input');
    searchInput.setAttribute('type', 'text');
    searchInput.classList.add('tf-select__search');
    searchInput.setAttribute('aria-autocomplete', 'list');
    searchInput.setAttribute('placeholder', '');
    popover.appendChild(searchInput);
  }

  const listbox = document.createElement('ul');
  listbox.setAttribute('role', 'listbox');
  listbox.classList.add('tf-select__listbox');
  popover.appendChild(listbox);
  wrapper.appendChild(popover);

  // Build option DOM nodes. Trzymamy mapping idx → {el, opt} + currently-
  // visible flag (search filter).
  const optionNodes = [];
  // Index per group_id → listEl (gdy groups). Niegrupowane opcje idą do
  // głównego listbox bez header'a.
  const renderOption = (opt, idx, container) => {
    const li = document.createElement('li');
    li.setAttribute('role', 'option');
    li.classList.add('tf-select__option');
    li.setAttribute('id', `${triggerId}-opt-${idx}`);
    if (opt.disabled) {
      li.setAttribute('aria-disabled', 'true');
      li.classList.add('tf-select__option--disabled');
    }
    if (opt.icon) {
      const iconEl = createOptionIcon(opt.icon, `Select.options[${idx}].icon`);
      iconEl.classList.add('tf-select__option-icon');
      li.appendChild(iconEl);
    }
    const labelEl2 = document.createElement('span');
    labelEl2.classList.add('tf-select__option-label');
    applyTextBind(labelEl2, opt.label, ctx);
    li.appendChild(labelEl2);
    if (opt.description) {
      const descEl = document.createElement('span');
      descEl.classList.add('tf-select__option-description');
      applyTextBind(descEl, opt.description, ctx);
      li.appendChild(descEl);
    }
    container.appendChild(li);
    optionNodes.push({ el: li, opt, idx, visible: true });
  };

  if (groups) {
    const groupContainers = new Map();
    for (const g of groups) {
      const groupBlock = document.createElement('li');
      groupBlock.setAttribute('role', 'group');
      groupBlock.setAttribute('aria-labelledby', `${triggerId}-grp-${g.id}`);
      groupBlock.classList.add('tf-select__group');
      const header = document.createElement('div');
      header.classList.add('tf-select__group-header');
      header.setAttribute('id', `${triggerId}-grp-${g.id}`);
      applyTextBind(header, g.label, ctx);
      groupBlock.appendChild(header);
      const inner = document.createElement('ul');
      inner.setAttribute('role', 'presentation');
      inner.classList.add('tf-select__group-list');
      groupBlock.appendChild(inner);
      listbox.appendChild(groupBlock);
      groupContainers.set(g.id, { block: groupBlock, inner });
    }
    options.forEach((opt, idx) => {
      const c = opt.groupId != null ? groupContainers.get(opt.groupId).inner : listbox;
      renderOption(opt, idx, c);
    });
  } else {
    options.forEach((opt, idx) => renderOption(opt, idx, listbox));
  }

  // ---- selection + active descendant state ----
  let activeIdx = -1;
  let isOpen = false;

  const findSelectedIdx = () => {
    let current;
    try { current = ctx.store.read(bindPath); } catch { current = undefined; }
    if (current === undefined || current === null) return -1;
    return options.findIndex((o) => selectValueEquals(o.value, current));
  };

  const updateTriggerLabel = () => {
    const idx = findSelectedIdx();
    if (idx < 0) {
      triggerLabel.classList.add('tf-select__trigger-label--placeholder');
      if (placeholderBind != null) {
        const v = resolveBindRef(placeholderBind, ctx.store);
        triggerLabel.textContent = v == null ? '' : String(v);
      } else {
        triggerLabel.textContent = '';
      }
      if (clearButton) clearButton.hidden = true;
    } else {
      triggerLabel.classList.remove('tf-select__trigger-label--placeholder');
      const v = resolveBindRef(options[idx].label, ctx.store);
      triggerLabel.textContent = v == null ? '' : String(v);
      if (clearButton) clearButton.hidden = false;
    }
  };
  updateTriggerLabel();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, updateTriggerLabel));
  if (placeholderBind != null) {
    ctx.registerCleanup(
      subscribeBindRef(placeholderBind, ctx.store, updateTriggerLabel)
    );
  }

  // Mark aria-selected on currently-selected option (renders w popover'ze).
  const updateAriaSelected = () => {
    const sel = findSelectedIdx();
    for (const n of optionNodes) {
      if (n.idx === sel) n.el.setAttribute('aria-selected', 'true');
      else n.el.removeAttribute('aria-selected');
    }
  };
  updateAriaSelected();
  ctx.registerCleanup(ctx.store.subscribe(bindPath, updateAriaSelected));

  const setActive = (idx) => {
    activeIdx = idx;
    for (const n of optionNodes) n.el.classList.remove('tf-select__option--active');
    if (idx < 0) {
      trigger.removeAttribute('aria-activedescendant');
      return;
    }
    const n = optionNodes[idx];
    n.el.classList.add('tf-select__option--active');
    trigger.setAttribute('aria-activedescendant', n.el.id);
    // Scroll into view (best-effort; happy-dom skip).
    if (typeof n.el.scrollIntoView === 'function') {
      try { n.el.scrollIntoView({ block: 'nearest' }); } catch {}
    }
  };

  const visibleNodes = () => optionNodes.filter((n) => n.visible && !n.opt.disabled);
  const moveActive = (dir) => {
    const vis = visibleNodes();
    if (vis.length === 0) return;
    let curPos = vis.findIndex((n) => n.idx === activeIdx);
    let nextPos;
    if (curPos < 0) {
      nextPos = dir > 0 ? 0 : vis.length - 1;
    } else {
      nextPos = (curPos + dir + vis.length) % vis.length;
    }
    setActive(vis[nextPos].idx);
  };

  const moveActiveTo = (pos) => {
    const vis = visibleNodes();
    if (vis.length === 0) return;
    const clamped = Math.max(0, Math.min(pos, vis.length - 1));
    setActive(vis[clamped].idx);
  };

  const applySearch = (query) => {
    searchQuery = query;
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
    // Group header visibility — hide groups with zero visible options.
    if (groups) {
      for (const g of groups) {
        const visible = optionNodes.some((n) => n.visible && n.opt.groupId === g.id);
        const block = listbox.querySelector(`#${CSS.escape(`${triggerId}-grp-${g.id}`)}`)
          ?.parentElement;
        if (block) block.hidden = !visible;
      }
    }
    // Reset active if no longer visible.
    const cur = optionNodes[activeIdx];
    if (!cur || !cur.visible || cur.opt.disabled) {
      const vis = visibleNodes();
      setActive(vis.length > 0 ? vis[0].idx : -1);
    }
  };

  const open = () => {
    if (isOpen || isDisabledFn()) return;
    isOpen = true;
    popover.hidden = false;
    trigger.setAttribute('aria-expanded', 'true');
    wrapper.classList.add('tf-select--open');
    // Initial active = selected, jeśli widoczna; inaczej pierwsza widoczna.
    const sel = findSelectedIdx();
    if (sel >= 0 && optionNodes[sel].visible && !optionNodes[sel].opt.disabled) {
      setActive(sel);
    } else {
      const vis = visibleNodes();
      setActive(vis.length > 0 ? vis[0].idx : -1);
    }
    if (searchInput) {
      searchInput.value = searchQuery;
      // Focus search input umozliwia natychmiastowe pisanie. Bez search'a
      // focus pozostaje na trigger'ze (combobox spec).
      try { searchInput.focus(); } catch {}
    }
  };

  const close = () => {
    if (!isOpen) return;
    isOpen = false;
    popover.hidden = true;
    trigger.setAttribute('aria-expanded', 'false');
    wrapper.classList.remove('tf-select--open');
    trigger.removeAttribute('aria-activedescendant');
    activeIdx = -1;
    for (const n of optionNodes) n.el.classList.remove('tf-select__option--active');
    try { trigger.focus(); } catch {}
  };

  const commit = (idx) => {
    if (idx < 0) return;
    // Reactive disabled może flipnąć w trakcie open'a — sprawdź per-commit,
    // nie tylko per-open. Bez tego click w już otwartym popover'ze nadal
    // przepuściłby change po flipie BindRef'a na true.
    if (isDisabledFn()) {
      close();
      return;
    }
    const opt = optionNodes[idx]?.opt;
    if (!opt || opt.disabled) return;
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: opt.value.value, kind: opt.value.tag },
      })
    );
    close();
  };

  // ---- event wiring ----
  const onTriggerClick = (e) => {
    if (isDisabledFn()) return;
    if (clearButton && e.target === clearButton) return;
    e.preventDefault();
    if (isOpen) close(); else open();
  };
  trigger.addEventListener('click', onTriggerClick);
  ctx.registerCleanup(() => trigger.removeEventListener('click', onTriggerClick));

  if (clearButton) {
    const onClearClick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (isDisabledFn()) return;
      // Clear emit'uje `change` z `null` — kontrakt z chunkiem 3.6 jest taki,
      // że host wie jak zinterpretować null (PatchOp::Delete).
      wrapper.dispatchEvent(
        new (globalThis.CustomEvent || globalThis.Event)('change', {
          bubbles: false,
          detail: { value: null, kind: null },
        })
      );
    };
    clearButton.addEventListener('click', onClearClick);
    ctx.registerCleanup(() => clearButton.removeEventListener('click', onClearClick));
  }

  const onTriggerKeyDown = (e) => {
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
      case ' ':
        if (!isOpen) {
          e.preventDefault();
          open();
        } else {
          e.preventDefault();
          commit(activeIdx);
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
  trigger.addEventListener('keydown', onTriggerKeyDown);
  ctx.registerCleanup(() => trigger.removeEventListener('keydown', onTriggerKeyDown));

  if (searchInput) {
    const onSearchInput = () => applySearch(searchInput.value);
    const onSearchKeyDown = (e) => {
      // Delegujemy nawigacyjne klawisze do trigger'a (active descendant
      // jest na trigger'ze per combobox spec).
      switch (e.key) {
        case 'ArrowDown':
        case 'ArrowUp':
        case 'Home':
        case 'End':
        case 'Enter':
        case 'Escape':
        case 'Tab':
          onTriggerKeyDown(e);
          return;
      }
    };
    searchInput.addEventListener('input', onSearchInput);
    searchInput.addEventListener('keydown', onSearchKeyDown);
    ctx.registerCleanup(() => {
      searchInput.removeEventListener('input', onSearchInput);
      searchInput.removeEventListener('keydown', onSearchKeyDown);
    });
  }

  // Click on option = commit. mousedown żeby ubiec blur-close.
  const onListboxMouseDown = (e) => {
    const li = e.target.closest('li[role="option"]');
    if (!li) return;
    e.preventDefault();
    const node = optionNodes.find((n) => n.el === li);
    if (!node) return;
    commit(node.idx);
  };
  listbox.addEventListener('mousedown', onListboxMouseDown);
  ctx.registerCleanup(() => listbox.removeEventListener('mousedown', onListboxMouseDown));

  // Outside click — close popover. Bazujemy na document-level listener
  // który sprawdza czy event.target jest pod wrapper'em.
  const onDocClick = (e) => {
    if (!isOpen) return;
    if (wrapper.contains(e.target)) return;
    close();
  };
  document.addEventListener('click', onDocClick);
  ctx.registerCleanup(() => document.removeEventListener('click', onDocClick));

  return wrapper;
}

// =============================================================================
// Rejestracja
// =============================================================================

export function registerFormSelectRenderers() {
  if (!lookupComponentRenderer(SELECT_TAG)) {
    registerComponentRenderer(SELECT_TAG, renderSelect);
  }
}
