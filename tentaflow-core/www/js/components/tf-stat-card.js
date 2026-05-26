// =============================================================================
// File: tf-stat-card.js
// Opis: KPI stat tile component. Displays a label, large value with optional
//       suffix, and a delta indicator with directional styling.
// =============================================================================

const ACCENT_CLASSES = new Set(['success', 'danger', 'warning', 'info']);
const DELTA_TYPES = new Set(['up', 'down', 'warn', 'neutral']);

const DELTA_ARROWS = { up: '↑', down: '↓', warn: '⚠', neutral: '—' };

class TfStatCard extends HTMLElement {
  static get observedAttributes() {
    return ['label', 'value', 'suffix', 'delta', 'delta-type', 'icon', 'accent'];
  }

  constructor() {
    super();
    this._root = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-stat-card';
    this.appendChild(el);
    this._root = el;
  }

  _update() {
    const label = this.getAttribute('label') || '';
    const value = this.getAttribute('value') || '';
    const suffix = this.getAttribute('suffix') || '';
    const delta = this.getAttribute('delta') || '';
    const deltaType = this.getAttribute('delta-type') || 'neutral';
    const icon = (this.getAttribute('icon') || '').trim();
    const accent = this.getAttribute('accent') || '';

    const cls = ['tf-stat-card'];
    if (ACCENT_CLASSES.has(accent)) cls.push(`accent-${accent}`);
    this._root.className = cls.join(' ');

    const parts = [];

    // Label row
    const iconHtml = icon
      ? `<svg class="tf-stat-card-icon" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><use href="/img/icons.svg#icon-${icon}"/></svg>`
      : '';
    parts.push(`<div class="tf-stat-card-label">${iconHtml}${label}</div>`);

    // Value row
    const suffixHtml = suffix ? `<span class="suffix">${suffix}</span>` : '';
    parts.push(`<div class="tf-stat-card-value">${value}${suffixHtml}</div>`);

    // Delta row
    if (delta) {
      const dtCls = DELTA_TYPES.has(deltaType) ? deltaType : 'neutral';
      const arrow = DELTA_ARROWS[dtCls] || '';
      parts.push(`<div class="tf-stat-card-delta ${dtCls}">${arrow} ${delta}</div>`);
    }

    this._root.innerHTML = parts.join('');
  }
}

customElements.define('tf-stat-card', TfStatCard);
export { TfStatCard };
