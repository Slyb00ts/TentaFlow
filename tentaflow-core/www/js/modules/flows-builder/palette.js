// =============================================================================
// Plik: modules/flows-builder/palette.js
// Opis: Paleta node'ów Flow Buildera - ładuje templates z API, grupuje po
//       kategoriach, obsługuje wyszukiwanie, pointer drag (touch + mysz).
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { ModelModalities } from '/js/modules/flows-builder/model-modalities.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { getNodeName, getNodeDescription } from '/js/modules/flows-builder/node-i18n.js';
import { nodeIconId, nodeColorVar } from '/js/modules/flows-builder/node-visuals.js';

const CATEGORY_ORDER = ['trigger', 'service', 'memory', 'transform', 'logic', 'filter', 'output', 'other'];

function categoryLabel(cat) {
  return I18n.t(`flows_palette.categories.${cat}`);
}

function catFor(tpl) {
  const c = (tpl.category || '').toLowerCase();
  if (CATEGORY_ORDER.includes(c)) return c;
  // Sensible fallback: typ noda -> kategoria
  const t = tpl.node_type;
  if (t === 'trigger' || t === 'start') return 'trigger';
  if (['llm','stt','tts','embeddings','reranker'].includes(t)) return 'service';
  if (['memory','conversation_history','session_context','speaker_context','memory_analyzer','persist_turn'].includes(t)) return 'memory';
  if (['condition','switch'].includes(t)) return 'logic';
  if (['template','transform','router'].includes(t)) return 'transform';
  if (['pii_filter','tts_clean'].includes(t)) return 'filter';
  if (['output','end'].includes(t)) return 'output';
  // Harness background blocks group under "service" (closest existing category).
  if (['spawn','await_subagents','subagent_status','interval'].includes(t)) return 'service';
  // Code Studio: `patch_review` gates the run on a human decision, so it sits
  // with the logic blocks; the rest act on the workspace.
  if (t === 'patch_review') return 'logic';
  if (['workspace_context','exec_command','delegate_cli'].includes(t)) return 'service';
  return 'other';
}

export class FlowPalette {
  constructor(rootEl, opts = {}) {
    this.root = rootEl;
    this.opts = opts;
    this.templates = [];
    // node_type występujące w kilku wariantach (presety agentów). Dla nich
    // nazwa z i18n jest wspólna, więc paleta musi pokazać etykietę szablonu —
    // inaczej 17 różnych presetów to 17 wierszy "Agent".
    this.presetTypes = new Set();
    // Read-only builder: templates still load (the canvas needs them for
    // rendering) but nothing can be dragged onto the canvas.
    this.readOnly = !!opts.readOnly;
    this.filter = '';
    this.collapsedCats = new Set();
    this._ghost = null;
    this._dragging = null;
    this._pointerMoveHandler = this._onPointerMove.bind(this);
    this._pointerUpHandler = this._onPointerUp.bind(this);
  }

  async init() {
    this.root.classList.add('fb-palette');
    this.root.innerHTML = `
      <div class="fb-palette-header">
        <span class="fb-panel-title">${escapeHtml(I18n.t('flows_palette.title'))}</span>
        <span class="fb-palette-count" data-role="count">0</span>
      </div>
      <div class="fb-palette-search">
        <input type="search" placeholder="${escapeAttr(I18n.t('flows_palette.search_placeholder'))}" aria-label="${escapeAttr(I18n.t('flows_palette.search_label'))}">
      </div>
      <div class="fb-palette-list" data-role="list"></div>
    `;
    this.listEl = this.root.querySelector('[data-role="list"]');
    this.countEl = this.root.querySelector('[data-role="count"]');
    this.searchEl = this.root.querySelector('input[type="search"]');

    let debounce = null;
    this.searchEl.addEventListener('input', (e) => {
      clearTimeout(debounce);
      debounce = setTimeout(() => {
        this.filter = (e.target.value || '').toLowerCase();
        this._render();
      }, 120);
    });

    // Alongside the palette: which model can take what. Fire-and-forget — a
    // canvas that renders before the catalog lands simply dims nothing, and the
    // next render (any edit) picks it up.
    ModelModalities.load();

    try {
      this.templates = await ApiBinary.list('flowNodeTemplatesListRequest', { arrayKey: 'templates' });
    } catch (err) {
      this.templates = [];
      this.listEl.innerHTML = `<div class="fb-palette-empty">${escapeHtml(I18n.t('flows_palette.load_error', { error: err.message }))}</div>`;
      return;
    }
    const perType = new Map();
    for (const tpl of this.templates) perType.set(tpl.node_type, (perType.get(tpl.node_type) ?? 0) + 1);
    this.presetTypes = new Set([...perType.entries()].filter(([, n]) => n > 1).map(([t]) => t));
    if (this.opts.onTemplatesLoaded) this.opts.onTemplatesLoaded(this.templates);
    this._render();
  }

  getTemplates() { return this.templates; }

  /** Nazwa wpisu palety — preset mówi własną etykietą, reszta tłumaczeniem typu. */
  _nameOf(tpl) {
    if (this.presetTypes.has(tpl.node_type) && tpl.label) return tpl.label;
    return getNodeName(tpl.node_type, tpl.label);
  }

  _descOf(tpl) {
    if (this.presetTypes.has(tpl.node_type) && tpl.description) return tpl.description;
    return getNodeDescription(tpl.node_type, tpl.description);
  }

  _render() {
    const groups = {};
    const total = this.templates.length;
    let shown = 0;
    for (let i = 0; i < this.templates.length; i += 1) {
      const tpl = this.templates[i];
      const c = catFor(tpl);
      const label = this._nameOf(tpl).toLowerCase();
      const desc = this._descOf(tpl).toLowerCase();
      const type = (tpl.node_type || '').toLowerCase();
      const matches = !this.filter || label.includes(this.filter) || desc.includes(this.filter) || type.includes(this.filter);
      if (!matches) continue;
      if (!groups[c]) groups[c] = [];
      groups[c].push({ tpl, index: i });
      shown += 1;
    }
    // Licznik odpowiada na "ile z ilu widzę", bo przy 70 blokach sam wynik
    // filtrowania nie mówi, jak bardzo lista została zawężona. Sama liczba przy
    // przewijanej liście czyta się jak "tyle widać", więc jednostkę niesie
    // etykieta dostępności i tooltip.
    this.countEl.textContent = shown === total
      ? String(total)
      : I18n.t('flows_palette.count_filtered', { shown, total });
    const label = shown === total
      ? I18n.t('flows_palette.count_title', { count: total })
      : I18n.t('flows_palette.count_title_filtered', { shown, total });
    this.countEl.title = label;
    this.countEl.setAttribute('aria-label', label);

    if (shown === 0) {
      this.listEl.innerHTML = `
        <div class="fb-palette-empty">
          <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" style="opacity:0.5;margin-bottom:8px;"><circle cx="11" cy="11" r="7"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>
          <div style="font-weight:600;color:var(--tf-text-2);margin-bottom:4px;">${escapeHtml(I18n.t('flows_palette.empty_title'))}</div>
          <div>${escapeHtml(I18n.t('flows_palette.empty_hint', { query: this.filter }))}</div>
        </div>`;
      return;
    }

    let html = '';
    for (const cat of CATEGORY_ORDER) {
      const items = groups[cat];
      if (!items || items.length === 0) continue;
      const collapsed = this.collapsedCats.has(cat);
      html += `
        <div class="fb-palette-category ${collapsed ? 'collapsed' : ''}" data-cat="${escapeAttr(cat)}">
          <div class="fb-palette-cat-header" data-role="cat-header">
            <span>${escapeHtml(categoryLabel(cat))}</span>
            <span class="fb-palette-cat-count">${items.length}</span>
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="6 9 12 15 18 9"/></svg>
          </div>
          <div class="fb-palette-items">
            ${items.map((it) => this._renderItem(it.tpl, it.index)).join('')}
          </div>
        </div>`;
    }
    this.listEl.innerHTML = html;

    this.listEl.querySelectorAll('[data-role="cat-header"]').forEach((h) => {
      h.addEventListener('click', () => {
        const cat = h.closest('.fb-palette-category').dataset.cat;
        if (this.collapsedCats.has(cat)) this.collapsedCats.delete(cat);
        else this.collapsedCats.add(cat);
        h.closest('.fb-palette-category').classList.toggle('collapsed');
      });
    });

    this.listEl.querySelectorAll('.fb-node-item').forEach((el) => {
      el.addEventListener('pointerdown', (ev) => this._onPointerDown(ev, el));
    });
  }

  _renderItem(tpl, index) {
    const cat = catFor(tpl);
    const iconId = nodeIconId(tpl.node_type, tpl.icon, cat);
    const varName = nodeColorVar(tpl.node_type, cat);
    const name = this._nameOf(tpl);
    const desc = this._descOf(tpl);
    return `
      <div class="fb-node-item" data-index="${index}" data-node-type="${escapeAttr(tpl.node_type)}" title="${escapeAttr(desc || name)}" style="--node-color: var(${varName})">
        <div class="fb-node-icon"><svg><use href="#i-${iconId}"/></svg></div>
        <div class="fb-node-info">
          <div class="fb-node-name">${escapeHtml(name)}</div>
          ${desc ? `<div class="fb-node-desc">${escapeHtml(desc)}</div>` : ''}
        </div>
      </div>`;
  }

  _onPointerDown(ev, el) {
    if (this.readOnly) return;
    if (ev.button !== undefined && ev.button !== 0) return;
    ev.preventDefault();
    // Po indeksie, nie po node_type: presety agentów dzielą jeden typ, więc
    // wyszukiwanie po typie zawsze upuszczało pierwszy z nich.
    const tpl = this.templates[Number(el.dataset.index)];
    if (!tpl) return;
    this._dragging = { tpl, startX: ev.clientX, startY: ev.clientY, moved: false };
    el.classList.add('dragging');
    el.setPointerCapture?.(ev.pointerId);
    this._dragging.el = el;
    this._dragging.pointerId = ev.pointerId;
    window.addEventListener('pointermove', this._pointerMoveHandler);
    window.addEventListener('pointerup', this._pointerUpHandler);
    window.addEventListener('pointercancel', this._pointerUpHandler);
  }

  _onPointerMove(ev) {
    if (!this._dragging) return;
    const d = this._dragging;
    const dx = ev.clientX - d.startX;
    const dy = ev.clientY - d.startY;
    if (!d.moved && Math.hypot(dx, dy) > 4) {
      d.moved = true;
      this._ghost = document.createElement('div');
      this._ghost.className = 'fb-drag-ghost';
      this._ghost.style.setProperty('--node-color', `var(${nodeColorVar(d.tpl.node_type, catFor(d.tpl))})`);
      this._ghost.textContent = this._nameOf(d.tpl);
      document.body.appendChild(this._ghost);
    }
    if (this._ghost) {
      this._ghost.style.left = `${ev.clientX}px`;
      this._ghost.style.top = `${ev.clientY}px`;
    }
    // Podświetl canvas jeśli kursor nad nim
    const canvas = document.querySelector('.fb-canvas');
    if (canvas) {
      const rect = canvas.getBoundingClientRect();
      const inside = ev.clientX >= rect.left && ev.clientX <= rect.right && ev.clientY >= rect.top && ev.clientY <= rect.bottom;
      canvas.classList.toggle('drop-target', inside && d.moved);
    }
  }

  _onPointerUp(ev) {
    if (!this._dragging) return;
    const d = this._dragging;
    window.removeEventListener('pointermove', this._pointerMoveHandler);
    window.removeEventListener('pointerup', this._pointerUpHandler);
    window.removeEventListener('pointercancel', this._pointerUpHandler);
    if (this._ghost) { this._ghost.remove(); this._ghost = null; }
    if (d.el) d.el.classList.remove('dragging');
    document.querySelectorAll('.fb-canvas.drop-target').forEach((c) => c.classList.remove('drop-target'));
    if (d.moved && this.opts.onDrop) {
      const canvas = document.querySelector('.fb-canvas');
      if (canvas) {
        const rect = canvas.getBoundingClientRect();
        if (ev.clientX >= rect.left && ev.clientX <= rect.right && ev.clientY >= rect.top && ev.clientY <= rect.bottom) {
          this.opts.onDrop(d.tpl, ev.clientX, ev.clientY);
        }
      }
    }
    this._dragging = null;
  }

  destroy() {
    window.removeEventListener('pointermove', this._pointerMoveHandler);
    window.removeEventListener('pointerup', this._pointerUpHandler);
    window.removeEventListener('pointercancel', this._pointerUpHandler);
    if (this._ghost) this._ghost.remove();
    this.root.innerHTML = '';
  }
}
