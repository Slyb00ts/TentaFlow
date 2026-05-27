// =============================================================================
// File: tf-calendar.js
// Description: <tf-calendar> — full calendar with day/week/month/timeline views,
//              time-slot drag selection, positioned event blocks, Polish locale.
//              Light DOM, no external dependencies.
// Example:
//   const cal = document.querySelector('tf-calendar');
//   cal.events = [{id:'1', title:'Spotkanie', start:'2026-05-26T10:00', end:'2026-05-26T11:30', color:'blue'}];
// =============================================================================

const DAYS_SHORT = ['Pon', 'Wt', 'Śr', 'Czw', 'Pt', 'Sob', 'Nd'];
const MONTHS = [
  'Styczeń','Luty','Marzec','Kwiecień','Maj','Czerwiec',
  'Lipiec','Sierpień','Wrzesień','Październik','Listopad','Grudzień'
];
const HOUR_H = 60; // px per hour row
const VISIBLE_START = 7;
const VISIBLE_END = 20;

function iso(d) {
  return `${d.getFullYear()}-${String(d.getMonth()+1).padStart(2,'0')}-${String(d.getDate()).padStart(2,'0')}`;
}
function isoFull(d) {
  return `${iso(d)}T${String(d.getHours()).padStart(2,'0')}:${String(d.getMinutes()).padStart(2,'0')}`;
}
function monday(d) {
  const c = new Date(d); const day = c.getDay();
  c.setDate(c.getDate() + ((day === 0 ? -6 : 1) - day));
  c.setHours(0,0,0,0); return c;
}
function sameDay(a, b) {
  return a.getFullYear()===b.getFullYear() && a.getMonth()===b.getMonth() && a.getDate()===b.getDate();
}
function esc(s) { if (!s) return ''; const e = document.createElement('span'); e.textContent = s; return e.innerHTML; }
function weekNum(d) {
  const t = new Date(Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()));
  t.setUTCDate(t.getUTCDate() + 4 - (t.getUTCDay() || 7));
  const y = new Date(Date.UTC(t.getUTCFullYear(), 0, 1));
  return Math.ceil((((t - y) / 86400000) + 1) / 7);
}

// resolve overlapping events into columns
function layoutColumns(events) {
  if (!events.length) return [];
  const sorted = events.map(ev => ({
    ev, s: new Date(ev.start).getTime(), e: new Date(ev.end).getTime()
  })).sort((a,b) => a.s - b.s || a.e - b.e);
  const cols = [];
  for (const item of sorted) {
    let placed = false;
    for (let c = 0; c < cols.length; c++) {
      if (cols[c] <= item.s) { item.col = c; cols[c] = item.e; placed = true; break; }
    }
    if (!placed) { item.col = cols.length; cols.push(item.e); }
    item.total = 0; // filled later
  }
  const total = cols.length;
  for (const item of sorted) item.total = total;
  return sorted;
}

class TfCalendar extends HTMLElement {
  static get observedAttributes() { return ['view', 'date']; }

  constructor() {
    super();
    this._events = [];
    this._el = null;
    this._drag = null;
    this._bound = {
      click: this._onClick.bind(this),
      down: this._onDown.bind(this),
      move: this._onMove.bind(this),
      up: this._onUp.bind(this),
    };
  }

  connectedCallback() {
    if (!this._el) this._build();
    this._render();
  }
  disconnectedCallback() {
    document.removeEventListener('mousemove', this._bound.move);
    document.removeEventListener('mouseup', this._bound.up);
  }
  attributeChangedCallback() { if (this._el) this._render(); }

  get view() { return this.getAttribute('view') || 'week'; }
  set view(v) { this.setAttribute('view', v); }
  get date() { return this.getAttribute('date') || iso(new Date()); }
  set date(v) { this.setAttribute('date', v); }
  get value() { return this.date; }

  set events(arr) { this._events = Array.isArray(arr) ? arr : []; if (this._el) this._render(); }
  get events() { return this._events; }

  _build() {
    this.innerHTML = '';
    const el = document.createElement('div');
    el.className = 'tf-calendar';
    el.addEventListener('click', this._bound.click);
    el.addEventListener('mousedown', this._bound.down);
    this.appendChild(el);
    this._el = el;
  }

  _focus() {
    const p = this.date.split('-');
    const d = new Date(+p[0], +p[1]-1, +p[2]||1);
    return isNaN(d.getTime()) ? new Date() : d;
  }

  _nav(delta) {
    const d = this._focus();
    if (this.view === 'day') d.setDate(d.getDate() + delta);
    else if (this.view === 'week') d.setDate(d.getDate() + delta * 7);
    else if (this.view === 'timeline') d.setDate(d.getDate() + delta * 7);
    else d.setMonth(d.getMonth() + delta);
    this.setAttribute('date', iso(d));
    this.dispatchEvent(new CustomEvent('date-change', { bubbles: true, detail: { date: iso(d) } }));
  }

  _setView(v) {
    this.setAttribute('view', v);
    this.dispatchEvent(new CustomEvent('view-change', { bubbles: true, detail: { view: v } }));
  }

  // -- header with view switcher + nav + title --
  _headerHtml(title) {
    const v = this.view;
    const views = [['day','Dzień'],['week','Tydzień'],['month','Miesiąc'],['timeline','Oś czasu']];
    const pills = views.map(([k,l]) =>
      `<button type="button" class="tf-cal-vpill${v===k?' active':''}" data-setview="${k}">${l}</button>`
    ).join('');
    return `<div class="tf-cal-header">
      <div class="tf-cal-views">${pills}</div>
      <div class="tf-cal-nav">
        <button type="button" class="tf-btn tf-btn-ghost tf-btn-sm" data-nav="prev">
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M10 3L5 8l5 5"/></svg>
        </button>
        <button type="button" class="tf-btn tf-btn-ghost tf-btn-sm" data-nav="today">Dziś</button>
        <button type="button" class="tf-btn tf-btn-ghost tf-btn-sm" data-nav="next">
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M6 3l5 5-5 5"/></svg>
        </button>
      </div>
      <span class="tf-cal-title">${title}</span>
    </div>`;
  }

  // -- click dispatcher --
  _onClick(e) {
    const nav = e.target.closest('[data-nav]');
    if (nav) {
      if (nav.dataset.nav === 'today') {
        this.setAttribute('date', iso(new Date()));
        this.dispatchEvent(new CustomEvent('date-change', { bubbles: true, detail: { date: iso(new Date()) } }));
      } else this._nav(nav.dataset.nav === 'prev' ? -1 : 1);
      return;
    }
    const vb = e.target.closest('[data-setview]');
    if (vb) { this._setView(vb.dataset.setview); return; }
    const ev = e.target.closest('.tf-cal-event');
    if (ev) {
      const obj = this._events.find(x => String(x.id) === ev.dataset.id);
      if (obj) this.dispatchEvent(new CustomEvent('event-click', { bubbles: true, detail: { event: obj } }));
      return;
    }
    // month day click -> day view
    const dayEl = e.target.closest('.tf-cal-day[data-date]');
    if (dayEl && this.view === 'month') {
      this.setAttribute('date', dayEl.dataset.date);
      this._setView('day');
    }
  }

  // -- drag selection (day/week) --
  _onDown(e) {
    const cell = e.target.closest('.tf-cal-cell[data-col-date]');
    if (!cell || e.button !== 0) return;
    e.preventDefault();
    const grid = cell.closest('.tf-cal-scroll');
    const rect = grid.getBoundingClientRect();
    const y = e.clientY - rect.top + grid.scrollTop;
    this._drag = { grid, date: cell.dataset.colDate, startY: y, curY: y };
    document.addEventListener('mousemove', this._bound.move);
    document.addEventListener('mouseup', this._bound.up);
    this._drawSel();
  }
  _onMove(e) {
    if (!this._drag) return;
    const rect = this._drag.grid.getBoundingClientRect();
    this._drag.curY = e.clientY - rect.top + this._drag.grid.scrollTop;
    this._drawSel();
  }
  _onUp() {
    document.removeEventListener('mousemove', this._bound.move);
    document.removeEventListener('mouseup', this._bound.up);
    if (!this._drag) return;
    const d = this._drag; this._drag = null;
    const top = Math.min(d.startY, d.curY), bot = Math.max(d.startY, d.curY);
    if (bot - top < 5) { this._clearSel(); return; }
    const startMin = Math.round(top / (HOUR_H / 60));
    const endMin = Math.round(bot / (HOUR_H / 60));
    const sH = Math.floor(startMin / 60), sM = startMin % 60;
    const eH = Math.floor(endMin / 60), eM = endMin % 60;
    const start = `${d.date}T${String(sH).padStart(2,'0')}:${String(sM).padStart(2,'0')}`;
    const end = `${d.date}T${String(eH).padStart(2,'0')}:${String(eM).padStart(2,'0')}`;
    this.dispatchEvent(new CustomEvent('slot-select', { bubbles: true, detail: { start, end } }));
    this._clearSel();
  }
  _drawSel() {
    this._clearSel();
    if (!this._drag) return;
    const d = this._drag;
    const cols = d.grid.querySelectorAll(`.tf-cal-cell[data-col-date="${d.date}"]`);
    if (!cols.length) return;
    const col = cols[0];
    const sel = document.createElement('div');
    sel.className = 'tf-cal-selection';
    const top = Math.min(d.startY, d.curY), h = Math.abs(d.curY - d.startY);
    sel.style.cssText = `position:absolute;top:${top}px;left:0;right:0;height:${h}px;pointer-events:none;`;
    col.appendChild(sel);
  }
  _clearSel() {
    if (!this._el) return;
    this._el.querySelectorAll('.tf-cal-selection').forEach(s => s.remove());
  }

  // -- render dispatcher --
  _render() {
    const v = this.view;
    if (v === 'day') this._renderDay();
    else if (v === 'month') this._renderMonth();
    else if (v === 'timeline') this._renderTimeline();
    else this._renderWeek();
  }

  // -- hour grid (shared by day + week) --
  _hourGrid(days) {
    const today = new Date(), now = today.getHours();
    const nowMin = today.getMinutes();
    let html = `<div class="tf-cal-scroll" style="overflow-y:auto;position:relative;max-height:600px;">`;
    // column headers inside scroll area for sticky positioning
    html += `<div class="tf-cal-colheads" style="display:grid;grid-template-columns:60px repeat(${days.length},1fr);position:sticky;top:0;z-index:2;background:var(--tf-bg-card);">`;
    html += `<div class="tf-cal-h"></div>`;
    for (const d of days) {
      const t = sameDay(d, today);
      html += `<div class="tf-cal-h${t?' today':''}">${DAYS_SHORT[(d.getDay()+6)%7]} ${d.getDate()}</div>`;
    }
    html += `</div>`;
    // time grid body
    html += `<div class="tf-cal-tbody" style="display:grid;grid-template-columns:60px repeat(${days.length},1fr);position:relative;">`;
    // hour rows
    for (let h = 0; h < 24; h++) {
      html += `<div class="tf-cal-time" style="height:${HOUR_H}px;">${String(h).padStart(2,'0')}:00</div>`;
      for (const d of days) {
        html += `<div class="tf-cal-cell" data-col-date="${iso(d)}" style="height:${HOUR_H}px;position:relative;"></div>`;
      }
    }
    html += `</div>`;
    // current time indicator
    for (let i = 0; i < days.length; i++) {
      if (sameDay(days[i], today)) {
        const top = now * HOUR_H + (nowMin / 60) * HOUR_H;
        html += `<div class="tf-cal-now" style="position:absolute;top:${top + 28}px;left:60px;right:0;height:2px;background:var(--tf-danger);opacity:0.6;z-index:3;pointer-events:none;"></div>`;
        break;
      }
    }
    html += `</div>`;
    return html;
  }

  _placeEvents(days) {
    const scroll = this._el.querySelector('.tf-cal-scroll');
    const tbody = this._el.querySelector('.tf-cal-tbody');
    if (!scroll || !tbody) return;

    for (let di = 0; di < days.length; di++) {
      const d = days[di];
      const dayEvts = this._events.filter(ev => sameDay(new Date(ev.start), d));
      if (!dayEvts.length) continue;
      const laid = layoutColumns(dayEvts);
      const colCount = days.length;
      const colW = 100 / colCount;
      for (const item of laid) {
        const s = new Date(item.ev.start), e = new Date(item.ev.end || item.ev.start);
        const topMin = s.getHours() * 60 + s.getMinutes();
        const dur = Math.max((e - s) / 60000, 30);
        const top = topMin * (HOUR_H / 60);
        const h = Math.max(dur * (HOUR_H / 60), 18);
        const subW = colW / item.total;
        const left = colW * di + subW * item.col;
        const el = document.createElement('div');
        el.className = `tf-cal-event ev-${item.ev.color || 'blue'}`;
        el.dataset.id = item.ev.id;
        el.style.cssText = `position:absolute;top:${top}px;left:calc(60px + ${left}%);width:calc(${subW}% - 4px);height:${h}px;box-sizing:border-box;overflow:hidden;z-index:1;`;
        el.innerHTML = `<span class="tf-cal-et">${esc(item.ev.title)}</span>${item.ev.subtitle ? `<span class="tf-cal-es">${esc(item.ev.subtitle)}</span>` : ''}`;
        tbody.appendChild(el);
      }
    }
    scroll.scrollTop = VISIBLE_START * HOUR_H;
  }

  // -- day view --
  _renderDay() {
    const d = this._focus();
    const dayName = DAYS_SHORT[(d.getDay()+6)%7];
    const title = `${dayName}, ${d.getDate()} ${MONTHS[d.getMonth()]} ${d.getFullYear()}`;
    this._el.innerHTML = this._headerHtml(title) + this._hourGrid([d]);
    this._placeEvents([d]);
  }

  // -- week view --
  _renderWeek() {
    const f = this._focus(), mon = monday(f);
    const days = Array.from({length:7}, (_,i) => { const x = new Date(mon); x.setDate(mon.getDate()+i); return x; });
    const title = `Tydzień ${weekNum(mon)}, ${MONTHS[mon.getMonth()]} ${mon.getFullYear()}`;
    this._el.innerHTML = this._headerHtml(title) + this._hourGrid(days);
    this._placeEvents(days);
  }

  // -- month view --
  _renderMonth() {
    const f = this._focus(), year = f.getFullYear(), month = f.getMonth();
    const today = new Date();
    let html = this._headerHtml(`${MONTHS[month]} ${year}`);
    html += '<div class="tf-cal-grid tf-cal-grid-month">';
    for (const wd of DAYS_SHORT) html += `<div class="tf-cal-h">${wd}</div>`;
    const first = new Date(year, month, 1);
    const pad = (first.getDay() + 6) % 7;
    const total = new Date(year, month+1, 0).getDate();
    const prev = new Date(year, month, 0).getDate();
    for (let i = pad-1; i >= 0; i--) html += `<div class="tf-cal-day other"><span>${prev-i}</span></div>`;
    for (let d = 1; d <= total; d++) {
      const date = new Date(year, month, d);
      const t = sameDay(date, today);
      const evts = this._events.filter(ev => sameDay(new Date(ev.start), date));
      const pills = evts.slice(0,3).map(ev =>
        `<span class="tf-cal-event-pill ev-${ev.color||'blue'}" data-id="${ev.id}">${esc(ev.title)}</span>`
      ).join('');
      const more = evts.length > 3 ? `<span class="tf-cal-more">+${evts.length-3}</span>` : '';
      html += `<div class="tf-cal-day${t?' today':''}" data-date="${iso(date)}"><span>${d}</span>${pills}${more}</div>`;
    }
    const rem = (7 - ((pad + total) % 7)) % 7;
    for (let i = 1; i <= rem; i++) html += `<div class="tf-cal-day other"><span>${i}</span></div>`;
    html += '</div>';
    this._el.innerHTML = html;
  }

  // -- timeline view --
  _renderTimeline() {
    const f = this._focus(), mon = monday(f);
    const days = Array.from({length:7}, (_,i) => { const x = new Date(mon); x.setDate(mon.getDate()+i); return x; });
    const title = `Tydzień ${weekNum(mon)}, ${MONTHS[mon.getMonth()]} ${mon.getFullYear()}`;
    let html = this._headerHtml(title);
    html += '<div class="tf-cal-timeline">';
    for (const d of days) {
      const evts = this._events.filter(ev => sameDay(new Date(ev.start), d)).sort((a,b) => new Date(a.start) - new Date(b.start));
      const dayLabel = `${DAYS_SHORT[(d.getDay()+6)%7]}, ${d.getDate()} ${MONTHS[d.getMonth()]}`;
      html += `<div class="tf-cal-tl-day"><div class="tf-cal-tl-header">${dayLabel}</div>`;
      if (!evts.length) {
        html += `<div class="tf-cal-tl-empty">Brak wydarzeń</div>`;
      } else {
        for (const ev of evts) {
          const s = new Date(ev.start), e = new Date(ev.end);
          const time = `${String(s.getHours()).padStart(2,'0')}:${String(s.getMinutes()).padStart(2,'0')} – ${String(e.getHours()).padStart(2,'0')}:${String(e.getMinutes()).padStart(2,'0')}`;
          html += `<div class="tf-cal-timeline-entry ev-${ev.color||'blue'}" data-id="${ev.id}">
            <div class="tf-cal-et">${esc(ev.title)}</div>
            ${ev.subtitle ? `<div class="tf-cal-es">${esc(ev.subtitle)}</div>` : ''}
            <div class="tf-cal-tl-time">${time}</div>
          </div>`;
        }
      }
      html += `</div>`;
    }
    html += '</div>';
    this._el.innerHTML = html;
  }
}

customElements.define('tf-calendar', TfCalendar);
export { TfCalendar };
