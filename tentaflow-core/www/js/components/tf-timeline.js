// =============================================================================
// File: tf-timeline.js
// Description: <tf-timeline> — vertical activity stream with colored dots,
//              tag chips and timestamps. Light DOM.
// Example: const tl = document.querySelector('tf-timeline');
//          tl.entries = [{title:'Created', time:'10:30', dotColor:'#22c55e'}];
// =============================================================================

class TfTimeline extends HTMLElement {
  constructor() {
    super();
    this._container = null;
    this._entries = [];
  }

  connectedCallback() {
    if (!this._container) this._build();
    this._render();
  }

  set entries(val) {
    this._entries = Array.isArray(val) ? val : [];
    if (this._container) this._render();
  }

  get entries() {
    return this._entries;
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-timeline';
    this.appendChild(el);
    this._container = el;
  }

  _render() {
    const entries = this._entries;

    if (!entries.length) {
      this._container.innerHTML = '';
      return;
    }

    const html = entries.map(entry => {
      const dotStyle = entry.dotColor ? ` style="background:${entry.dotColor};box-shadow:0 0 8px ${entry.dotColor}"` : '';

      const tagHtml = entry.tag
        ? `<span class="tf-chip ${entry.tagTone || 'info'}">${entry.tag}</span>`
        : '';

      const timeHtml = entry.time
        ? `<span class="tf-timeline-time">${entry.time}</span>`
        : '';

      const descHtml = entry.description
        ? `<div class="tf-timeline-desc">${entry.description}</div>`
        : '';

      return `<div class="tf-timeline-item"><div class="tf-timeline-dot"${dotStyle}></div><div class="tf-timeline-content"><div class="tf-timeline-head"><span class="tf-timeline-title">${entry.title || ''}</span>${tagHtml}${timeHtml}</div>${descHtml}</div></div>`;
    }).join('');

    this._container.innerHTML = html;
  }
}

customElements.define('tf-timeline', TfTimeline);
export { TfTimeline };
