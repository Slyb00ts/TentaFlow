// =============================================================================
// Plik: tf-table.js
// Opis: Komponent <tf-table sortable selectable> z <tf-column key="..." label
//       renderer="text|chip|num" sortable sticky hide-below="900" fill nowrap
//       width="40%" priority="low">.
//       `hide-below` ukrywa kolumne ponizej podanej szerokosci viewportu
//       (dozwolone: 480 640 720 900 1024 1180 1280 — regula zyje w controls.css,
//       a media query nie czyta zmiennej CSS). Komorki zostaja w DOM, wiec stan
//       tabeli (zaznaczenie, ekspansja, sort) przezywa zmiane szerokosci.
//       `fill` marks the one column that absorbs free width
//       and ellipsises (flush variant), `width` pins a column width (any CSS
//       length) so stacked tables share one template, `priority="low"` hides
//       the column on phones (flush variant, <=480px). Atrybut `narrow` na
//       tf-table (flush) = tabela w waskiej karcie: kolumna fill traci swoje
//       minimum, paski udzialu sie kurcza, a na telefonie szablon procentowy
//       zostaje (tabela miesci sie w karcie zamiast przewijac).
//       Properties .rows (array;
//       a row's optional `_class` adds modifier classes to its <tr>) +
//       .columns (computed z dzieci). Emituje "row-click", "row-dblclick",
//       "sort", "select-all", "row-expand" i "page-change".
//       Paginacja (server-side): atrybuty page-size / total / page (1-based).
//       Gdy total > page-size, pod tabela renderuje sie pasek stronicowania;
//       klik prev/next emituje "page-change" {page, pageSize} — host laduje
//       nowa strone i aktualizuje atrybuty `page` + .rows.
//       Mobile (<=720px): td otrzymuja data-label dla widoku kart.
//       variant="flush": bez ramki wrapa (karta hosta rysuje ramke), wiersze
//       klikalne, na mobile tabela NIE zwija sie do kart — wrap przewija sie
//       poziomo wewnatrz karty.
// Przyklad:
//   const t = document.createElement('tf-table');
//   t.innerHTML = '<tf-column key="name" label="Nazwa" sortable></tf-column>...';
//   t.rows = [{ name: 'x', status: 'ok' }, ...];
// =============================================================================

import { adoptControlsInto, injectSpriteIntoShadow } from './shared-styles.js';

class TfColumn extends HTMLElement {
  // rola pamietaj-tagu — dane czerpane z atrybutow przez parenta
  connectedCallback() {
    this.style.display = 'none';
  }
}
customElements.define('tf-column', TfColumn);

// Breakpoints supported by <tf-column hide-below="…">. The rule has to live in
// controls.css (the only sheet adopted into the shadow root) and a media query
// cannot read a custom property, so the scale is finite by construction. A value
// outside this set leaves the column visible rather than guessing a neighbour.
const HIDE_BELOW_BREAKPOINTS = new Set([480, 640, 720, 900, 1024, 1180, 1280]);

function hideBelowOf(el) {
  const raw = parseInt(el.getAttribute('hide-below') || '', 10);
  return HIDE_BELOW_BREAKPOINTS.has(raw) ? raw : 0;
}

// Szerokosc komorek sticky uzywana do skladania offsetu `left` kolejnych
// przypietych kolumn. Bez znanego layoutu tabela nie zna realnych szerokosci,
// wiec stosujemy stala bazowa — wystarczy do wizualnego przypiecia bez nakladki.
const STICKY_COLUMN_WIDTH = 160;

class TfTable extends HTMLElement {
  static get observedAttributes() {
    return ['sortable', 'selectable', 'variant', 'density', 'narrow', 'page-size', 'total', 'page', 'actions-label'];
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
    // Optional per-row actions builder: (row, index) => Element | null.
    // When set, tf-table renders a trailing actions column hosting the
    // returned element (e.g. a kebab tf-menu). Cells are rebuilt on every
    // render so the element stays bound to its current row object.
    this._rowActions = null;
    // Count of leading columns pinned with position:sticky. Per-column sticky
    // flags (<tf-column sticky>) extend this for explicitly marked columns.
    this._stickyColumns = 0;
    // When true, a leading expand toggle column is rendered; clicking it emits
    // "row-expand" and renders the builder output in an inserted expansion row.
    this._expandable = false;
    // Optional expansion-region builder: (row, index) => Element | null.
    this._expandRenderer = null;
    // Optional row-object field holding a STABLE per-row identity. When set,
    // expansion state is keyed by that id so sort/page changes do not move the
    // expansion panel to whatever row now sits at a given visible index. When
    // unset the table falls back to keying expansion by visible row index.
    this._rowKey = null;
    // Expanded rows, keyed by stable row identity (see _rowIdentity).
    this._expandedRows = new Set();
    this._onClick = this._onClick.bind(this);
    this._onDblClick = this._onDblClick.bind(this);
    this._onChange = this._onChange.bind(this);
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

  get rowActions() { return this._rowActions; }
  set rowActions(fn) {
    this._rowActions = typeof fn === 'function' ? fn : null;
    // Column count changes when actions toggle on/off — force thead rebuild.
    this._lastColsSig = null;
    this._render();
  }

  get stickyColumns() { return this._stickyColumns; }
  set stickyColumns(n) {
    const count = Number.isInteger(n) && n > 0 ? n : 0;
    this._stickyColumns = count;
    this._lastColsSig = null;
    this._render();
  }

  get expandable() { return this._expandable; }
  set expandable(v) {
    this._expandable = !!v;
    this._lastColsSig = null;
    this._render();
  }

  get expandRenderer() { return this._expandRenderer; }
  set expandRenderer(fn) {
    this._expandRenderer = typeof fn === 'function' ? fn : null;
    this._render();
  }

  get rowKey() { return this._rowKey; }
  set rowKey(field) {
    this._rowKey = typeof field === 'string' && field.length > 0 ? field : null;
    this._render();
  }

  // Stable identity for a row. Uses the configured rowKey field when present and
  // the value is a string/number; otherwise falls back to the visible index so
  // tables without a key keep their previous index-based expansion behaviour.
  _rowIdentity(row, idx) {
    if (this._rowKey != null && row != null && typeof row === 'object') {
      const v = row[this._rowKey];
      if (typeof v === 'string' || typeof v === 'number') return `k:${v}`;
    }
    return `i:${idx}`;
  }

  get columns() {
    return Array.from(this.querySelectorAll('tf-column')).map((c) => ({
      key: c.getAttribute('key') || '',
      label: c.getAttribute('label') || '',
      sortable: c.hasAttribute('sortable'),
      renderer: (c.getAttribute('renderer') || 'text').toLowerCase(),
      align: (c.getAttribute('align') || '').toLowerCase(),
      sticky: c.hasAttribute('sticky'),
      hideBelow: hideBelowOf(c),
      fill: c.hasAttribute('fill'),
      nowrap: c.hasAttribute('nowrap'),
      width: c.getAttribute('width') || '',
      lowPriority: (c.getAttribute('priority') || '').toLowerCase() === 'low',
    }));
  }

  // Hiding is a CSS concern: the cells stay in the DOM, so selection, expansion,
  // sort and the recycled-row bookkeeping survive a viewport change untouched.
  // Idempotent — recycled cells drop a stale breakpoint before taking the new one.
  _applyHideBelow(cell, col) {
    if (cell.classList.length) {
      for (const cls of [...cell.classList]) {
        if (cls.startsWith('tf-table__col--hide-below-')) cell.classList.remove(cls);
      }
    }
    if (col.hideBelow) cell.classList.add(`tf-table__col--hide-below-${col.hideBelow}`);
  }

  // The card view (<=720px) draws the column label above each value from this
  // attribute. A column may declare no label — a summary cell that already
  // reads as a sentence — and then no caption line is rendered at all.
  // Idempotent, so a recycled cell drops a stale label.
  _applyCardLabel(cell, col) {
    if (col.label) cell.dataset.label = col.label;
    else delete cell.dataset.label;
  }

  // Indeksy kolumn (sposrod realnie renderowanych <td>, BEZ kolumny expand)
  // ktore maja byc przypiete: pierwsze N (stickyColumns) plus per-kolumna sticky.
  _stickyColumnIndices(cols) {
    const set = new Set();
    for (let i = 0; i < cols.length; i += 1) {
      if (i < this._stickyColumns || cols[i].sticky) set.add(i);
    }
    return set;
  }

  // Offset `left` dla i-tej przypietej kolumny danych. Kolumna expand (gdy jest)
  // zajmuje pierwsza pozycje, wiec kolumny danych zaczynaja sie za nia.
  _stickyLeft(colIndex) {
    const lead = this._expandable ? STICKY_COLUMN_WIDTH : 0;
    return `${lead + colIndex * STICKY_COLUMN_WIDTH}px`;
  }

  _applySticky(cell, colIndex) {
    cell.classList.add('tf-table__sticky-col');
    cell.style.position = 'sticky';
    cell.style.left = this._stickyLeft(colIndex);
    cell.style.zIndex = '1';
  }

  _build() {
    adoptControlsInto(this._shadow);
    // Row-action tf-buttons render <use href="#i-*"> — the document sprite is
    // not reachable from inside the shadow root, so clone it in.
    injectSpriteIntoShadow(this._shadow);
    const wrap = document.createElement('div');
    wrap.className = 'tf-table-wrap';
    const table = document.createElement('table');
    table.className = 'tf-table';
    // Handlery byly zbindowane w konstruktorze, ale nigdy nie podlaczone:
    // sortowanie naglowka, row-click/row-dblclick, rozwijanie wierszy i akcje
    // wiersza nie reagowaly w ZADNEJ tabeli dashboardu. "change" dodatkowo nie
    // jest composed, wiec nasluch musi siedziec tutaj, w shadow root.
    table.addEventListener('click', this._onClick);
    table.addEventListener('dblclick', this._onDblClick);
    table.addEventListener('change', this._onChange);
    const thead = document.createElement('thead');
    const tbody = document.createElement('tbody');
    table.appendChild(thead);
    table.appendChild(tbody);
    wrap.appendChild(table);
    this._shadow.appendChild(wrap);

    // Pager lives in the shadow root, so controls.css cannot be extended for
    // it from the outside — a small scoped stylesheet keeps it self-contained.
    const pagerStyle = document.createElement('style');
    pagerStyle.textContent = `
      .tf-table__pager {
        display: flex;
        align-items: center;
        justify-content: flex-end;
        gap: 8px;
        padding: 8px 4px 2px;
        font-size: 11.5px;
        color: var(--text-3, #8a8f98);
      }
      .tf-table__pager[hidden] { display: none; }
      .tf-table__page-btn {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 24px;
        height: 24px;
        border: 1px solid var(--border, #333);
        border-radius: var(--radius-sm, 6px);
        background: transparent;
        color: var(--text-2, #b8bcc4);
        cursor: pointer;
        font: inherit;
        line-height: 1;
      }
      .tf-table__page-btn:hover:not(:disabled) {
        border-color: var(--border-hover, #555);
        color: var(--text, #e6e8ec);
      }
      .tf-table__page-btn:disabled { opacity: 0.4; cursor: default; }
    `;
    this._shadow.appendChild(pagerStyle);

    const pager = document.createElement('div');
    pager.className = 'tf-table__pager';
    pager.hidden = true;
    const range = document.createElement('span');
    range.className = 'tf-table__page-range';
    const prev = document.createElement('button');
    prev.type = 'button';
    prev.className = 'tf-table__page-btn';
    prev.dataset.page = 'prev';
    prev.setAttribute('aria-label', 'Poprzednia strona');
    prev.textContent = '‹';
    const next = document.createElement('button');
    next.type = 'button';
    next.className = 'tf-table__page-btn';
    next.dataset.page = 'next';
    next.setAttribute('aria-label', 'Nastepna strona');
    next.textContent = '›';
    pager.append(range, prev, next);
    pager.addEventListener('click', (e) => {
      const btn = e.target.closest('.tf-table__page-btn');
      if (!btn || btn.disabled) return;
      const { page, pages } = this._pageState();
      const target = btn.dataset.page === 'prev' ? page - 1 : page + 1;
      if (target < 1 || target > pages) return;
      this.dispatchEvent(new CustomEvent('page-change', {
        bubbles: true,
        detail: { page: target, pageSize: this._pageState().pageSize },
      }));
    });
    this._shadow.appendChild(pager);

    this._wrap = wrap;
    this._table = table;
    this._thead = thead;
    this._tbody = tbody;
    this._pager = pager;
    this._pagerRange = range;
    this._pagerPrev = prev;
    this._pagerNext = next;
  }

  // Reads pagination attributes: page-size (>0 enables the pager), total row
  // count and the current 1-based page. Rows are provided by the host for the
  // CURRENT page only — the table never slices `.rows` itself.
  _pageState() {
    const pageSize = Math.max(0, parseInt(this.getAttribute('page-size') || '0', 10) || 0);
    const total = Math.max(0, parseInt(this.getAttribute('total') || '0', 10) || 0);
    const pages = pageSize > 0 ? Math.max(1, Math.ceil(total / pageSize)) : 1;
    const page = Math.min(pages, Math.max(1, parseInt(this.getAttribute('page') || '1', 10) || 1));
    return { pageSize, total, page, pages };
  }

  _renderPager() {
    if (!this._pager) return;
    const { pageSize, total, page, pages } = this._pageState();
    const active = pageSize > 0 && total > pageSize;
    this._pager.hidden = !active;
    if (!active) return;
    const from = (page - 1) * pageSize + 1;
    const to = Math.min(page * pageSize, total);
    this._pagerRange.textContent = `${from}–${to} / ${total}`;
    this._pagerPrev.disabled = page <= 1;
    this._pagerNext.disabled = page >= pages;
  }

  // Sygnatura kolumn — sluzy do detekcji "kolumny sie nie zmienily" zeby
  // unikac rebuildu thead przy kazdym set rows / sort. thead trzymamy
  // wylacznie dla ARIA i sortowania, nie zalezy od liczby wierszy.
  _columnsSignature(cols) {
    const sig = cols.map(c => `${c.key}|${c.label}|${c.sortable ? 1 : 0}|${c.renderer}|${c.align}|${c.sticky ? 1 : 0}|${c.hideBelow}|${c.fill ? 1 : 0}|${c.width}|${c.lowPriority ? 1 : 0}`).join('');
    const selectAll = this._isMultiSelect() ? 'S' : '';
    const actions = this._rowActions ? `A${this.getAttribute('actions-label') || ''}` : '';
    return `${this._stickyColumns}#${this._expandable ? 'E' : ''}${selectAll}${actions}#${sig}`;
  }

  // Select-all afordancja istnieje tylko w trybie wielokrotnego wyboru, czyli
  // gdy tabela jest selectable z atrybutem selectable="multi" (lub bez wartosci).
  _isMultiSelect() {
    if (!this.hasAttribute('selectable')) return false;
    const mode = (this.getAttribute('selectable') || '').toLowerCase();
    return mode === '' || mode === 'multi';
  }

  _renderThead(cols, sortableTable) {
    const tr = document.createElement('tr');
    const stickySet = this._stickyColumnIndices(cols);
    if (this._expandable) {
      const expTh = document.createElement('th');
      expTh.className = 'tf-table__expand-col';
      expTh.setAttribute('aria-label', 'Rozwin');
      tr.appendChild(expTh);
    }
    cols.forEach((col, i) => {
      const th = document.createElement('th');
      // Select-all afordancja siedzi w naglowku pierwszej kolumny danych (bez
      // dodatkowej kolumny), wiec liczba kolumn naglowka == liczba kolumn danych.
      if (i === 0 && this._isMultiSelect()) {
        const cb = document.createElement('tf-checkbox');
        cb.className = 'tf-table__select-all';
        cb.setAttribute('aria-label', 'Zaznacz wszystkie');
        th.appendChild(cb);
        th.appendChild(document.createTextNode(col.label));
      } else {
        th.textContent = col.label;
      }
      if (col.align === 'num' || col.renderer === 'num') th.classList.add('num');
      if (col.nowrap) th.classList.add('nowrap');
      if (col.fill) th.classList.add('fill');
      if (col.lowPriority) th.classList.add('lo');
      if (col.width) th.style.width = col.width;
      if (sortableTable && col.sortable) {
        th.classList.add('sortable');
        th.dataset.key = col.key;
      }
      this._applyHideBelow(th, col);
      if (stickySet.has(i)) this._applySticky(th, i);
      tr.appendChild(th);
    });
    if (this._rowActions) {
      const actTh = document.createElement('th');
      actTh.className = 'tf-table__actions-col';
      // `actions-label` names the trailing column in the header; without it the
      // column stays visually empty and carries the name for assistive tech only.
      const actionsLabel = this.getAttribute('actions-label');
      if (actionsLabel) actTh.textContent = actionsLabel;
      else actTh.setAttribute('aria-label', 'Akcje');
      tr.appendChild(actTh);
    }
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
    // Tabela rozwijalna wstawia dodatkowe wiersze ekspansji miedzy wierszami
    // danych, wiec recykling po indeksie sie nie zgadza — odbudowujemy w calosci.
    // To NIE jest sciezka czestego odswiezania (rozwijalne tabele sa rzadkie).
    if (this._expandable) {
      this._renderTbodyExpandable(cols, rows);
      return;
    }
    const tbody = this._tbody;
    const existingRows = tbody.children;
    const target = rows.length;

    // 1) Update istniejacych tr w miejscu
    const reuseCount = Math.min(existingRows.length, target);
    for (let i = 0; i < reuseCount; i += 1) {
      const tr = existingRows[i];
      tr.dataset.idx = String(i);
      this._updateRowCells(tr, cols, rows[i], i);
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

  _renderTbodyExpandable(cols, rows) {
    const tbody = this._tbody;
    // Drop expansion state for identities no longer present in the visible row
    // set — a removed/filtered row naturally loses its expansion.
    const presentIds = new Set(rows.map((row, idx) => this._rowIdentity(row, idx)));
    for (const id of [...this._expandedRows]) {
      if (!presentIds.has(id)) this._expandedRows.delete(id);
    }
    const frag = document.createDocumentFragment();
    const leadSpan = this._expandable ? 1 : 0;
    const totalSpan = leadSpan + cols.length + (this._rowActions ? 1 : 0);
    rows.forEach((row, idx) => {
      frag.appendChild(this._buildRow(cols, row, idx));
      const rowId = this._rowIdentity(row, idx);
      if (this._expandedRows.has(rowId)) {
        const exTr = document.createElement('tr');
        exTr.className = 'tf-table__expansion-row';
        exTr.dataset.expansionFor = String(idx);
        const exTd = document.createElement('td');
        exTd.className = 'tf-table__expansion-cell';
        exTd.colSpan = totalSpan;
        let content = null;
        if (this._expandRenderer) {
          try { content = this._expandRenderer(row, idx); } catch { content = null; }
        }
        if (content instanceof Node) exTd.appendChild(content);
        exTr.appendChild(exTd);
        frag.appendChild(exTr);
      }
    });
    tbody.replaceChildren(frag);
  }

  // Wstawia wiodaca komorke toggle ekspansji (jedyna kolumna wiodaca w body).
  // Select-all jest tylko w naglowku, wiec body NIE ma kolumny wyboru.
  _appendLeadingCells(rtr, row, idx) {
    if (this._expandable) {
      const expTd = document.createElement('td');
      expTd.className = 'tf-table__expand-cell';
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'tf-table__expand-toggle';
      const expanded = this._expandedRows.has(this._rowIdentity(row, idx));
      btn.setAttribute('aria-expanded', expanded ? 'true' : 'false');
      btn.setAttribute('aria-label', expanded ? 'Zwin' : 'Rozwin');
      btn.textContent = expanded ? '▾' : '▸';
      expTd.appendChild(btn);
      rtr.appendChild(expTd);
    }
  }

  _buildRow(cols, row, idx) {
    const rtr = document.createElement('tr');
    rtr.dataset.idx = String(idx);
    if (row && row._selected) rtr.classList.add('selected');
    this._applyRowClass(rtr, row);
    const stickySet = this._stickyColumnIndices(cols);
    this._appendLeadingCells(rtr, row, idx);
    cols.forEach((col, i) => {
      const td = document.createElement('td');
      this._applyCardLabel(td, col);
      if (col.renderer === 'num' || col.align === 'num') td.classList.add('num');
      this._applyHideBelow(td, col);
      if (col.fill) td.classList.add('fill');
      if (col.nowrap) td.classList.add('nowrap');
      if (col.lowPriority) td.classList.add('lo');
      if (stickySet.has(i)) this._applySticky(td, i);
      // The select-all box lives in the first header cell, so the per-row box
      // belongs in the matching first data cell — no extra column.
      if (i === 0 && this._isMultiSelect()) {
        const cb = document.createElement('tf-checkbox');
        cb.className = 'tf-table__row-select';
        cb.setAttribute('aria-label', 'Zaznacz wiersz');
        if (row && row._selected) cb.setAttribute('checked', '');
        td.appendChild(cb);
      }
      this._writeCell(td, col, row[col.key], i === 0 && this._isMultiSelect());
      rtr.appendChild(td);
    });
    if (this._rowActions) {
      const actTd = document.createElement('td');
      actTd.className = 'tf-table__actions-cell';
      this._writeActionsCell(actTd, row, idx);
      rtr.appendChild(actTd);
    }
    return rtr;
  }

  // Optional `_class` on a row object = extra modifier classes on its <tr>
  // (e.g. a highlighted "needs attention" row); recycled rows drop the old set.
  _applyRowClass(tr, row) {
    const prev = tr.dataset.rowClass;
    if (prev) for (const c of prev.split(' ')) if (c) tr.classList.remove(c);
    const next = row && typeof row._class === 'string' ? row._class.trim() : '';
    if (next) for (const c of next.split(' ')) if (c) tr.classList.add(c);
    if (next) tr.dataset.rowClass = next; else delete tr.dataset.rowClass;
  }

  _updateRowCells(tr, cols, row, idx) {
    const tds = tr.children;
    this._applyRowClass(tr, row);
    // Sciezka recyklingu dziala tylko gdy _expandable === false, a select-all
    // siedzi w naglowku — body nie ma kolumn wiodacych, wiec td[i] == kolumna i.
    const expected = cols.length + (this._rowActions ? 1 : 0);
    if (tds.length !== expected) {
      // Liczba kolumn sie zmienila (np. wlaczono row actions) — odbuduj wiersz.
      const rebuilt = this._buildRow(cols, row, idx);
      tr.replaceChildren(...rebuilt.childNodes);
      return;
    }
    const stickySet = this._stickyColumnIndices(cols);
    for (let i = 0; i < cols.length; i += 1) {
      const td = tds[i];
      this._applyHideBelow(td, cols[i]);
      this._applyCardLabel(td, cols[i]);
      if (stickySet.has(i)) this._applySticky(td, i);
      this._writeCell(td, cols[i], row[cols[i].key]);
    }
    if (this._rowActions) {
      // Recyklowany wiersz wskazuje teraz na inny obiekt row — odbuduj
      // element akcji, zeby byl zbindowany do aktualnego wiersza.
      this._writeActionsCell(tds[cols.length], row, idx);
    }
  }

  _writeActionsCell(td, row, idx) {
    let el = null;
    try { el = this._rowActions(row, idx); } catch { el = null; }
    if (el instanceof Node) td.replaceChildren(el);
    else td.replaceChildren();
  }

  _writeCell(td, col, value, keepExisting = false) {
    if (value && typeof value === 'object' && 'display' in value && 'value' in value) {
      this._writeCell(td, col, value.display, keepExisting);
      return;
    }
    if (keepExisting) {
      const holder = document.createElement('span');
      this._writeCell(holder, col, value);
      td.appendChild(holder);
      return;
    }
    if (col.renderer === 'chip') {
      const chip = typeof value === 'object' && value
        ? value
        : { status: 'info', label: String(value ?? '') };
      const status = String(chip.status || 'info').replace(/[^a-zA-Z0-9_-]/g, '');
      const span = document.createElement('span');
      span.className = `tf-chip ${status}`;
      if (chip.dot) {
        const dot = document.createElement('span');
        dot.className = 'tf-chip-dot';
        span.appendChild(dot);
      }
      span.appendChild(document.createTextNode(chip.label == null ? '' : String(chip.label)));
      td.replaceChildren(span);
    } else if (col.renderer === 'html') {
      const next = value ?? '';
      // Skip jesli identyczne — eliminuje koszt parsowania HTML komorki gdy
      // wiersz przyszedl niezmieniony z API (najczestsze w 2-sekundowym refreshu).
      if (td.innerHTML !== next) td.innerHTML = next;
    } else if (col.renderer === 'img') {
      // Small inline thumbnail from an image URL cell. An empty value renders a
      // muted em-dash so a missing thumbnail is visible but unobtrusive. The URL
      // is set via the DOM `.src` property (never innerHTML) so the cell value
      // cannot inject markup.
      const url = typeof value === 'string' ? value.trim() : '';
      if (!url) {
        if (td.firstChild == null || td.firstChild.nodeName !== '#text' || td.textContent !== '—') {
          td.replaceChildren(document.createTextNode('—'));
        }
        return;
      }
      let img = td.firstChild;
      if (!(img instanceof HTMLImageElement)) {
        img = document.createElement('img');
        img.className = 'tf-table__thumb';
        img.loading = 'lazy';
        img.alt = '';
        td.replaceChildren(img);
      }
      if (img.getAttribute('src') !== url) img.src = url;
    } else {
      const next = value ?? '';
      const txt = typeof next === 'string' ? next : String(next);
      if (td.textContent !== txt) td.textContent = txt;
    }
  }

  _render() {
    if (!this._thead) return;
    this._syncTableModifiers();
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
    this._renderPager();
  }

  // Mirror the `variant`/`density` attributes onto the real shadow <table> as
  // BEM modifier classes. controls.css is adopted into the shadow root, so
  // `.tf-table--variant-*` / `.tf-table--density-*` rules reach this table's
  // th/td/tbody directly — light-DOM descendant selectors cannot pierce here.
  _syncTableModifiers() {
    if (!this._table) return;
    const classes = ['tf-table'];
    const variant = this.getAttribute('variant');
    if (variant) classes.push(`tf-table--variant-${variant}`);
    const density = this.getAttribute('density');
    if (density) classes.push(`tf-table--density-${density}`);
    if (this.hasAttribute('narrow')) classes.push('tf-table--narrow');
    this._table.className = classes.join(' ');
    // `flush` also strips the wrap chrome (the host card draws the frame).
    if (this._wrap) this._wrap.classList.toggle('tf-table-wrap--flush', variant === 'flush');
  }

  _sortedRows() {
    if (!this._sortKey) return this._rows;
    const key = this._sortKey;
    const dir = this._sortDir === 'asc' ? 1 : -1;
    return this._rows.slice().sort((a, b) => {
      const unwrap = (v) => (v && typeof v === 'object' && 'display' in v && 'value' in v ? v.value : v);
      const va = unwrap(a[key]);
      const vb = unwrap(b[key]);
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
    // Toggle ekspansji nie wyzwala row-click/selection — emituje wlasny event.
    const toggle = e.target.closest('.tf-table__expand-toggle');
    if (toggle) {
      e.stopPropagation();
      const tr = toggle.closest('tbody tr');
      const idx = tr ? parseInt(tr.dataset.idx, 10) : NaN;
      if (Number.isInteger(idx)) this._toggleExpansion(idx);
      return;
    }
    // Klik w komorce akcji nie wyzwala row-click/selection — menu obsluguje
    // wlasne zdarzenia per pozycja.
    if (e.target.closest('.tf-table__actions-cell')) return;
    const tr = e.target.closest('tbody tr');
    // Wiersz ekspansji nie jest wierszem danych — ignoruj.
    if (!tr || tr.classList.contains('tf-table__expansion-row')) return;
    const idx = parseInt(tr.dataset.idx, 10);
    // Selection is checkbox-driven; a row click stays a plain open action so a
    // table can both select rows and drill into them.
    if (e.target.closest('.tf-table__row-select')) return;
    const row = this._sortedRows()[idx];
    this.dispatchEvent(new CustomEvent('row-click', {
      bubbles: true,
      detail: { row, index: idx, selected: tr.classList.contains('selected') },
    }));
  }

  _onDblClick(e) {
    if (e.target.closest('th')) return;
    if (e.target.closest('.tf-table__actions-cell')) return;
    if (e.target.closest('.tf-table__expand-toggle')) return;
    const tr = e.target.closest('tbody tr');
    if (!tr || tr.classList.contains('tf-table__expansion-row')) return;
    const idx = parseInt(tr.dataset.idx, 10);
    if (!Number.isInteger(idx)) return;
    const row = this._sortedRows()[idx];
    this.dispatchEvent(new CustomEvent('row-dblclick', {
      bubbles: true,
      detail: { row, index: idx },
    }));
  }

  // Select-all checkbox (tf-checkbox emituje natywny "change" z .checked).
  // tf-checkbox trzyma swój <input> w LIGHT DOM, wiec e.target to ten input, a
  // klasa marker siedzi na hoscie — bez wejscia w przodkow warunek nigdy nie
  // przechodzi i "select-all" nie jest emitowane (akcje zbiorcze byly martwe).
  _onChange(e) {
    const target = e.target;
    if (!target || !target.closest) return;
    const rowBox = target.closest('.tf-table__row-select');
    if (rowBox) {
      const tr = rowBox.closest('tbody tr');
      const idx = tr ? parseInt(tr.dataset.idx, 10) : NaN;
      if (!Number.isInteger(idx)) return;
      const checked = typeof target.checked === 'boolean' ? target.checked : !!rowBox.checked;
      tr.classList.toggle('selected', checked);
      this.dispatchEvent(new CustomEvent('row-select', {
        bubbles: true,
        detail: { row: this._sortedRows()[idx], index: idx, selected: checked },
      }));
      return;
    }
    const box = target.closest('.tf-table__select-all');
    if (!box) return;
    // Host odzwierciedla stan atrybutem; input niesie go wprost.
    const checked = typeof target.checked === 'boolean' ? target.checked : !!box.checked;
    this.dispatchEvent(new CustomEvent('select-all', {
      bubbles: true,
      detail: { selected: checked },
    }));
  }

  _toggleExpansion(idx) {
    const row = this._sortedRows()[idx];
    const rowId = this._rowIdentity(row, idx);
    const willExpand = !this._expandedRows.has(rowId);
    if (willExpand) this._expandedRows.add(rowId);
    else this._expandedRows.delete(rowId);
    this.dispatchEvent(new CustomEvent('row-expand', {
      bubbles: true,
      detail: { row, index: idx, expanded: willExpand },
    }));
    this._render();
  }
}

customElements.define('tf-table', TfTable);
export { TfTable, TfColumn };
