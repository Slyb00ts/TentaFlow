// =============================================================================
// Plik: modules/flows-builder/palette.js
// Opis: Paleta node'ów Flow Buildera - ładuje templates z API, grupuje po
//       kategoriach, obsługuje wyszukiwanie, pointer drag (touch + mysz).
// =============================================================================

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { I18n } from '/js/i18n.js';
import { getNodeName, getNodeDescription } from '/js/modules/flows-builder/node-i18n.js';

const CATEGORY_ORDER = ['trigger', 'service', 'memory', 'transform', 'logic', 'filter', 'output', 'other'];

function categoryLabel(cat) {
  return I18n.t(`flows_palette.categories.${cat}`);
}

// Mapa node_type -> ikona kategorii (sprite id)
const TYPE_ICON = {
  trigger: 'bolt', start: 'bolt',
  llm: 'chip', embeddings: 'sparkle', reranker: 'sparkle',
  stt: 'mic', tts: 'speaker',
  memory: 'database',
  conversation_history: 'database', session_context: 'database',
  speaker_context: 'database', memory_analyzer: 'sparkle',
  condition: 'branch', switch: 'branch',
  template: 'code', transform: 'transform', router: 'transform',
  pii_filter: 'shield', tts_clean: 'shield',
  output: 'arrow-out', end: 'arrow-out',
};

const TYPE_VAR = {
  trigger: '--node-trigger', start: '--node-start',
  llm: '--node-llm', stt: '--node-stt', tts: '--node-tts',
  memory: '--node-memory',
  embeddings: '--node-embeddings', reranker: '--node-reranker',
  condition: '--node-condition', switch: '--node-switch',
  template: '--node-template', transform: '--node-transform',
  pii_filter: '--node-pii_filter', tts_clean: '--node-tts_clean',
  router: '--node-router', output: '--node-output', end: '--node-end',
  conversation_history: '--node-conversation_history',
  session_context: '--node-session_context',
  speaker_context: '--node-speaker_context',
  memory_analyzer: '--node-memory_analyzer',
};

function catFor(tpl) {
  const c = (tpl.category || '').toLowerCase();
  if (CATEGORY_ORDER.includes(c)) return c;
  // Sensible fallback: typ noda -> kategoria
  const t = tpl.node_type;
  if (t === 'trigger' || t === 'start') return 'trigger';
  if (['llm','stt','tts','embeddings','reranker'].includes(t)) return 'service';
  if (['memory','conversation_history','session_context','speaker_context','memory_analyzer'].includes(t)) return 'memory';
  if (['condition','switch'].includes(t)) return 'logic';
  if (['template','transform','router'].includes(t)) return 'transform';
  if (['pii_filter','tts_clean'].includes(t)) return 'filter';
  if (['output','end'].includes(t)) return 'output';
  return 'other';
}

export class FlowPalette {
  constructor(rootEl, opts = {}) {
    this.root = rootEl;
    this.opts = opts;
    this.templates = [];
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

    try {
      this.templates = await ApiBinary.list('flowNodeTemplatesListRequest', { arrayKey: 'templates' });
    } catch (err) {
      this.templates = [];
      this.listEl.innerHTML = `<div class="fb-palette-empty">${escapeHtml(I18n.t('flows_palette.load_error', { error: err.message }))}</div>`;
      return;
    }
    if (this.opts.onTemplatesLoaded) this.opts.onTemplatesLoaded(this.templates);
    this._render();
  }

  getTemplates() { return this.templates; }

  _render() {
    const groups = {};
    const total = this.templates.length;
    let shown = 0;
    for (const tpl of this.templates) {
      const c = catFor(tpl);
      const label = getNodeName(tpl.node_type, tpl.label).toLowerCase();
      const desc = getNodeDescription(tpl.node_type, tpl.description).toLowerCase();
      const type = (tpl.node_type || '').toLowerCase();
      const matches = !this.filter || label.includes(this.filter) || desc.includes(this.filter) || type.includes(this.filter);
      if (!matches) continue;
      if (!groups[c]) groups[c] = [];
      groups[c].push(tpl);
      shown += 1;
    }
    this.countEl.textContent = String(total);

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
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><polyline points="6 9 12 15 18 9"/></svg>
          </div>
          <div class="fb-palette-items">
            ${items.map((t) => this._renderItem(t)).join('')}
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

  _renderItem(tpl) {
    const iconId = TYPE_ICON[tpl.node_type] || 'chip';
    const varName = TYPE_VAR[tpl.node_type] || '--node-llm';
    const name = getNodeName(tpl.node_type, tpl.label);
    const desc = getNodeDescription(tpl.node_type, tpl.description);
    return `
      <div class="fb-node-item" data-node-type="${escapeAttr(tpl.node_type)}" style="--node-color: var(${varName})">
        <div class="fb-node-icon"><svg><use href="#i-${iconId}"/></svg></div>
        <div class="fb-node-info">
          <div class="fb-node-name">${escapeHtml(name)}</div>
          ${desc ? `<div class="fb-node-desc">${escapeHtml(desc)}</div>` : ''}
        </div>
      </div>`;
  }

  _onPointerDown(ev, el) {
    if (ev.button !== undefined && ev.button !== 0) return;
    ev.preventDefault();
    const nodeType = el.dataset.nodeType;
    const tpl = this.templates.find((t) => t.node_type === nodeType);
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
      this._ghost.style.setProperty('--node-color', `var(${TYPE_VAR[d.tpl.node_type] || '--node-llm'})`);
      this._ghost.textContent = getNodeName(d.tpl.node_type, d.tpl.label);
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
