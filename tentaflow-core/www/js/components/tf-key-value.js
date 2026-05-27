// =============================================================================
// File: tf-key-value.js
// Description: <tf-key-value> — key-value display table. Set .entries to an
//              array of {key, value, chip?, chipTone?} to render rows.
// Example:
//   const kv = document.querySelector('tf-key-value');
//   kv.entries = [
//     { key: 'Status', value: 'Active', chip: 'OK', chipTone: 'ok' },
//     { key: 'Version', value: '1.2.3' },
//   ];
// =============================================================================

class TfKeyValue extends HTMLElement {
  constructor() {
    super();
    this._table = null;
    this._entries = [];
  }

  connectedCallback() {
    if (!this._table) this._build();
    this._render();
  }

  get entries() { return this._entries; }
  set entries(v) {
    this._entries = Array.isArray(v) ? v : [];
    if (this._table) this._render();
  }

  _build() {
    this.innerHTML = '';
    const table = document.createElement('table');
    table.className = 'tf-kv-table';
    this.appendChild(table);
    this._table = table;
  }

  _render() {
    const tbody = document.createElement('tbody');
    for (const entry of this._entries) {
      const tr = document.createElement('tr');

      const keyTd = document.createElement('td');
      keyTd.className = 'tf-kv-key';
      keyTd.textContent = entry.key || '';

      const valTd = document.createElement('td');
      valTd.className = 'tf-kv-value';

      const valText = document.createElement('span');
      valText.textContent = entry.value || '';
      valTd.appendChild(valText);

      if (entry.chip) {
        const chip = document.createElement('span');
        chip.className = `tf-chip ${entry.chipTone || 'info'}`;
        chip.textContent = entry.chip;
        valTd.appendChild(chip);
      }

      tr.appendChild(keyTd);
      tr.appendChild(valTd);
      tbody.appendChild(tr);
    }
    this._table.innerHTML = '';
    this._table.appendChild(tbody);
  }
}

customElements.define('tf-key-value', TfKeyValue);
export { TfKeyValue };
