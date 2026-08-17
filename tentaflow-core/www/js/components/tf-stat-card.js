// =============================================================================
// File: tf-stat-card.js
// Opis: KPI stat tile component. Displays a label, large value with optional
//       suffix, and a delta indicator with directional styling.
//       The `size` attribute (sm|md|lg) switches to the compact .tf-stat
//       variant (no card chrome). Pre-existing light-DOM children (e.g. an
//       SDK footnote) are preserved after the generated content.
// =============================================================================

const ACCENT_CLASSES = new Set(['success', 'danger', 'warning', 'info']);
const DELTA_TYPES = new Set(['up', 'down', 'warn', 'neutral']);
const SIZE_CLASSES = new Set(['sm', 'md', 'lg']);

// A neutral delta is plain context ("3 suites"), so it gets no glyph — the dash
// only added noise in front of every such line.
const DELTA_ARROWS = { up: '↑', down: '↓', warn: '⚠', neutral: '' };

function escapeHtml(value) {
  return String(value)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

class TfStatCard extends HTMLElement {
  static get observedAttributes() {
    return ['label', 'value', 'suffix', 'delta', 'delta-type', 'icon', 'accent', 'size'];
  }

  constructor() {
    super();
    this._root = null;
    this._main = null;
  }

  connectedCallback() {
    if (!this._root) this._build();
    this._update();
  }

  attributeChangedCallback() {
    if (this._root) this._update();
  }

  _build() {
    // Keep slotted children (appended by callers before connection) so
    // attribute-driven re-renders don't wipe them.
    const extras = [...this.childNodes];
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-stat-card';
    this._main = document.createElement('div');
    this._main.style.display = 'contents';
    el.appendChild(this._main);
    if (extras.length) {
      const extra = document.createElement('div');
      extra.style.display = 'contents';
      for (const node of extras) extra.appendChild(node);
      el.appendChild(extra);
    }
    this.appendChild(el);
    this._root = el;
  }

  _update() {
    const label = escapeHtml(this.getAttribute('label') || '');
    const value = escapeHtml(this.getAttribute('value') || '');
    const suffix = escapeHtml(this.getAttribute('suffix') || '');
    const delta = escapeHtml(this.getAttribute('delta') || '');
    const deltaType = this.getAttribute('delta-type') || 'neutral';
    const icon = (this.getAttribute('icon') || '').trim();
    const accent = this.getAttribute('accent') || '';
    const size = this.getAttribute('size') || '';

    const dtCls = DELTA_TYPES.has(deltaType) ? deltaType : 'neutral';
    const arrow = DELTA_ARROWS[dtCls] || '';
    const deltaHtml = delta
      ? `<span class="tf-stat-card-delta ${dtCls}">${arrow} ${delta}</span>`
      : '';

    if (SIZE_CLASSES.has(size)) {
      // Compact stat variant — plain label + value row, no card chrome.
      this._root.className = `tf-stat tf-stat--size-${size}`;
      const suffixHtml = suffix ? `<span class="suffix">${suffix}</span>` : '';
      this._main.innerHTML =
        `<span class="tf-stat__label">${label}</span>` +
        `<div class="tf-stat__value-row"><span class="tf-stat__value">${value}${suffixHtml}</span>${deltaHtml}</div>`;
      return;
    }

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
      parts.push(`<div class="tf-stat-card-delta ${dtCls}">${arrow} ${delta}</div>`);
    }

    this._main.innerHTML = parts.join('');
  }
}

customElements.define('tf-stat-card', TfStatCard);
export { TfStatCard };
