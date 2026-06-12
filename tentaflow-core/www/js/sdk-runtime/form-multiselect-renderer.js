// =============================================================================
// Plik: sdk-runtime/form-multiselect-renderer.js
// Opis: Renderer MultiSelect (0x0304) — multi-value chip-based select z
// popover'em listbox (role=listbox + aria-multiselectable=true), klik
// opcji TOGGLE'uje zaznaczenie BEZ zamykania popover'a, trigger pokazuje
// chipy aktualnie zaznaczonych opcji (z indywidualnym × do usuwania),
// opcjonalne max_selections (blokuje dodawanie powyżej limitu),
// opcjonalne show_select_all (header w popover'ze: all/none w zależności
// od stanu, lub deaktywowane przy max_selections < liczba opcji).
//
// Selected_path w store to ARRAY zaznaczonych wartości (każda element ma
// shape SelectValue {kind, value}). Renderer jest read-only — toggle/clear
// emit'uje `change` z `{ value: SelectValue[], kind: 'array' }`; write-back
// dopina chunk 3.6.
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

/// Porównanie parsed SelectValue do store value (number lub bigint dla intów).
function selectValueEquals(parsed, storeValue) {
  if (parsed.tag === 'tstr') return typeof storeValue === 'string' && storeValue === parsed.value;
  if (parsed.tag === 'bool') return typeof storeValue === 'boolean' && storeValue === parsed.value;
  if (parsed.tag === 'u32' || parsed.tag === 'i32') {
    if (typeof storeValue === 'number' && storeValue === parsed.value) return true;
    if (typeof storeValue === 'bigint' && storeValue === BigInt(parsed.value)) return true;
  }
  return false;
}

/// Sprawdza czy SelectValue jest na liście zaznaczonych (raw store array).
function isOptionSelected(parsed, storeArray) {
  if (!Array.isArray(storeArray)) return false;
  return storeArray.some((sv) => {
    // Akceptujemy zarówno `SelectValue` (tagged object) jak i raw value
    // — capable host może zapisywać oba kształty. Normalizujemy do
    // primitive porównania.
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

function applyTextBind(element, bindRef, ctx) {
  const apply = () => {
    const v = resolveBindRef(bindRef, ctx.store);
    element.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(bindRef, ctx.store, apply));
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
  if (groups) {
    const ids = new Set(groups.map((g) => g.id));
    for (let i = 0; i < options.length; i++) {
      const opt = options[i];
      if (opt.groupId != null && !ids.has(opt.groupId)) {
        throw new TypeError(`MultiSelect.options[${i}].group_id '${opt.groupId}' nie ma w groups`);
      }
    }
  }
  const maxSelectionsRaw = ctx.readField(component.fields, 10);
  const maxSelections = maxSelectionsRaw == null ? null : requireU32(maxSelectionsRaw, 'MultiSelect.max_selections');
  if (maxSelections != null && maxSelections === 0) {
    throw new TypeError('MultiSelect.max_selections must be > 0 if set');
  }
  const showSelectAll = requireBool(ctx.readField(component.fields, 11), 'MultiSelect.show_select_all');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-multiselect');
  wrapper.classList.add(`tf-multiselect--size-${size}`);
  if (virtualize) wrapper.classList.add('tf-multiselect--virtualize');

  let labelEl = null;
  // div role=combobox nie odpowiada na <label for=...>; używamy <div>+
  // aria-labelledby dla powiązania semantycznego. Element wygląda jak
  // label, więc CSS daje "label-like" styling.
  let labelDomId = null;
  if (labelBind != null) {
    labelEl = document.createElement('div');
    labelEl.classList.add('tf-multiselect__label');
    labelDomId = `tf-multiselect-${component.id}-label`;
    labelEl.setAttribute('id', labelDomId);
    applyTextBind(labelEl, labelBind, ctx);
    wrapper.appendChild(labelEl);
  }

  // Trigger to <div role="combobox"> — NIE <button> — bo musi zawierać
  // nested interaktywne elementy (chipy z × i clear). Button-in-button
  // jest niepoprawnym HTML i łamie a11y/focus. Tabindex=0 zapewnia
  // focusability; klik/keyboard handler'y na div'ie obsługują interakcję.
  const trigger = document.createElement('div');
  trigger.setAttribute('role', 'combobox');
  trigger.setAttribute('aria-haspopup', 'listbox');
  trigger.setAttribute('aria-expanded', 'false');
  trigger.setAttribute('tabindex', '0');
  trigger.classList.add('tf-multiselect__trigger');
  const triggerId = `tf-multiselect-${component.id}`;
  trigger.setAttribute('id', triggerId);
  if (labelDomId) trigger.setAttribute('aria-labelledby', labelDomId);

  if (labelBind == null) {
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
      if (typeof v === 'string' && v.trim().length > 0) {
        trigger.setAttribute('aria-label', v);
      } else {
        trigger.removeAttribute('aria-label');
      }
    };
    applyAriaLabel();
    ctx.registerCleanup(subscribeBindRef(component.a11y.label, ctx.store, applyAriaLabel));
  }

  // Trigger zawiera chips area + (opcjonalnie) clear button + caret.
  const chipsArea = document.createElement('span');
  chipsArea.classList.add('tf-multiselect__chips');
  trigger.appendChild(chipsArea);

  let clearButton = null;
  if (clearable) {
    clearButton = document.createElement('button');
    clearButton.setAttribute('type', 'button');
    clearButton.classList.add('tf-multiselect__clear');
    clearButton.setAttribute('aria-label', 'Clear all selections');
    clearButton.textContent = '×';
    clearButton.hidden = true;
    trigger.appendChild(clearButton);
  }

  const caret = document.createElement('span');
  caret.classList.add('tf-multiselect__caret');
  caret.setAttribute('aria-hidden', 'true');
  caret.textContent = '▾';
  trigger.appendChild(caret);

  // Disabled na <div role=combobox>: aria-disabled, brak tabindex, plus
  // synchronizacja `disabled` na nested <button>'ach (clear oraz chip ×).
  // Chipy są re-render'owane, więc disabled state aplikujemy dynamicznie w
  // refreshChips() — tu trzymamy referencyjny stan.
  let disabledActive = false;
  // Trzymamy referencję do `refreshSelectAll` przez zmienną lazy-bind, bo
  // ta funkcja jest zdefiniowana niżej — TDZ blokuje bezpośredni access.
  let refreshSelectAllRef = null;
  const syncNestedDisabled = () => {
    if (clearButton) clearButton.disabled = disabledActive;
    chipsArea.querySelectorAll('.tf-multiselect__chip-remove').forEach((b) => {
      b.disabled = disabledActive;
    });
    if (refreshSelectAllRef != null) refreshSelectAllRef();
  };
  const isDisabledFn = (() => {
    if (disabledBind == null) return () => false;
    const apply = () => {
      disabledActive = resolveBindRef(disabledBind, ctx.store) === true;
      if (disabledActive) {
        trigger.setAttribute('aria-disabled', 'true');
        trigger.setAttribute('data-disabled', '');
        trigger.removeAttribute('tabindex');
      } else {
        trigger.removeAttribute('aria-disabled');
        trigger.removeAttribute('data-disabled');
        trigger.setAttribute('tabindex', '0');
      }
      syncNestedDisabled();
    };
    apply();
    ctx.registerCleanup(subscribeBindRef(disabledBind, ctx.store, apply));
    return () => disabledActive;
  })();

  wrapper.appendChild(trigger);

  // Popover.
  const popover = document.createElement('div');
  popover.classList.add('tf-multiselect__popover');
  popover.hidden = true;
  const popoverId = `${triggerId}-popover`;
  popover.setAttribute('id', popoverId);
  trigger.setAttribute('aria-controls', popoverId);

  let searchInput = null;
  let searchQuery = '';
  if (searchable) {
    searchInput = document.createElement('input');
    searchInput.setAttribute('type', 'text');
    searchInput.classList.add('tf-multiselect__search');
    searchInput.setAttribute('aria-autocomplete', 'list');
    popover.appendChild(searchInput);
  }

  // Optional select-all header.
  let selectAllBtn = null;
  if (showSelectAll) {
    selectAllBtn = document.createElement('button');
    selectAllBtn.setAttribute('type', 'button');
    selectAllBtn.classList.add('tf-multiselect__select-all');
    // Tekst aktualizowany w refreshSelectAll() ze stanu zaznaczeń.
    selectAllBtn.textContent = '';
    popover.appendChild(selectAllBtn);
  }

  const listbox = document.createElement('ul');
  listbox.setAttribute('role', 'listbox');
  listbox.setAttribute('aria-multiselectable', 'true');
  listbox.classList.add('tf-multiselect__listbox');
  popover.appendChild(listbox);
  wrapper.appendChild(popover);

  // ---- option DOM build ----
  const optionNodes = [];
  const renderOption = (opt, idx, container) => {
    const li = document.createElement('li');
    li.setAttribute('role', 'option');
    li.classList.add('tf-multiselect__option');
    li.setAttribute('id', `${triggerId}-opt-${idx}`);
    if (opt.disabled) {
      li.setAttribute('aria-disabled', 'true');
      li.classList.add('tf-multiselect__option--disabled');
    }
    // Checkbox-style indicator (czysto wizualny — selection state lecimy
    // przez aria-selected, ale wizualnie pokazujemy ✓).
    const check = document.createElement('span');
    check.classList.add('tf-multiselect__option-check');
    check.setAttribute('aria-hidden', 'true');
    check.textContent = '';
    li.appendChild(check);
    if (opt.icon) {
      const iconEl = renderIcon(opt.icon, `MultiSelect.options[${idx}].icon`);
      iconEl.classList.add('tf-multiselect__option-icon');
      li.appendChild(iconEl);
    }
    const lblEl = document.createElement('span');
    lblEl.classList.add('tf-multiselect__option-label');
    applyTextBind(lblEl, opt.label, ctx);
    li.appendChild(lblEl);
    if (opt.description) {
      const descEl = document.createElement('span');
      descEl.classList.add('tf-multiselect__option-description');
      applyTextBind(descEl, opt.description, ctx);
      li.appendChild(descEl);
    }
    container.appendChild(li);
    optionNodes.push({ el: li, opt, idx, visible: true, check });
  };

  if (groups) {
    const groupContainers = new Map();
    for (const g of groups) {
      const groupBlock = document.createElement('li');
      groupBlock.setAttribute('role', 'group');
      groupBlock.setAttribute('aria-labelledby', `${triggerId}-grp-${g.id}`);
      groupBlock.classList.add('tf-multiselect__group');
      const header = document.createElement('div');
      header.classList.add('tf-multiselect__group-header');
      header.setAttribute('id', `${triggerId}-grp-${g.id}`);
      applyTextBind(header, g.label, ctx);
      groupBlock.appendChild(header);
      const inner = document.createElement('ul');
      inner.setAttribute('role', 'presentation');
      inner.classList.add('tf-multiselect__group-list');
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

  // ---- state ----
  let activeIdx = -1;
  let isOpen = false;

  const readSelected = () => {
    let arr;
    try { arr = ctx.store.read(selectedPath); } catch { arr = undefined; }
    return Array.isArray(arr) ? arr : [];
  };

  /// Index list zaznaczonych opcji (po index'ach w `options`).
  const selectedIndices = () => {
    const sel = readSelected();
    if (sel.length === 0) return [];
    const out = [];
    for (let i = 0; i < options.length; i++) {
      if (isOptionSelected(options[i].value, sel)) out.push(i);
    }
    return out;
  };

  const refreshChips = () => {
    chipsArea.innerHTML = '';
    const indices = selectedIndices();
    if (indices.length === 0) {
      chipsArea.classList.add('tf-multiselect__chips--empty');
      if (placeholderBind != null) {
        const v = resolveBindRef(placeholderBind, ctx.store);
        const ph = document.createElement('span');
        ph.classList.add('tf-multiselect__placeholder');
        ph.textContent = v == null ? '' : String(v);
        chipsArea.appendChild(ph);
      }
      if (clearButton) clearButton.hidden = true;
      return;
    }
    chipsArea.classList.remove('tf-multiselect__chips--empty');
    for (const i of indices) {
      const chip = document.createElement('span');
      chip.classList.add('tf-multiselect__chip');
      chip.setAttribute('data-option-idx', String(i));
      const chipLabel = document.createElement('span');
      chipLabel.classList.add('tf-multiselect__chip-label');
      const v = resolveBindRef(options[i].label, ctx.store);
      chipLabel.textContent = v == null ? '' : String(v);
      chip.appendChild(chipLabel);
      // Individual remove × na chipie.
      const rm = document.createElement('button');
      rm.setAttribute('type', 'button');
      rm.classList.add('tf-multiselect__chip-remove');
      rm.setAttribute('aria-label', `Remove ${chipLabel.textContent}`);
      rm.setAttribute('tabindex', '-1');
      rm.setAttribute('data-option-idx', String(i));
      rm.textContent = '×';
      if (disabledActive) rm.disabled = true;
      chip.appendChild(rm);
      chipsArea.appendChild(chip);
    }
    if (clearButton) clearButton.hidden = false;
  };

  const refreshAriaSelected = () => {
    const sel = readSelected();
    for (const n of optionNodes) {
      const isSel = isOptionSelected(n.opt.value, sel);
      if (isSel) {
        n.el.setAttribute('aria-selected', 'true');
        n.el.classList.add('tf-multiselect__option--selected');
        n.check.textContent = '✓';
      } else {
        n.el.removeAttribute('aria-selected');
        n.el.classList.remove('tf-multiselect__option--selected');
        n.check.textContent = '';
      }
    }
  };

  const refreshSelectAll = () => {
    if (!selectAllBtn) return;
    const enabledOptions = options.filter((o) => !o.disabled);
    // Reactive component-level disabled zawsze wygrywa nad computed state.
    if (disabledActive) {
      selectAllBtn.disabled = true;
      selectAllBtn.textContent = 'Select all';
      selectAllBtn.dataset.mode = 'noop';
      return;
    }
    if (enabledOptions.length === 0) {
      selectAllBtn.disabled = true;
      selectAllBtn.textContent = 'Select all';
      return;
    }
    const sel = readSelected();
    const allSelected = enabledOptions.every((o) => isOptionSelected(o.value, sel));
    // Jeśli max_selections < liczba enabled options, "Select all" nie ma sensu
    // przy stanie all=false (nie mieści wszystkich) — pokaż "Clear" jak coś
    // zaznaczone, inaczej deaktywuj.
    const anySelected = enabledOptions.some((o) => isOptionSelected(o.value, sel));
    if (allSelected) {
      selectAllBtn.disabled = false;
      selectAllBtn.textContent = 'Clear all';
      selectAllBtn.dataset.mode = 'clear';
    } else if (maxSelections != null && enabledOptions.length > maxSelections) {
      selectAllBtn.disabled = !anySelected;
      selectAllBtn.textContent = anySelected ? 'Clear all' : 'Select all';
      selectAllBtn.dataset.mode = anySelected ? 'clear' : 'noop';
    } else {
      selectAllBtn.disabled = false;
      selectAllBtn.textContent = 'Select all';
      selectAllBtn.dataset.mode = 'all';
    }
  };

  // Po definicji refreshSelectAll możemy podpiąć ref dla syncNestedDisabled.
  refreshSelectAllRef = refreshSelectAll;

  const refreshAll = () => {
    refreshChips();
    refreshAriaSelected();
    refreshSelectAll();
  };

  refreshAll();
  ctx.registerCleanup(ctx.store.subscribe(selectedPath, refreshAll));
  if (placeholderBind != null) {
    ctx.registerCleanup(subscribeBindRef(placeholderBind, ctx.store, refreshChips));
  }

  // ---- emit helpers ----
  const emitChange = (nextIndices) => {
    const detailArr = nextIndices.map((i) => ({
      kind: options[i].value.tag,
      value: options[i].value.value,
    }));
    wrapper.dispatchEvent(
      new (globalThis.CustomEvent || globalThis.Event)('change', {
        bubbles: false,
        detail: { value: detailArr, kind: 'array' },
      })
    );
  };

  const toggle = (idx) => {
    if (isDisabledFn()) { close(); return; }
    const opt = optionNodes[idx]?.opt;
    if (!opt || opt.disabled) return;
    const current = selectedIndices();
    const inIdx = current.indexOf(idx);
    let next;
    if (inIdx >= 0) {
      next = current.filter((i) => i !== idx);
    } else {
      if (maxSelections != null && current.length >= maxSelections) return;
      next = [...current, idx];
    }
    emitChange(next);
  };

  const removeChip = (idx) => {
    if (isDisabledFn()) return;
    const current = selectedIndices();
    if (current.indexOf(idx) < 0) return;
    emitChange(current.filter((i) => i !== idx));
  };

  const clearAll = () => {
    if (isDisabledFn()) return;
    if (selectedIndices().length === 0) return;
    emitChange([]);
  };

  // ---- active descendant / keyboard nav ----
  const visibleNodes = () => optionNodes.filter((n) => n.visible && !n.opt.disabled);
  const setActive = (idx) => {
    activeIdx = idx;
    for (const n of optionNodes) n.el.classList.remove('tf-multiselect__option--active');
    if (idx < 0) {
      trigger.removeAttribute('aria-activedescendant');
      return;
    }
    const n = optionNodes[idx];
    n.el.classList.add('tf-multiselect__option--active');
    trigger.setAttribute('aria-activedescendant', n.el.id);
    if (typeof n.el.scrollIntoView === 'function') {
      try { n.el.scrollIntoView({ block: 'nearest' }); } catch {}
    }
  };
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
    if (groups) {
      for (const g of groups) {
        const visible = optionNodes.some((n) => n.visible && n.opt.groupId === g.id);
        const headerEl = document.getElementById(`${triggerId}-grp-${g.id}`);
        if (headerEl && headerEl.parentElement) headerEl.parentElement.hidden = !visible;
      }
    }
    const cur = optionNodes[activeIdx];
    if (!cur || !cur.visible || cur.opt.disabled) {
      const vis = visibleNodes();
      setActive(vis.length > 0 ? vis[0].idx : -1);
    }
  };

  // ---- open/close ----
  const open = () => {
    if (isOpen || isDisabledFn()) return;
    isOpen = true;
    popover.hidden = false;
    trigger.setAttribute('aria-expanded', 'true');
    wrapper.classList.add('tf-multiselect--open');
    const vis = visibleNodes();
    setActive(vis.length > 0 ? vis[0].idx : -1);
    if (searchInput) {
      searchInput.value = searchQuery;
      try { searchInput.focus(); } catch {}
    }
  };

  const close = () => {
    if (!isOpen) return;
    isOpen = false;
    popover.hidden = true;
    trigger.setAttribute('aria-expanded', 'false');
    wrapper.classList.remove('tf-multiselect--open');
    trigger.removeAttribute('aria-activedescendant');
    activeIdx = -1;
    for (const n of optionNodes) n.el.classList.remove('tf-multiselect__option--active');
    try { trigger.focus(); } catch {}
  };

  // ---- event wiring ----
  const onTriggerClick = (e) => {
    if (isDisabledFn()) return;
    if (clearButton && e.target === clearButton) return;
    if (e.target.classList?.contains('tf-multiselect__chip-remove')) return;
    e.preventDefault();
    if (isOpen) close(); else open();
  };
  trigger.addEventListener('click', onTriggerClick);
  ctx.registerCleanup(() => trigger.removeEventListener('click', onTriggerClick));

  if (clearButton) {
    const onClearClick = (e) => {
      e.preventDefault();
      e.stopPropagation();
      clearAll();
    };
    clearButton.addEventListener('click', onClearClick);
    ctx.registerCleanup(() => clearButton.removeEventListener('click', onClearClick));
  }

  // Chip × remove — delegacja na chipsArea (chipy są re-render'owane przy
  // każdej zmianie selection, więc listener musi być na trwałym kontenerze).
  const onChipsClick = (e) => {
    const rm = e.target.closest('.tf-multiselect__chip-remove');
    if (!rm) return;
    e.preventDefault();
    e.stopPropagation();
    const idx = Number(rm.getAttribute('data-option-idx'));
    if (Number.isInteger(idx)) removeChip(idx);
  };
  chipsArea.addEventListener('click', onChipsClick);
  ctx.registerCleanup(() => chipsArea.removeEventListener('click', onChipsClick));

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
          toggle(activeIdx);
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

  // Listbox click toggle (mousedown ubiega blur).
  const onListboxMouseDown = (e) => {
    const li = e.target.closest('li[role="option"]');
    if (!li) return;
    e.preventDefault();
    const node = optionNodes.find((n) => n.el === li);
    if (!node) return;
    toggle(node.idx);
  };
  listbox.addEventListener('mousedown', onListboxMouseDown);
  ctx.registerCleanup(() => listbox.removeEventListener('mousedown', onListboxMouseDown));

  if (selectAllBtn) {
    const onSelectAll = (e) => {
      e.preventDefault();
      e.stopPropagation();
      if (selectAllBtn.disabled || isDisabledFn()) return;
      const mode = selectAllBtn.dataset.mode;
      if (mode === 'clear') {
        clearAll();
      } else if (mode === 'all') {
        const next = [];
        for (let i = 0; i < options.length; i++) {
          if (!options[i].disabled) next.push(i);
        }
        emitChange(next);
      }
    };
    selectAllBtn.addEventListener('click', onSelectAll);
    ctx.registerCleanup(() => selectAllBtn.removeEventListener('click', onSelectAll));
  }

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

export function registerFormMultiSelectRenderers() {
  if (!lookupComponentRenderer(MULTISELECT_TAG)) {
    registerComponentRenderer(MULTISELECT_TAG, renderMultiSelect);
  }
}
