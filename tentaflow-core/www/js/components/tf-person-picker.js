// =============================================================================
// File: tf-person-picker.js
// Description: <tf-person-picker> — searchable listbox of people and (optionally)
//   agents for assigning, reassigning and handing work over. Each row shows an
//   avatar with initials, the name, the function, a load badge (>= 90 % warns,
//   > 100 % is "overloaded"), an absence tag and a suggestion tag. Suggested
//   people come first inside their group.
//
//   Data in:  `.items = [{ id, name, role?, initials?, kind?: 'person'|'agent',
//             load?: number, absence?: string, suggestion?: string,
//             disabled?: string }]` — every string arrives already formatted
//             and translated by the caller; `disabled` is the reason shown as
//             the row tooltip. `.value` = selected id (single) or ids (with the
//             `multiple` attribute).
//   Events:   `change` (detail: { value, items }) on every selection change,
//             `activate` (detail: { item }) on Enter or a double click.
//   Keyboard: the search box owns typing; ArrowDown enters the list, the list
//             moves with Up/Down/Home/End, Space selects (toggles when
//             multiple), Enter selects and activates, typing returns to search.
//
// Example:
//   const picker = document.createElement('tf-person-picker');
//   picker.items = [{ id: 'u1', name: 'Anna Kowalska', role: 'PM', load: 70 }];
//   picker.addEventListener('change', (e) => use(e.detail.value));
// =============================================================================

import { I18n } from '/js/i18n.js';
import { escapeHtml, escapeAttr } from '/js/utils.js';
import './tf-searchbox.js';
import './tf-avatar.js';
import './tf-badge.js';
import './tf-chip.js';
import './tf-empty-state.js';

const HIGH_LOAD = 90;
const OVER_LOAD = 100;

let nextUid = 1;

const t = (key, vars) => I18n.t(`actions.picker.${key}`, vars);

// Case- and diacritic-insensitive so "zielinski" finds "Zieliński".
function fold(text) {
  return String(text ?? '').normalize('NFD').replace(/[̀-ͯ]/g, '').toLowerCase();
}

function initialsOf(name) {
  const parts = String(name ?? '').trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return '';
  const first = parts[0][0];
  return (parts.length > 1 ? first + parts[parts.length - 1][0] : first).toUpperCase();
}

class TfPersonPicker extends HTMLElement {
  static get observedAttributes() { return ['multiple', 'label']; }

  constructor() {
    super();
    this._items = [];
    this._selected = new Set();
    this._query = '';
    this._visible = [];
    this._activeId = null;
    this._uid = nextUid++;
    this._built = false;
    this._onInput = this._onInput.bind(this);
    this._onKeyDown = this._onKeyDown.bind(this);
    this._onClick = this._onClick.bind(this);
    this._onDblClick = this._onDblClick.bind(this);
  }

  connectedCallback() {
    if (!this._built) this._build();
    this.addEventListener('input', this._onInput);
    this.addEventListener('keydown', this._onKeyDown);
    this.addEventListener('click', this._onClick);
    this.addEventListener('dblclick', this._onDblClick);
    this._render();
  }

  disconnectedCallback() {
    this.removeEventListener('input', this._onInput);
    this.removeEventListener('keydown', this._onKeyDown);
    this.removeEventListener('click', this._onClick);
    this.removeEventListener('dblclick', this._onDblClick);
  }

  attributeChangedCallback() {
    if (this._built) this._render();
  }

  get multiple() { return this.hasAttribute('multiple'); }

  get items() { return this._items; }
  set items(list) {
    this._items = Array.isArray(list) ? list.slice() : [];
    const known = new Set(this._items.map((it) => it.id));
    for (const id of [...this._selected]) if (!known.has(id)) this._selected.delete(id);
    if (this._built) this._render();
  }

  get value() {
    if (this.multiple) return [...this._selected];
    return this._selected.size ? [...this._selected][0] : null;
  }

  set value(v) {
    const ids = Array.isArray(v) ? v : (v == null || v === '' ? [] : [v]);
    const known = new Set(this._items.map((it) => it.id));
    this._selected = new Set(ids.filter((id) => known.has(id)).slice(0, this.multiple ? Infinity : 1));
    if (this._built) this._render();
  }

  get selectedItems() {
    return this._items.filter((it) => this._selected.has(it.id));
  }

  focusSearch() {
    this._searchInput()?.focus();
  }

  _searchInput() {
    return this.querySelector('tf-searchbox input');
  }

  _build() {
    this._built = true;
    this.innerHTML = `
      <tf-searchbox class="tf-pp__search" debounce="0" placeholder="${escapeAttr(t('search'))}"></tf-searchbox>
      <div class="tf-pp__list" role="listbox" tabindex="0"></div>
      <tf-empty-state class="tf-pp__empty" icon="users" title="${escapeAttr(t('empty'))}" hidden></tf-empty-state>`;
    this._list = this.querySelector('.tf-pp__list');
    this._empty = this.querySelector('.tf-pp__empty');
  }

  _matches(item) {
    if (!this._query) return true;
    const hay = fold([item.name, item.role, item.absence, item.suggestion].filter(Boolean).join(' '));
    return this._query.split(/\s+/).every((word) => hay.includes(word));
  }

  // Suggested people first inside a group; Array.sort is stable so the caller's
  // order (usually alphabetical) survives among equals.
  _ordered(kind) {
    return this._items
      .filter((it) => (it.kind === 'agent' ? 'agent' : 'person') === kind && this._matches(it))
      .sort((a, b) => Number(Boolean(b.suggestion)) - Number(Boolean(a.suggestion)));
  }

  _render() {
    const people = this._ordered('person');
    const agents = this._ordered('agent');
    this._visible = [...people, ...agents];
    const listLabel = this.getAttribute('label') || t('list_label');
    this._list.setAttribute('aria-label', listLabel);
    this._list.setAttribute('aria-multiselectable', this.multiple ? 'true' : 'false');
    const searchInput = this._searchInput();
    if (searchInput) searchInput.setAttribute('aria-label', t('search'));

    const grouped = people.length > 0 && agents.length > 0;
    const group = (name, rows) => {
      const html = rows.map((it) => this._rowHtml(it)).join('');
      if (!grouped) return html;
      return `<div role="group" aria-label="${escapeAttr(name)}"><div class="tf-pp__group" aria-hidden="true">${escapeHtml(name)}</div>${html}</div>`;
    };
    this._list.innerHTML = group(t('group_people'), people) + group(t('group_agents'), agents);

    if (!this._visible.some((it) => it.id === this._activeId)) {
      this._activeId = (this._visible.find((it) => this._selected.has(it.id) && !it.disabled)
        ?? this._visible.find((it) => !it.disabled))?.id ?? null;
    }
    this._syncActive();
    this._empty.hidden = this._visible.length > 0;
    this._list.hidden = this._visible.length === 0;
  }

  _rowId(item) {
    return `tf-pp-${this._uid}-${this._items.indexOf(item)}`;
  }

  _rowHtml(item) {
    const isAgent = item.kind === 'agent';
    const selected = this._selected.has(item.id);
    const avatar = isAgent
      ? `<span class="tf-pp__agent-icon" aria-hidden="true"><svg width="16" height="16"><use href="#i-bot"/></svg></span>`
      : `<tf-avatar size="sm" initials="${escapeAttr(item.initials || initialsOf(item.name))}"></tf-avatar>`;
    const tags = [];
    if (item.suggestion) tags.push(`<tf-chip variant="tag" tone="success" size="xs" label="${escapeAttr(item.suggestion)}"></tf-chip>`);
    if (item.absence) tags.push(`<tf-chip variant="tag" tone="warning" size="xs" label="${escapeAttr(item.absence)}"></tf-chip>`);
    const over = typeof item.load === 'number' && item.load > OVER_LOAD;
    if (over) tags.push(`<tf-chip variant="tag" tone="critical" size="xs" label="${escapeAttr(t('overloaded'))}"></tf-chip>`);
    let load = '';
    if (!isAgent && typeof item.load === 'number') {
      const tone = over ? 'danger' : item.load >= HIGH_LOAD ? 'warning' : 'neutral';
      load = `<tf-badge class="tf-pp__load" tone="${tone}" value="${item.load}%" title="${escapeAttr(t('load_title', { percent: item.load }))}"></tf-badge>`;
    }
    const classes = ['tf-pp__row'];
    if (selected) classes.push('is-selected');
    if (item.disabled) classes.push('is-disabled');
    return `<div class="${classes.join(' ')}" role="option" id="${this._rowId(item)}" data-id="${escapeAttr(item.id)}"
        aria-selected="${selected}"${item.disabled ? ` aria-disabled="true" title="${escapeAttr(item.disabled)}"` : ''}>
      ${avatar}
      <span class="tf-pp__main">
        <span class="tf-pp__name">${escapeHtml(item.name)}</span>
        ${item.role ? `<span class="tf-pp__role">${escapeHtml(item.role)}</span>` : ''}
        ${tags.length ? `<span class="tf-pp__tags">${tags.join('')}</span>` : ''}
      </span>
      ${load}
    </div>`;
  }

  _syncActive() {
    const active = this._items.find((it) => it.id === this._activeId);
    for (const row of this._list.querySelectorAll('.tf-pp__row')) {
      row.classList.toggle('is-active', active != null && row.id === this._rowId(active));
    }
    if (active && this._visible.includes(active)) {
      this._list.setAttribute('aria-activedescendant', this._rowId(active));
      this._list.querySelector('.is-active')?.scrollIntoView?.({ block: 'nearest' });
    } else {
      this._list.removeAttribute('aria-activedescendant');
    }
  }

  _itemById(id) {
    return this._items.find((it) => it.id === id);
  }

  _select(item) {
    if (!item || item.disabled) return;
    if (this.multiple) {
      if (this._selected.has(item.id)) this._selected.delete(item.id);
      else this._selected.add(item.id);
    } else {
      this._selected = new Set([item.id]);
    }
    this._activeId = item.id;
    this._render();
    this.dispatchEvent(new CustomEvent('change', {
      bubbles: true,
      detail: { value: this.value, items: this.selectedItems },
    }));
  }

  _move(step) {
    const enabled = this._visible.filter((it) => !it.disabled);
    if (!enabled.length) return;
    const at = enabled.findIndex((it) => it.id === this._activeId);
    const next = step === 'first' ? 0 : step === 'last' ? enabled.length - 1
      : Math.min(enabled.length - 1, Math.max(0, at + step));
    this._activeId = enabled[next].id;
    this._syncActive();
  }

  _onInput(e) {
    if (e.target !== this._searchInput()) return;
    this._query = fold(e.target.value).trim();
    this._render();
  }

  _onKeyDown(e) {
    const inSearch = e.target === this._searchInput();
    if (inSearch) {
      if (e.key === 'ArrowDown') {
        e.preventDefault();
        this._list.focus();
        if (this._activeId == null) this._move('first');
      } else if (e.key === 'Enter' && this._visible.length === 1) {
        e.preventDefault();
        this._commit(this._visible[0]);
      }
      return;
    }
    if (e.target !== this._list) return;
    const active = this._itemById(this._activeId);
    switch (e.key) {
      case 'ArrowDown': e.preventDefault(); this._move(1); break;
      case 'ArrowUp': {
        e.preventDefault();
        const first = this._visible.find((it) => !it.disabled);
        if (!active || active === first) this.focusSearch();
        else this._move(-1);
        break;
      }
      case 'Home': e.preventDefault(); this._move('first'); break;
      case 'End': e.preventDefault(); this._move('last'); break;
      case ' ': e.preventDefault(); this._select(active); break;
      case 'Enter': e.preventDefault(); this._commit(active); break;
      default:
        if (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey) this.focusSearch();
    }
  }

  // Enter picks the row and reports it: the owner decides whether that submits
  // the form or moves on to the next field.
  _commit(item) {
    if (!item || item.disabled) return;
    if (!(this.multiple && this._selected.has(item.id))) this._select(item);
    this.dispatchEvent(new CustomEvent('activate', { bubbles: true, detail: { item } }));
  }

  _rowFromEvent(e) {
    const row = e.target.closest?.('.tf-pp__row');
    return row ? this._items.find((it) => String(it.id) === row.dataset.id) ?? null : null;
  }

  _onClick(e) {
    const item = this._rowFromEvent(e);
    if (!item) return;
    this._select(item);
    this._list.focus();
  }

  _onDblClick(e) {
    const item = this._rowFromEvent(e);
    if (item && !item.disabled && !this.multiple) {
      this.dispatchEvent(new CustomEvent('activate', { bubbles: true, detail: { item } }));
    }
  }
}

customElements.define('tf-person-picker', TfPersonPicker);
export { TfPersonPicker };
