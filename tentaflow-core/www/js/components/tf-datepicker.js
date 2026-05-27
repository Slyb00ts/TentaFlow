// =============================================================================
// File: tf-datepicker.js
// Description: <tf-datepicker> — calendar date selector with month navigation,
//              range selection support, min/max constraints. Light DOM.
// Example:
//   <tf-datepicker value="2026-05-26"></tf-datepicker>
// =============================================================================

const WDAY_LABELS = ['Pn', 'Wt', 'Śr', 'Cz', 'Pt', 'So', 'Nd'];

function toIso(d) {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const dd = String(d.getDate()).padStart(2, '0');
  return `${y}-${m}-${dd}`;
}

function parseIso(s) {
  if (!s) return null;
  const p = s.split('-');
  const d = new Date(+p[0], +p[1] - 1, +p[2] || 1);
  return isNaN(d.getTime()) ? null : d;
}

function sameDay(a, b) {
  return a && b &&
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate();
}

class TfDatepicker extends HTMLElement {
  static get observedAttributes() { return ['value', 'min', 'max', 'range-start', 'range-end']; }

  constructor() {
    super();
    this._container = null;
    this._viewYear = null;
    this._viewMonth = null;
    this._onClick = this._onClick.bind(this);
  }

  connectedCallback() {
    const focus = parseIso(this.getAttribute('value')) || new Date();
    this._viewYear = focus.getFullYear();
    this._viewMonth = focus.getMonth();
    if (!this._container) this._build();
    this._render();
  }

  disconnectedCallback() {
    if (this._container) this._container.removeEventListener('click', this._onClick);
  }

  attributeChangedCallback() { if (this._container) this._render(); }

  get value() { return this.getAttribute('value') || ''; }
  set value(v) {
    if (v !== this.value) this.setAttribute('value', v ?? '');
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-datepicker';
    el.addEventListener('click', this._onClick);
    this.appendChild(el);
    this._container = el;
  }

  _onClick(e) {
    const nav = e.target.closest('[data-nav]');
    if (nav) {
      if (nav.dataset.nav === 'prev') {
        this._viewMonth--;
        if (this._viewMonth < 0) { this._viewMonth = 11; this._viewYear--; }
      } else {
        this._viewMonth++;
        if (this._viewMonth > 11) { this._viewMonth = 0; this._viewYear++; }
      }
      this._render();
      return;
    }
    const dayEl = e.target.closest('.tf-dp-day:not(.disabled)');
    if (dayEl && dayEl.dataset.date) {
      this.setAttribute('value', dayEl.dataset.date);
      const d = parseIso(dayEl.dataset.date);
      this.dispatchEvent(new CustomEvent('change', { bubbles: true, detail: { value: dayEl.dataset.date, date: d } }));
    }
  }

  _render() {
    const selected = parseIso(this.value);
    const minD = parseIso(this.getAttribute('min'));
    const maxD = parseIso(this.getAttribute('max'));
    const rangeStart = parseIso(this.getAttribute('range-start'));
    const rangeEnd = parseIso(this.getAttribute('range-end'));
    const today = new Date();

    const y = this._viewYear;
    const m = this._viewMonth;
    const monthNames = ['Styczen', 'Luty', 'Marzec', 'Kwiecien', 'Maj', 'Czerwiec', 'Lipiec', 'Sierpien', 'Wrzesien', 'Pazdziernik', 'Listopad', 'Grudzien'];

    let html = `<div class="tf-dp-header">
      <button type="button" class="tf-btn tf-btn-ghost tf-btn-sm" data-nav="prev">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M10 3L5 8l5 5"/></svg>
      </button>
      <span>${monthNames[m]} ${y}</span>
      <button type="button" class="tf-btn tf-btn-ghost tf-btn-sm" data-nav="next">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 3l5 5-5 5"/></svg>
      </button>
    </div>`;

    html += '<div class="tf-dp-grid">';
    for (const wd of WDAY_LABELS) {
      html += `<div class="tf-dp-wday">${wd}</div>`;
    }

    const first = new Date(y, m, 1);
    const startPad = (first.getDay() + 6) % 7;
    const daysInMonth = new Date(y, m + 1, 0).getDate();
    const prevDays = new Date(y, m, 0).getDate();

    // Previous month
    for (let i = startPad - 1; i >= 0; i--) {
      const dayNum = prevDays - i;
      const d = new Date(y, m - 1, dayNum);
      html += `<div class="tf-dp-day other" data-date="${toIso(d)}">${dayNum}</div>`;
    }

    // Current month
    for (let day = 1; day <= daysInMonth; day++) {
      const d = new Date(y, m, day);
      const iso = toIso(d);
      const classes = ['tf-dp-day'];

      if (sameDay(d, today)) classes.push('today');
      if (selected && sameDay(d, selected)) classes.push('selected');
      if (minD && d < minD) classes.push('disabled');
      if (maxD && d > maxD) classes.push('disabled');

      // Range
      if (rangeStart && sameDay(d, rangeStart)) classes.push('range-start');
      if (rangeEnd && sameDay(d, rangeEnd)) classes.push('range-end');
      if (rangeStart && rangeEnd && d > rangeStart && d < rangeEnd) classes.push('range');

      html += `<div class="${classes.join(' ')}" data-date="${iso}">${day}</div>`;
    }

    // Next month padding
    const totalCells = startPad + daysInMonth;
    const remaining = (7 - (totalCells % 7)) % 7;
    for (let i = 1; i <= remaining; i++) {
      const d = new Date(y, m + 1, i);
      html += `<div class="tf-dp-day other" data-date="${toIso(d)}">${i}</div>`;
    }

    html += '</div>';
    this._container.innerHTML = html;
  }
}

customElements.define('tf-datepicker', TfDatepicker);
export { TfDatepicker };
