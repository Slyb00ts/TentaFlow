// =============================================================================
// Plik: tf-table.js
// Opis: Komponent <tf-table sortable selectable> z <tf-column key="..." label
//       renderer="text|chip|num" sortable>. Properties .rows (array) + .columns
//       (computed z dzieci). Emituje "row-click" i "sort".
//       Mobile (<=720px): td otrzymuja data-label dla widoku kart.
// Przyklad:
//   const t = document.createElement('tf-table');
//   t.innerHTML = '<tf-column key="name" label="Nazwa" sortable></tf-column>...';
//   t.rows = [{ name: 'x', status: 'ok' }, ...];
// =============================================================================

import { adoptControlsInto } from './shared-styles.js';

class TfColumn extends HTMLElement {
  // rola pamietaj-tagu — dane czerpane z atrybutow przez parenta
  connectedCallback() {
    this.style.display = 'none';
  }
}
customElements.define('tf-column', TfColumn);

class TfTable extends HTMLElement {
  static get observedAttributes() {
    return ['sortable', 'selectable'];
  }

  constructor() {
    super();
    this._shadow = this.attachShadow({ mode: 'open' });
    this._wrap = null;
    this._table = null;
    this._thead = null;
    this._tbody = null;
    this._rows = [];
    this._sortKey = null;
    this._sortDir = 'asc';
    this._onClick = this._onClick.bind(this);
  }

  connectedCallback() {
    if (!this._wrap) this._build();
    // render po ogarniciu <tf-column> dzieci
    this._render();
  }

  attributeChangedCallback() {
    if (this._wrap) this._render();
  }

  get rows() { return this._rows; }
  set rows(arr) {
    this._rows = Array.isArray(arr) ? arr.slice() : [];
    this._render();
  }

  get columns() {
    return Array.from(this.querySelectorAll('tf-column')).map((c) => ({
      key: c.getAttribute('key') || '',
      label: c.getAttribute('label') || '',
      sortable: c.hasAttribute('sortable'),
      renderer: (c.getAttribute('renderer') || 'text').toLowerCase(),
      align: (c.getAttribute('align') || '').toLowerCase(),
    }));
  }

  _build() {
    adoptControlsInto(this._shadow);
    const wrap = document.createElement('div');
    wrap.className = 'tf-table-wrap';
    const table = document.createElement('table');
    table.className = 'tf-table';
    const thead = document.createElement('thead');
    const tbody = document.createElement('tbody');
    table.appendChild(thead);
    table.appendChild(tbody);
    wrap.appendChild(table);
    this._shadow.appendChild(wrap);

    table.addEventListener('click', this._onClick);

    this._wrap = wrap;
    this._table = table;
    this._thead = thead;
    this._tbody = tbody;
  }

  // Sygnatura kolumn — sluzy do detekcji "kolumny sie nie zmienily" zeby
  // unikac rebuildu thead przy kazdym set rows / sort. thead trzymamy
  // wylacznie dla ARIA i sortowania, nie zalezy od liczby wierszy.
  _columnsSignature(cols) {
    return cols.map(c => `${c.key}|${c.label}|${c.sortable ? 1 : 0}|${c.renderer}|${c.align}`).join('');
  }

  _renderThead(cols, sortableTable) {
    const tr = document.createElement('tr');
    cols.forEach((col) => {
      const th = document.createElement('th');
      th.textContent = col.label;
      if (col.align === 'num' || col.renderer === 'num') th.classList.add('num');
      if (sortableTable && col.sortable) {
        th.classList.add('sortable');
        th.dataset.key = col.key;
      }
      tr.appendChild(th);
    });
    this._thead.replaceChildren(tr);
  }

  _updateSortIndicators() {
    const ths = this._thead.querySelectorAll('th.sortable');
    ths.forEach((th) => {
      th.classList.remove('sorted-asc', 'sorted-desc');
      if (th.dataset.key === this._sortKey) {
        th.classList.add(this._sortDir === 'asc' ? 'sorted-asc' : 'sorted-desc');
      }
    });
  }

  // Recyklinguje wiersze: aktualizuje istniejace `<tr>`/`<td>` zamiast je
  // burzyc. Eliminuje pelen rebuild tbody przy kazdym set rows / sort i
  // pozwala browserowi zachowac focus/selection w komorkach.
  _renderTbody(cols, rows) {
    const tbody = this._tbody;
    const existingRows = tbody.children;
    const target = rows.length;

    // 1) Update istniejacych tr w miejscu
    const reuseCount = Math.min(existingRows.length, target);
    for (let i = 0; i < reuseCount; i += 1) {
      const tr = existingRows[i];
      tr.dataset.idx = String(i);
      this._updateRowCells(tr, cols, rows[i]);
    }

    // 2) Dodaj brakujace
    if (target > existingRows.length) {
      const frag = document.createDocumentFragment();
      for (let i = existingRows.length; i < target; i += 1) {
        frag.appendChild(this._buildRow(cols, rows[i], i));
      }
      tbody.appendChild(frag);
    }

    // 3) Usun nadmiarowe od konca (szybsze niz removeChild w petli z poczatku)
    while (tbody.children.length > target) {
      tbody.removeChild(tbody.lastChild);
    }
  }

  _buildRow(cols, row, idx) {
    const rtr = document.createElement('tr');
    rtr.dataset.idx = String(idx);
    cols.forEach((col) => {
      const td = document.createElement('td');
      td.dataset.label = col.label;
      if (col.renderer === 'num' || col.align === 'num') td.classList.add('num');
      this._writeCell(td, col, row[col.key]);
      rtr.appendChild(td);
    });
    return rtr;
  }

  _updateRowCells(tr, cols, row) {
    const tds = tr.children;
    for (let i = 0; i < cols.length; i += 1) {
      const td = tds[i];
      if (!td) {
        // Brakujaca kolumna (zmiana liczby kolumn) — odbuduj caly wiersz.
        // Rzadki przypadek; szybka sciezka i tak zwykle dziala.
        tr.replaceChildren();
        cols.forEach((col) => {
          const fresh = document.createElement('td');
          fresh.dataset.label = col.label;
          if (col.renderer === 'num' || col.align === 'num') fresh.classList.add('num');
          this._writeCell(fresh, col, row[col.key]);
          tr.appendChild(fresh);
        });
        return;
      }
      this._writeCell(td, cols[i], row[cols[i].key]);
    }
  }

  _writeCell(td, col, value) {
    if (col.renderer === 'chip') {
      const chip = typeof value === 'object' && value
        ? value
        : { status: 'info', label: String(value ?? '') };
      td.innerHTML = `<span class="tf-chip ${chip.status || 'info'}">${chip.dot ? '<span class="tf-chip-dot"></span>' : ''}${chip.label ?? ''}</span>`;
    } else if (col.renderer === 'html') {
      const next = value ?? '';
      // Skip jesli identyczne — eliminuje koszt parsowania HTML komorki gdy
      // wiersz przyszedl niezmieniony z API (najczestsze w 2-sekundowym refreshu).
      if (td.innerHTML !== next) td.innerHTML = next;
    } else {
      const next = value ?? '';
      const txt = typeof next === 'string' ? next : String(next);
      if (td.textContent !== txt) td.textContent = txt;
    }
  }

  _render() {
    if (!this._thead) return;
    const cols = this.columns;
    const sortableTable = this.hasAttribute('sortable');
    const sig = this._columnsSignature(cols);

    if (sig !== this._lastColsSig) {
      this._renderThead(cols, sortableTable);
      this._lastColsSig = sig;
    }
    this._updateSortIndicators();

    const rows = this._sortedRows();
    this._renderTbody(cols, rows);
  }

  _sortedRows() {
    if (!this._sortKey) return this._rows;
    const key = this._sortKey;
    const dir = this._sortDir === 'asc' ? 1 : -1;
    return this._rows.slice().sort((a, b) => {
      const va = a[key];
      const vb = b[key];
      if (va == null && vb == null) return 0;
      if (va == null) return 1;
      if (vb == null) return -1;
      if (typeof va === 'number' && typeof vb === 'number') return (va - vb) * dir;
      return String(va).localeCompare(String(vb)) * dir;
    });
  }

  _onClick(e) {
    const th = e.target.closest('th.sortable');
    if (th) {
      const key = th.dataset.key;
      if (this._sortKey === key) {
        this._sortDir = this._sortDir === 'asc' ? 'desc' : 'asc';
      } else {
        this._sortKey = key;
        this._sortDir = 'asc';
      }
      this.dispatchEvent(new CustomEvent('sort', {
        bubbles: true,
        detail: { key: this._sortKey, dir: this._sortDir },
      }));
      this._render();
      return;
    }
    const tr = e.target.closest('tbody tr');
    if (!tr) return;
    const idx = parseInt(tr.dataset.idx, 10);
    if (this.hasAttribute('selectable')) {
      tr.classList.toggle('selected');
    }
    const row = this._sortedRows()[idx];
    this.dispatchEvent(new CustomEvent('row-click', {
      bubbles: true,
      detail: { row, index: idx, selected: tr.classList.contains('selected') },
    }));
  }
}

customElements.define('tf-table', TfTable);
export { TfTable, TfColumn };
