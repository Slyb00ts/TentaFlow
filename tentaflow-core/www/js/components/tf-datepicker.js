// =============================================================================
// File: tf-datepicker.js
// Description: <tf-datepicker> — calendar date selector with month navigation,
//              range selection support, min/max constraints. Light DOM.
//              Weekday names, month names and the first day of the week come
//              from Intl in the UI language; the value is always `YYYY-MM-DD`.
//              Arrow keys move between days, PageUp/PageDown between months.
// Example:
//   <tf-datepicker value="2026-05-26"></tf-datepicker>
// =============================================================================

import { I18n } from '/js/i18n.js';
import { firstWeekday, monthTitle, weekdayLabels } from '/js/lib/date-format.js';

const t = (key) => I18n.t(`date_field.${key}`);

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

// The selected day (or the first of the month when nothing is selected) is the one tab stop of the grid.
function dayCell(classes, date, label, tabStop) {
  const disabled = classes.includes('disabled');
  return `<button type="button" class="${classes.join(' ')}" data-date="${toIso(date)}" tabindex="${tabStop ? 0 : -1}"${disabled ? ' disabled' : ''}>${label}</button>`;
}

class TfDatepicker extends HTMLElement {
  static get observedAttributes() { return ['value', 'min', 'max', 'range-start', 'range-end']; }

  constructor() {
    super();
    this._container = null;
    this._viewYear = null;
    this._viewMonth = null;
    this._onClick = this._onClick.bind(this);
    this._onKeydown = this._onKeydown.bind(this);
  }

  connectedCallback() {
    this._showMonthOf(parseIso(this.getAttribute('value')) || new Date());
    if (!this._container) this._build();
    this._render();
  }

  attributeChangedCallback(name, oldVal, newVal) {
    if (oldVal === newVal || !this._container) return;
    if (name === 'value') this._showMonthOf(parseIso(newVal) || new Date());
    this._render();
  }

  _showMonthOf(date) {
    this._viewYear = date.getFullYear();
    this._viewMonth = date.getMonth();
  }

  get value() { return this.getAttribute('value') || ''; }
  set value(v) {
    if (v !== this.value) this.setAttribute('value', v ?? '');
  }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-datepicker';
    el.addEventListener('click', this._onClick);
    el.addEventListener('keydown', this._onKeydown);
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

  _onKeydown(e) {
    const day = e.target.closest?.('.tf-dp-day');
    if (!day) return;
    const step = { ArrowLeft: -1, ArrowRight: 1, ArrowUp: -7, ArrowDown: 7 }[e.key];
    const pageStep = { PageUp: -1, PageDown: 1 }[e.key];
    if (step === undefined && pageStep === undefined) return;
    e.preventDefault();
    e.stopPropagation();
    const from = parseIso(day.dataset.date);
    const target = step !== undefined
      ? new Date(from.getFullYear(), from.getMonth(), from.getDate() + step)
      : new Date(from.getFullYear(), from.getMonth() + pageStep, 1);
    this._showMonthOf(target);
    this._render();
    this._container.querySelector(`.tf-dp-day[data-date="${toIso(target)}"]`)?.focus();
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

    let html = `<div class="tf-dp-header">
      <button type="button" class="tf-btn tf-btn-ghost tf-btn-sm" data-nav="prev" aria-label="${t('prev_month')}">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M10 3L5 8l5 5"/></svg>
      </button>
      <span aria-live="polite">${monthTitle(y, m)}</span>
      <button type="button" class="tf-btn tf-btn-ghost tf-btn-sm" data-nav="next" aria-label="${t('next_month')}">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 3l5 5-5 5"/></svg>
      </button>
    </div>`;

    html += '<div class="tf-dp-grid">';
    for (const wd of weekdayLabels()) {
      html += `<div class="tf-dp-wday">${wd}</div>`;
    }

    const tabDay = selected && selected.getFullYear() === y && selected.getMonth() === m ? selected.getDate() : 1;
    const first = new Date(y, m, 1);
    const startPad = (first.getDay() - firstWeekday() + 7) % 7;
    const daysInMonth = new Date(y, m + 1, 0).getDate();
    const prevDays = new Date(y, m, 0).getDate();

    // Previous month
    for (let i = startPad - 1; i >= 0; i--) {
      const dayNum = prevDays - i;
      const d = new Date(y, m - 1, dayNum);
      html += dayCell(['tf-dp-day', 'other'], d, dayNum, false);
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

      html += dayCell(classes, d, day, day === tabDay);
    }

    // Next month padding
    const totalCells = startPad + daysInMonth;
    const remaining = (7 - (totalCells % 7)) % 7;
    for (let i = 1; i <= remaining; i++) {
      const d = new Date(y, m + 1, i);
      html += dayCell(['tf-dp-day', 'other'], d, i, false);
    }

    html += '</div>';
    this._container.innerHTML = html;
  }
}

customElements.define('tf-datepicker', TfDatepicker);
export { TfDatepicker };
