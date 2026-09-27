// =============================================================================
// Plik: tf-table.js
// Opis: Komponent <tf-table sortable selectable> z <tf-column key="..." label
//       renderer="text|chip|num" sortable sticky hide-below="900" fill nowrap
//       width="40%" priority="low">.
//       `hide-below` ukrywa kolumne ponizej podanej szerokosci viewportu
//       (dozwolone: 480 640 720 900 1024 1180 1280 — regula zyje w controls.css,
//       a media query nie czyta zmiennej CSS). Komorki zostaja w DOM, wiec stan
//       tabeli (zaznaczenie, ekspansja, sort) przezywa zmiane szerokosci.
//       `hint="…"` puts one explanatory sentence on the column HEADER (title),
//       for a caveat that belongs to the column and not to each of its cells.
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

import { adoptControlsInto, adoptScopedSheetsInto, injectSpriteIntoShadow } from './shared-styles.js';

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
    return ['sortable', 'selectable', 'variant', 'density', 'narrow', 'page-size', 'total', 'page', 'actions-label', 'empty-message'];
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
    // Optional per-row actions builder:
    //   (row, index, currentRow) => Element | null
    // When set, tf-table renders a trailing actions column hosting the
    // returned element (e.g. a kebab tf-menu). `currentRow()` returns the row
    // occupying this slot AT CALL TIME, so a handler that reads through it
    // acts on the row that is actually there rather than on the one the
    // element was built from. Builders read every row value their HANDLERS
    // need through it; MARKUP may still come from `row`, which is rendered at
    // once and therefore can never be stale. That split is what lets
    // _writeActionsCell keep an existing node whenever a rebuild would have
    // produced identical markup.
    this._rowActions = null;
    // Optional signature of the actions cell: (row, index) => string | number.
    // Rows are rebuilt from fresh API data on every poll, so `row === lastRow`
    // is never true and object identity cannot tell "unchanged" from "changed".
    // The host declares the signature instead; while it holds, the element
    // already in the cell is KEPT rather than rebuilt — which is what stops a
    // 5 s poll from swapping a click target out from under the user's cursor.
    // The signature must cover every row field the builder RENDERS and every
    // field its handlers CLOSE OVER, including the row's identity. A field left
    // out keeps the values captured when the element was built.
    this._rowActionsKey = null;
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
    // A NEW builder means every cached actions element was produced by the OLD
    // one and still closes over its values (an `isAdmin` that has since
    // changed, a stale permission check). The cached signature describes the
    // ROW, so on its own it would keep those elements alive for good. Bumping a
    // generation invalidates every stored signature at once.
    this._rowActionsGen = (this._rowActionsGen || 0) + 1;
    // Column count changes when actions toggle on/off — force thead rebuild.
    this._lastColsSig = null;
    this._render();
  }

  get rowActionsKey() { return this._rowActionsKey; }
  set rowActionsKey(fn) {
    this._rowActionsKey = typeof fn === 'function' ? fn : null;
    // A different key function can return the SAME string for a different
    // notion of "unchanged", so signatures cached under the previous one are
    // meaningless and must not match.
    this._rowActionsGen = (this._rowActionsGen || 0) + 1;
    // Deliberately does NOT render: this is bookkeeping about how to refresh
    // the actions cell, not a change to anything already on screen. Rendering
    // here would cost a second full pass whenever a host assigns both this and
    // `rowActions` while drawing a tab.
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
      // One sentence that explains what the column MEANS, on the header only.
      // A caveat a screen has to state ("as measured by this node") belongs
      // next to the heading, not repeated in every cell under it.
      hint: c.getAttribute('hint') || '',
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
    // `renderer="html"` cells are written with td.innerHTML INSIDE this shadow
    // root, where the screen's own stylesheet cannot reach them. Adopt the
    // sheets scoped to the screen this table sits in (see shared-styles.js).
    //
    // Sequenced, not raced: adoptedStyleSheets is ordered, so letting the two
    // adoptions resolve in whatever order the network returns would leave the
    // cascade order up to chance. controls.css is the base, the screen sheet
    // refines it, so the screen sheet must always come second.
    //
    // Each adoption carries its OWN catch. A shared trailing catch would let a
    // single failed controls.css fetch skip the screen sheet entirely and
    // swallow the reason — silently restoring the very defect this exists to
    // fix (unstyled cells, black sparklines, nothing in the console).
    adoptControlsInto(this._shadow)
      .catch(() => { /* base styles unavailable — the screen sheet must still load */ })
      .then(() => adoptScopedSheetsInto(this._shadow, this))
      .catch(() => { /* styling is best-effort; the table still renders */ });
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
      if (col.hint) th.title = col.hint;
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

  // The header select-all box was built once in `_renderThead` and never
  // touched again, so it kept whatever `checked` a previous click left it at:
  // once the host cleared the selection (a bulk action, a filter change) the
  // box stayed ticked with every row unticked underneath it (MINOR D, critic
  // 2026-09-22, probe S3). Re-derived from the rows ON SCREEN on every render
  // instead — all selected -> checked, none -> unchecked, a mix ->
  // indeterminate (tf-checkbox supports the attribute) — and written only
  // when a value actually differs, the same rule every other sync in this file
  // follows so an unrelated poll cannot cause a spurious re-render of the box.
  _syncSelectAllBox(rows) {
    if (!this._isMultiSelect()) return;
    const cb = this._thead && this._thead.querySelector('.tf-table__select-all');
    if (!cb) return;
    const total = rows.length;
    let selectedCount = 0;
    for (const r of rows) if (r && typeof r === 'object' && r._selected) selectedCount += 1;
    const allSelected = total > 0 && selectedCount === total;
    const noneSelected = selectedCount === 0;
    const indeterminate = total > 0 && !allSelected && !noneSelected;
    if (cb.hasAttribute('indeterminate') !== indeterminate) {
      if (indeterminate) cb.setAttribute('indeterminate', '');
      else cb.removeAttribute('indeterminate');
    }
    if (cb.hasAttribute('checked') !== allSelected) {
      if (allSelected) cb.setAttribute('checked', '');
      else cb.removeAttribute('checked');
    }
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
    // An empty table has to say WHY it is empty. `empty-message` was passed by
    // 19 call sites across 11 screens and every one of them was inert: the
    // attribute was never read here and was not in `observedAttributes`, so a
    // table with no rows rendered as nothing at all and the caller's sentence
    // went nowhere. Handled before the expandable split, because both paths
    // produce the same empty tbody.
    if (rows.length === 0 && this.hasAttribute('empty-message')) {
      this._renderEmptyRow(cols);
      return;
    }
    // A leftover empty row must never survive into the recycling path below:
    // that path updates `<tr>`s by index and would write data cells into it.
    if (this._tbody.firstElementChild?.classList.contains('tf-table__empty-row')) {
      this._tbody.textContent = '';
    }
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

  // The span comes from the RENDERED header rather than a second copy of the
  // lead/actions arithmetic: selection, expansion and the actions column all
  // already appear there, so the two can never drift apart.
  //
  // Patched in place when the row is already there: an unchanged poll on an
  // empty table must not replace the node, for the same reason the data path
  // recycles its rows.
  _renderEmptyRow(cols) {
    const tbody = this._tbody;
    const text = this.getAttribute('empty-message') || '';
    const span = this._thead?.querySelector('tr')?.children.length || Math.max(1, cols.length);
    const current = tbody.firstElementChild;
    if (tbody.children.length === 1 && current?.classList.contains('tf-table__empty-row')) {
      const td = current.firstElementChild;
      if (td.colSpan !== span) td.colSpan = span;
      // Same source-cache rule as `_writeCell`'s text branch (see there for
      // why comparing against a DOM read-back is the wrong contract to build
      // on), applied here because this cell is written directly and never
      // goes through `_writeCell`.
      if (td.__tfText !== text) {
        td.textContent = text;
        td.__tfText = text;
      }
      return;
    }
    const tr = document.createElement('tr');
    tr.className = 'tf-table__empty-row';
    const td = document.createElement('td');
    td.className = 'tf-table__empty-cell';
    td.colSpan = span;
    td.textContent = text;
    td.__tfText = text;
    tr.appendChild(td);
    tbody.replaceChildren(tr);
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

  // Builds the row checkbox exactly the way `_buildRow` always has, factored
  // out so the recycle path (`_syncRowSelectBox`) creates the SAME node shape
  // when a checkbox is missing — one construction site, not two that could
  // drift apart.
  _makeRowCheckbox() {
    const cb = document.createElement('tf-checkbox');
    cb.className = 'tf-table__row-select';
    cb.setAttribute('aria-label', 'Zaznacz wiersz');
    return cb;
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
        const cb = this._makeRowCheckbox();
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

  // Multi-select cell 0 is recycled by index, so a poll, a sort or a filter
  // can hand this <td> a DIFFERENT logical row than the one whose tick is
  // still sitting in the DOM. The row's own `_selected` field is the single
  // source of truth for what should be checked (the same field `_buildRow`
  // reads, and the field select-all / `_onChange` set on the data the host
  // reassigns) — this brings the checkbox and the `<tr>`'s `selected` class
  // back in line with it on every update, creating the checkbox first when a
  // table only just became multi-select (see the `selectable` toggle test).
  // Writes only when a value differs, so an unchanged row's checkbox is left
  // completely alone, which is what lets it survive an unrelated poll intact.
  //
  // A row with no OWN `_selected` key (a table that never uses selection, or
  // a poll that only refreshes other fields) leaves the existing checked /
  // selected state untouched rather than forcing it to false — otherwise a
  // user's in-flight click, or a caller that manages selection without ever
  // setting the field, would be silently unticked on the very next render.
  _syncRowSelectBox(td, tr, row) {
    let cb = td.firstElementChild;
    if (!(cb && cb.tagName === 'TF-CHECKBOX' && cb.classList.contains('tf-table__row-select'))) {
      cb = this._makeRowCheckbox();
      td.insertBefore(cb, td.firstChild);
    }
    if (!(row && typeof row === 'object' && Object.prototype.hasOwnProperty.call(row, '_selected'))) return;
    const selected = !!row._selected;
    if (cb.hasAttribute('checked') !== selected) {
      if (selected) cb.setAttribute('checked', '');
      else cb.removeAttribute('checked');
    }
    if (tr.classList.contains('selected') !== selected) {
      tr.classList.toggle('selected', selected);
    }
  }

  _updateRowCells(tr, cols, row, idx) {
    const tds = tr.children;
    this._applyRowClass(tr, row);
    // Without the multi-select checkbox (single mode, or a host that marks
    // rows itself), the <tr>'s `selected` class follows the row's OWN
    // `_selected` too: a recycled slot must not keep the highlight of the row
    // that sat in it before a page change or a new pick (iteration-5 MAJOR 2).
    // A row with no `_selected` key leaves the class alone, as in
    // `_syncRowSelectBox`; writes only when it differs.
    if (!this._isMultiSelect() && row && typeof row === 'object' && Object.prototype.hasOwnProperty.call(row, '_selected')) {
      const selected = !!row._selected;
      if (tr.classList.contains('selected') !== selected) tr.classList.toggle('selected', selected);
    }
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
      const isSelectCell = i === 0 && this._isMultiSelect();
      // Ensure the checkbox exists and its state matches the row BEFORE the
      // value write below: `_writeCell`'s `keepExisting` path decides whether
      // it can reuse the cached holder by looking at the td's children, and it
      // must find the checkbox (new or existing) already in place.
      if (isSelectCell) this._syncRowSelectBox(td, tr, row);
      // Cell 0 of a multi-select row carries the row checkbox ahead of its
      // value (see _buildRow). Without `keepExisting` here this recycled
      // write went straight into the td — for the html renderer, `__tfHtml`
      // is never recorded on that path (see `_buildRow`'s own `keepExisting`
      // call), so it wrote unconditionally; for every other renderer the
      // first poll after a fresh build found no matching cache either and
      // wrote too. Either way `td.innerHTML =` / `td.textContent =` deleted
      // the checkbox outright. Routing through the same `keepExisting` path
      // the build uses keeps the checkbox untouched and rewrites only the
      // value holder.
      this._writeCell(td, cols[i], row[cols[i].key], isSelectCell);
    }
    if (this._rowActions) {
      // A recycled <tr> may now show a different logical row. The actions
      // element is replaced only when that changes the markup it builds;
      // otherwise the node stays put and its handlers follow the current row
      // by themselves (see _writeActionsCell).
      this._writeActionsCell(tds[cols.length], row, idx);
    }
  }

  // Signature of the actions cell for `row`, or null when the host declared
  // none — then the builder runs on every render and the markup comparison in
  // _writeActionsCell decides whether anything is written. A builder that
  // throws, or returns anything but a string/number, counts as "no signature"
  // rather than as a false match.
  _rowActionsSignature(row, idx) {
    if (!this._rowActionsKey) return null;
    let key = null;
    try { key = this._rowActionsKey(row, idx); } catch { return null; }
    return typeof key === 'string' || typeof key === 'number'
      ? `k:${this._rowActionsGen || 0}:${key}`
      : null;
  }

  // Builds the actions element for `row` and puts it in `td` — unless the cell
  // already holds the markup that was just built, in which case the NEW
  // element is DISCARDED and the existing node is left untouched. Not writing
  // the DOM at all is the point: it is what keeps a click target alive under
  // the user's cursor across a 5 s poll.
  //
  // Keeping a node is sound because the builder is handed `currentRow`, so its
  // handlers resolve the row occupying this slot at CLICK time rather than the
  // one they were built from. Identical markup therefore also means identical
  // behaviour, even after a sort, a filter or a poll moved a different row
  // into this position — which is why the guard needs no promise from the
  // caller, unlike `rowActionsKey`.
  //
  // The comparison is against the markup AS BUILT, recorded here, and never
  // against the live node: a tf-menu the user has opened carries an `open`
  // attribute, and a button with an action in flight carries `disabled`.
  // Comparing against the live DOM would read those as "changed" and destroy
  // precisely the element being interacted with.
  _writeActionsCell(td, row, idx) {
    // Whether an existing element may be KEPT at all — decided before either
    // path that could keep one. A builder is trusted only when it declared the
    // live accessor (three parameters), because then its handlers resolve the
    // row occupying this slot at click time. One that took `(row)` or
    // `(row, idx)` closes over the row it was built from, so reusing its
    // element would fire its handlers on data from a poll ago with no visible
    // symptom. Those are rebuilt, exactly as tf-table behaved before any of
    // this existed: such a caller loses performance, never correctness.
    //
    // `Function.length` stops counting at the first defaulted or rest
    // parameter, so `(row = x, idx, cur) => {}` and `(...args) => {}` both
    // report 0. Requiring >= 3 is therefore sound BY CONSTRUCTION rather than
    // by convention: every shape that could hide a row read fails the test
    // instead of passing it.
    const cannotHoldStaleRow = typeof this._rowActions === 'function' && this._rowActions.length >= 3;
    const key = this._rowActionsSignature(row, idx);
    // A declared signature that still holds skips the build entirely — the
    // optional fast path, gated on the SAME condition. A hand-rolled key that
    // omits the row identity, paired with an unmigrated builder, would
    // otherwise keep a node whose handlers fire on the row that used to sit
    // here (reproduced against the shipped code during review). `null` never
    // matches itself, so a table without a signature always builds and falls
    // through to the markup comparison below.
    if (key !== null && cannotHoldStaleRow && td._tfActionsKey === key) return;
    const gen = this._rowActionsGen || 0;
    let el = null;
    try {
      el = this._rowActions(row, idx, () => this._sortedRows()[idx] ?? row);
    } catch { el = null; }
    // `outerHTML` is read off the element, not through `instanceof Element`:
    // the test harness does not export that global, and a text node simply has
    // no outerHTML and so can never match.
    const html = el instanceof Node && typeof el.outerHTML === 'string' ? el.outerHTML : null;
    const held = td.childNodes.length === 1 ? td.firstChild : null;
    // The generation is part of the match: a NEW builder closes over new
    // values (an `isAdmin` that has since changed), so its output has to
    // replace the old element even when the two render the same markup.
    //
    // `html !== null` below is defensive, not decisive: no mutation of it can
    // fail a test, because a builder returning a Text or Comment node is
    // already stopped by `typeof held.outerHTML === 'string'` on the next
    // render. Kept as a guard against a future path that records a non-null
    // html for a node that has none, and said plainly here so the redundancy
    // is not mistaken for untested logic.
    if (html !== null
      && cannotHoldStaleRow
      && held != null && typeof held.outerHTML === 'string'
      && td._tfActionsGen === gen
      && td._tfActionsHtml === html) {
      td._tfActionsKey = key;
      return;
    }
    if (el instanceof Node) td.replaceChildren(el);
    else td.replaceChildren();
    // Recorded even when null, so a cell whose builder returned nothing is not
    // mistaken for one that was never written.
    td._tfActionsHtml = html;
    td._tfActionsGen = gen;
    td._tfActionsKey = key;
  }

  _writeCell(td, col, value, keepExisting = false) {
    if (value && typeof value === 'object' && 'display' in value && 'value' in value) {
      this._writeCell(td, col, value.display, keepExisting);
      return;
    }
    if (keepExisting) {
      // Multi-select cell 0 holds the row checkbox PLUS the value: the
      // checkbox is appended by the caller (build path) or already sits there
      // (update path), and the value goes into a separate holder <span> so it
      // can be rewritten without ever touching the checkbox node. The holder
      // is cached on the td (`__tfValueHolder`) and REUSED across updates —
      // never recreated — for the same reason `_writeActionsCell` keeps its
      // node: recreating it here would still delete nothing visible, but it
      // would reset the holder's own `__tfHtml`/`__tfText`/`__tfRenderer`
      // cache every poll, defeating the source-cache rule below for every
      // multi-select first column. `_buildRow` and `_updateRowCells` both
      // funnel through here so the two paths produce the exact same DOM shape
      // (checkbox, then holder) and can never diverge.
      let holder = td.__tfValueHolder;
      // A plain write in between (the table stopped being multi-select, or a
      // column change handed this recycled cell to another renderer) replaces
      // the td's children and leaves the cached holder detached — writing the
      // value into it would put it nowhere on screen.
      if (!holder || holder.parentNode !== td) {
        // Whatever that plain write left (a text node, stale markup) goes too;
        // only the row checkbox stays.
        for (const node of [...td.childNodes]) {
          if (!(node.nodeType === 1 && node.classList.contains('tf-table__row-select'))) node.remove();
        }
        holder = document.createElement('span');
        holder.className = 'tf-table__cell-value';
        td.appendChild(holder);
        td.__tfValueHolder = holder;
      }
      // The value now lives in the HOLDER, which keeps its own
      // `__tfHtml`/`__tfText` source cache. The td's own cache still describes
      // whatever a PLAIN write last put directly on the td, before multi-select
      // wrapped the value in a holder. Left in place, that stale cache survives
      // untouched here (this branch never writes td.__tfHtml/__tfText) and later
      // matches again the moment `selectable` turns off and cell 0 goes back to
      // a plain write with the SAME value — skipping the very write that must
      // remove the checkbox and holder from the td (MINOR E, critic
      // 2026-09-22). Clearing it on every entry into this branch means the next
      // plain write always finds no cache to (falsely) agree with.
      td.__tfHtml = undefined;
      td.__tfText = undefined;
      this._writeCell(holder, col, value);
      return;
    }
    // The source caches below (`__tfHtml`, `__tfText`) describe what ONE
    // renderer last wrote. Cells are recycled by index and a column change
    // rebuilds only the head, so the same <td> can be written by another
    // renderer (chip, img) and later by its old one with the old string —
    // a cache left from before would then skip a write the cell needs.
    if (td.__tfRenderer !== col.renderer) {
      td.__tfRenderer = col.renderer;
      td.__tfHtml = undefined;
      td.__tfText = undefined;
    }
    if (col.renderer === 'chip') {
      const chip = typeof value === 'object' && value
        ? value
        : { status: 'info', label: String(value ?? '') };
      const status = String(chip.status || 'info').replace(/[^a-zA-Z0-9_-]/g, '');
      // `variant: 'outline'` — the sentence-case pill (tf-chip variant="outline").
      const cls = `tf-chip ${status}${chip.variant === 'outline' ? ' tf-chip--outline' : ''}`;
      const label = chip.label == null ? '' : String(chip.label);
      // Skip an identical write, the way the `html` renderer below already
      // does. Without this the cell swapped one <span> per row on EVERY poll:
      // measured on the live disks table, 319 of the 329 remaining DOM
      // mutations per 21 s came from here, long after the row-actions churn
      // had been fixed.
      const current = td.firstElementChild;
      const unchanged = td.childNodes.length === 1
        && current instanceof HTMLElement
        && current.className === cls
        && current.textContent === label
        && !!current.querySelector('.tf-chip-dot') === !!chip.dot;
      if (!unchanged) {
        const span = document.createElement('span');
        span.className = cls;
        if (chip.dot) {
          const dot = document.createElement('span');
          dot.className = 'tf-chip-dot';
          span.appendChild(dot);
        }
        span.appendChild(document.createTextNode(label));
        td.replaceChildren(span);
      }
    } else if (col.renderer === 'html') {
      const next = value ?? '';
      // Skip jesli identyczne — eliminuje koszt parsowania HTML komorki gdy
      // wiersz przyszedl niezmieniony z API (najczestsze w 2-sekundowym refreshu).
      //
      // The comparison MUST be against the SOURCE string this component last
      // wrote (cached on the cell as `__tfHtml`), never against `td.innerHTML`
      // read back from the browser. Once a cell has been parsed, its innerHTML
      // is a RE-SERIALIZATION, not the original string: a custom element such
      // as <tf-chip> renders into its own light DOM and replaces the markup it
      // was given, a self-closing `<use/>` comes back as `<use></use>`, and
      // attribute quoting/order gets normalized. Comparing `td.innerHTML !==
      // next` is therefore true on almost every poll even when nothing
      // changed, and every table in the product rewrote every html cell on
      // every refresh (critic 2026-09-21: mockups n01-n10 M11, n11-n19 MAJOR
      // 9) — tearing down live subtrees the owner's hard rule says must only
      // be patched. `__tfHtml` starts undefined on a fresh `<td>` (including
      // one recycled from a previous, unrelated row), so the first write for
      // any cell always happens; after that it can only match when THIS
      // component produced the current content from this exact string, which
      // is exactly "unchanged" — even for a row-recycled cell now showing a
      // different logical row, since identical source text means identical
      // visible content regardless of which row it came from.
      if (td.__tfHtml !== next) {
        td.innerHTML = next;
        td.__tfHtml = next;
      }
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
      // Plain text does not go through HTML parsing, so `td.textContent` read
      // back is exact today — but cache the SOURCE string here too rather than
      // rely on that being true forever, for the same reason the html branch
      // above cannot trust `td.innerHTML`: one write path, one comparison rule.
      if (td.__tfText !== txt) {
        td.textContent = txt;
        td.__tfText = txt;
      }
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
    this._syncSelectAllBox(rows);
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
      const row = this._sortedRows()[idx];
      // The DATA row is the one source of selection truth: every render syncs
      // the box FROM `row._selected` (`_syncRowSelectBox`), so a click that
      // only toggled the box would be undone by the next render — a sort
      // header click, a page change — before the host had reassigned rows.
      if (row && typeof row === 'object') row._selected = checked;
      rowBox.toggleAttribute('checked', checked);
      tr.classList.toggle('selected', checked);
      this.dispatchEvent(new CustomEvent('row-select', {
        bubbles: true,
        detail: { row, index: idx, selected: checked },
      }));
      return;
    }
    const box = target.closest('.tf-table__select-all');
    if (!box) return;
    // Host odzwierciedla stan atrybutem; input niesie go wprost.
    const checked = typeof target.checked === 'boolean' ? target.checked : !!box.checked;
    // Same rule as a row box: what the user sees selected is written into the
    // rows on screen, so a render before the host answers keeps it.
    for (const row of this._rows) {
      if (row && typeof row === 'object') row._selected = checked;
    }
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
