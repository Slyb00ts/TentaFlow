// =============================================================================
// File: tf-keyvalue-editor.js
// Description: <tf-keyvalue-editor> — editable list of string key -> string
//       value pairs, rendered as repeatable rows (tf-input key, tf-input
//       value, tf-button remove) plus a trailing tf-button to add a row.
//       Light DOM (no Shadow DOM) so controls.css styles it like every other
//       tf-* primitive. Built for the generic node-config-form renderer's
//       JSON-schema `type: "object"` arm (additionalProperties: string), but
//       carries no flow-builder-specific knowledge — labels/placeholders are
//       supplied by the caller via attributes, same as <tf-tag-input>.
//       Attributes: disabled, key-placeholder, value-placeholder, add-label,
//       remove-label.
//       Property: .value — plain object, e.g. {"content-type": "text/plain"}.
//       Rows with an empty key or an empty value are dropped from `.value`
//       (a half-typed row stays visible while editing, never reaches config).
//       Event: "change" (detail: {value}), bubbles — fired after every row
//       edit, add and remove.
// Example:
//   <tf-keyvalue-editor key-placeholder="Key" value-placeholder="Value"></tf-keyvalue-editor>
//   el.value = { 'content-type': 'application/json' };
//   el.addEventListener('change', (e) => save(e.detail.value));
// =============================================================================

import '/js/components/tf-input.js';
import '/js/components/tf-button.js';

class TfKeyvalueEditor extends HTMLElement {
  static get observedAttributes() {
    return ['disabled', 'key-placeholder', 'value-placeholder', 'add-label', 'remove-label'];
  }

  constructor() {
    super();
    this._wrap = null;
    this._list = null;
    this._addBtn = null;
    // Row objects (not just the committed `.value` object) so a row with one
    // side typed and the other still empty survives an add/remove re-render
    // instead of being silently dropped.
    this._rows = [];
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    this._renderRows();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._wrap) return;
    if (name === 'disabled' || name === 'key-placeholder' || name === 'value-placeholder') {
      this._renderRows();
    } else if (name === 'add-label') {
      this._updateAddButton();
    }
  }

  get value() {
    const out = {};
    for (const row of this._rows) {
      const k = (row.key || '').trim();
      const v = row.value || '';
      if (k === '' || v === '') continue;
      out[k] = v;
    }
    return out;
  }

  set value(obj) {
    this._rows = obj && typeof obj === 'object'
      ? Object.entries(obj).map(([key, val]) => ({ key: String(key), value: String(val ?? '') }))
      : [];
    if (this._wrap) this._renderRows();
  }

  get disabled() { return this.hasAttribute('disabled'); }
  set disabled(v) { v ? this.setAttribute('disabled', '') : this.removeAttribute('disabled'); }

  _build() {
    this.innerHTML = '';
    const wrap = document.createElement('div');
    wrap.className = 'tf-kve';

    const list = document.createElement('div');
    list.className = 'tf-kve-rows';
    wrap.appendChild(list);

    const addBtn = document.createElement('tf-button');
    addBtn.setAttribute('variant', 'secondary');
    addBtn.setAttribute('size', 'sm');
    addBtn.setAttribute('icon', 'plus');
    addBtn.className = 'tf-kve-add';
    addBtn.addEventListener('click', () => this._addRow());
    wrap.appendChild(addBtn);

    this.appendChild(wrap);
    this._wrap = wrap;
    this._list = list;
    this._addBtn = addBtn;
    this._updateAddButton();

    // Delegated listeners: rows are rebuilt wholesale on every add/remove, so
    // binding once on the host (events bubble through light-DOM tf-input/
    // tf-button, same as <tf-tag-input>'s chip removal) avoids re-attaching a
    // fresh closure per row per render.
    this.addEventListener('change', (ev) => {
      const field = ev.target?.dataset?.field;
      if (field !== 'key' && field !== 'value') return;
      this._rows = this._readLiveRows();
      this._emitChange();
    });
    this.addEventListener('click', (ev) => {
      const btn = ev.target.closest?.('[data-action="remove-row"]');
      if (!btn) return;
      const rowEl = btn.closest('.tf-kve-row');
      const idx = Number(rowEl?.dataset.idx);
      if (Number.isInteger(idx)) this._removeAt(idx);
    });
  }

  _updateAddButton() {
    if (!this._addBtn) return;
    const label = this.getAttribute('add-label') || '';
    this._addBtn.textContent = label;
  }

  _addRow() {
    if (this.disabled) return;
    // Reads any in-flight typed values from the live DOM first so adding a
    // row never clobbers one the user started but has not blurred yet.
    this._rows = this._readLiveRows();
    this._rows.push({ key: '', value: '' });
    this._renderRows();
    const rows = this._list.querySelectorAll('.tf-kve-row [data-field="key"]');
    rows[rows.length - 1]?.focus?.();
  }

  _removeAt(index) {
    if (this.disabled) return;
    this._rows = this._readLiveRows();
    if (index < 0 || index >= this._rows.length) return;
    this._rows.splice(index, 1);
    this._renderRows();
    this._emitChange();
  }

  _readLiveRows() {
    const rows = [];
    this._list.querySelectorAll('.tf-kve-row').forEach((rowEl) => {
      const keyEl = rowEl.querySelector('[data-field="key"]');
      const valEl = rowEl.querySelector('[data-field="value"]');
      rows.push({ key: keyEl?.value ?? '', value: valEl?.value ?? '' });
    });
    return rows;
  }

  _renderRows() {
    const disabled = this.disabled;
    const keyPh = this.getAttribute('key-placeholder') || '';
    const valPh = this.getAttribute('value-placeholder') || '';
    const removeLabel = this.getAttribute('remove-label') || '';

    this._list.innerHTML = '';
    this._rows.forEach((row, idx) => {
      const rowEl = document.createElement('div');
      rowEl.className = 'tf-kve-row';
      rowEl.dataset.idx = String(idx);

      const keyInput = document.createElement('tf-input');
      keyInput.setAttribute('type', 'text');
      keyInput.className = 'tf-kve-key';
      keyInput.dataset.field = 'key';
      if (keyPh) keyInput.setAttribute('placeholder', keyPh);
      if (disabled) keyInput.setAttribute('disabled', '');
      keyInput.value = row.key;

      const valInput = document.createElement('tf-input');
      valInput.setAttribute('type', 'text');
      valInput.className = 'tf-kve-value';
      valInput.dataset.field = 'value';
      if (valPh) valInput.setAttribute('placeholder', valPh);
      if (disabled) valInput.setAttribute('disabled', '');
      valInput.value = row.value;

      rowEl.appendChild(keyInput);
      rowEl.appendChild(valInput);

      if (!disabled) {
        const removeBtn = document.createElement('tf-button');
        removeBtn.setAttribute('variant', 'ghost');
        removeBtn.setAttribute('size', 'sm');
        removeBtn.setAttribute('icon', 'trash');
        removeBtn.dataset.action = 'remove-row';
        if (removeLabel) removeBtn.setAttribute('title', removeLabel);
        rowEl.appendChild(removeBtn);
      }

      this._list.appendChild(rowEl);
    });

    if (this._addBtn) this._addBtn.toggleAttribute('disabled', disabled);
  }

  _emitChange() {
    this.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: this.value } }));
  }
}

customElements.define('tf-keyvalue-editor', TfKeyvalueEditor);
export { TfKeyvalueEditor };
