// =============================================================================
// File: sdk-runtime/data-specialised-renderer.js
// Description: Renderers for CalendarMonth (0x0223), Image (0x0224),
// VisuallyHidden (0x0225), LiveRegionComponent (0x0226) — chunk 3.3d-15.
//
// CalendarMonth: static month view (7-column grid). month BindRef
// "YYYY-MM", optional events_path (array of {date, tone?, label?}),
// show_week_numbers, first_day_of_week (sunday/monday). day_click handler.
//
// Image: <img> with src_ref BindRef, alt, width/height DimensionToken,
// fit (cover/contain/fill/none), aspect_ratio, radius, clickable, lazy_load.
//
// VisuallyHidden: screen-reader-only content (CSS clip pattern). Optional
// as_live (polite/assertive/off) → aria-live.
//
// LiveRegionComponent: aria-live region with politeness, content BindRef,
// visible bool, optional tone/icon/clear_after_ms. Auto-clear content
// after timeout.
//
// Spec ref: tentaflow-sdk-spec/src/protocol/ui/data/specialised.rs.
// =============================================================================

import {
  registerComponentRenderer,
  lookupComponentRenderer,
} from './component-renderer.js';
import { resolveBindRef, subscribeBindRef, assertBindRef } from './bind-resolver.js';
import {
  TONES,
  requireEnum, requireBool, requireU8, requireU16, requireString,
  requirePath, assertOnlyKnownFields,
} from './data-chart-shared.js';
import { renderIcon } from './icon-renderer.js';

// =============================================================================
// CalendarMonth (0x0223)
// =============================================================================

export const CALENDAR_MONTH_TAG = 0x0223;
const CALENDAR_MONTH_FIELD_KEYS = new Set([0, 1, 2, 3]);
const DAYS_OF_WEEK = new Set(['sunday', 'monday']);
const MONTH_RE = /^\d{4}-(?:0[1-9]|1[0-2])$/;
const DAY_NAMES_SUN = ['Su', 'Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa'];
const DAY_NAMES_MON = ['Mo', 'Tu', 'We', 'Th', 'Fr', 'Sa', 'Su'];

// Deliberately NOT <tf-calendar>: that element is an interactive scheduling
// calendar (day/week/month/timeline views, internal nav state, drag slot
// selection, datetime events {start,end,color}, Monday-first/Polish labels
// hardcoded) whose month-view day click navigates into the day view. The SDK
// CalendarMonth contract is a passive, host-bound month grid (BindRef month,
// {date,tone} event markers, sunday/monday first-day, week numbers, plain
// day_click). Adapting tf-calendar would require gating its header/nav/click
// behavior — non-additive changes — so this stays a hand-rolled grid styled
// by the shared .tf-calendar__* block in controls.css.
function renderCalendarMonth(component, ctx) {
  assertOnlyKnownFields(component.fields, CALENDAR_MONTH_FIELD_KEYS, 'CalendarMonth');

  const monthBind = ctx.readField(component.fields, 0);
  if (monthBind == null) throw new TypeError('CalendarMonth.month is required (BindRef)');
  assertBindRef(monthBind, 'CalendarMonth.month');
  const eventsPathRaw = ctx.readField(component.fields, 1);
  let eventsPath = null;
  if (eventsPathRaw != null) eventsPath = requirePath(eventsPathRaw, 'CalendarMonth.events_path');
  const showWeekNumbers = requireBool(ctx.readField(component.fields, 2), 'CalendarMonth.show_week_numbers');
  const firstDay = requireEnum(ctx.readField(component.fields, 3), DAYS_OF_WEEK, 'CalendarMonth.first_day_of_week');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-calendar-month');
  wrapper.setAttribute('role', 'grid');
  wrapper.setAttribute('aria-label', 'Calendar');

  let rebuildCleanups = [];
  const runRebuildCleanups = () => {
    for (const fn of rebuildCleanups) { try { fn(); } catch {} }
    rebuildCleanups = [];
  };
  ctx.registerCleanup(runRebuildCleanups);

  const rebuild = () => {
    runRebuildCleanups();
    wrapper.replaceChildren();
    const monthStr = resolveBindRef(monthBind, ctx.store);
    if (typeof monthStr !== 'string' || !MONTH_RE.test(monthStr)) {
      const err = document.createElement('div');
      err.classList.add('tf-calendar__error');
      err.textContent = monthStr == null ? '—' : `Invalid month: ${monthStr}`;
      wrapper.appendChild(err);
      return;
    }
    const [yearStr, monStr] = monthStr.split('-');
    const year = parseInt(yearStr, 10);
    const month = parseInt(monStr, 10) - 1;

    let events = [];
    if (eventsPath) {
      try {
        const arr = ctx.store.read(eventsPath);
        if (Array.isArray(arr)) events = arr;
      } catch { /* no data yet */ }
    }
    const eventMap = new Map();
    for (const ev of events) {
      if (ev == null || typeof ev !== 'object') continue;
      const d = ev.date;
      if (typeof d !== 'string') continue;
      if (!eventMap.has(d)) eventMap.set(d, []);
      eventMap.get(d).push(ev);
    }

    // Header: month name + year.
    const header = document.createElement('div');
    header.classList.add('tf-calendar__header');
    const title = document.createElement('span');
    title.classList.add('tf-calendar__title');
    const monthDate = new Date(year, month, 1);
    title.textContent = monthDate.toLocaleString(ctx.locale || 'en', { month: 'long', year: 'numeric' });
    header.appendChild(title);
    wrapper.appendChild(header);

    // Day-of-week header row.
    const dayNames = firstDay === 'monday' ? DAY_NAMES_MON : DAY_NAMES_SUN;
    const dowRow = document.createElement('div');
    dowRow.classList.add('tf-calendar__dow-row');
    if (showWeekNumbers) {
      const wk = document.createElement('span');
      wk.classList.add('tf-calendar__week-no', 'tf-calendar__dow-cell');
      wk.textContent = '#';
      dowRow.appendChild(wk);
    }
    for (const dn of dayNames) {
      const cell = document.createElement('span');
      cell.classList.add('tf-calendar__dow-cell');
      cell.textContent = dn;
      dowRow.appendChild(cell);
    }
    wrapper.appendChild(dowRow);

    // Build day grid.
    const firstOfMonth = new Date(year, month, 1);
    const lastOfMonth = new Date(year, month + 1, 0);
    const daysInMonth = lastOfMonth.getDate();
    let startDow = firstOfMonth.getDay(); // 0=Sun
    if (firstDay === 'monday') startDow = (startDow + 6) % 7;
    const totalCells = Math.ceil((startDow + daysInMonth) / 7) * 7;

    let week = null;
    for (let i = 0; i < totalCells; i++) {
      if (i % 7 === 0) {
        week = document.createElement('div');
        week.classList.add('tf-calendar__week');
        if (showWeekNumbers) {
          const dayInSlot = i - startDow + 1;
          const refDay = dayInSlot < 1 ? 1 : dayInSlot > daysInMonth ? daysInMonth : dayInSlot;
          const d = new Date(year, month, refDay);
          const wn = document.createElement('span');
          wn.classList.add('tf-calendar__week-no');
          wn.textContent = String(getISOWeek(d));
          week.appendChild(wn);
        }
        wrapper.appendChild(week);
      }
      const dayNum = i - startDow + 1;
      const cell = document.createElement('button');
      cell.type = 'button';
      cell.classList.add('tf-calendar__day');
      if (dayNum < 1 || dayNum > daysInMonth) {
        cell.classList.add('tf-calendar__day--outside');
        cell.disabled = true;
        cell.textContent = '';
      } else {
        cell.textContent = String(dayNum);
        const isoDate = `${yearStr}-${monStr}-${String(dayNum).padStart(2, '0')}`;
        cell.setAttribute('data-date', isoDate);
        const dayEvents = eventMap.get(isoDate);
        if (dayEvents && dayEvents.length > 0) {
          cell.classList.add('tf-calendar__day--has-event');
          const topTone = dayEvents[0].tone;
          if (topTone && TONES.has(topTone)) cell.classList.add(`tf-calendar__day--tone-${topTone}`);
        }
        const today = new Date();
        if (today.getFullYear() === year && today.getMonth() === month && today.getDate() === dayNum) {
          cell.classList.add('tf-calendar__day--today');
        }
        const onClick = () => {
          wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('day_click', {
            bubbles: false,
            detail: { date: isoDate, events: dayEvents || [] },
          }));
        };
        cell.addEventListener('click', onClick);
        rebuildCleanups.push(() => cell.removeEventListener('click', onClick));
      }
      week.appendChild(cell);
    }
  };
  rebuild();
  ctx.registerCleanup(subscribeBindRef(monthBind, ctx.store, rebuild));
  if (eventsPath) ctx.registerCleanup(ctx.store.subscribe(eventsPath, rebuild));
  return wrapper;
}

function getISOWeek(date) {
  const d = new Date(Date.UTC(date.getFullYear(), date.getMonth(), date.getDate()));
  d.setUTCDate(d.getUTCDate() + 4 - (d.getUTCDay() || 7));
  const yearStart = new Date(Date.UTC(d.getUTCFullYear(), 0, 1));
  return Math.ceil(((d - yearStart) / 86400000 + 1) / 7);
}

// =============================================================================
// Image (0x0224)
// =============================================================================

export const IMAGE_TAG = 0x0224;
const IMAGE_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5, 6, 7, 8]);
const IMAGE_FITS = new Set(['cover', 'contain', 'fill', 'none']);
const RADIUS_TOKENS = new Set(['none', 'xs', 'sm', 'md', 'lg', 'xl', 'pill', 'circle']);

const SPACING_TOKENS = new Set(['zero', 'xxs', 'xs', 'sm', 'md', 'lg', 'xl', 'xxl']);
export function parseDimensionToken(raw, ctx) {
  if (raw == null) return null;
  if (typeof raw !== 'object' || !raw.kind) throw new TypeError(`${ctx}: DimensionToken must have kind`);
  const kind = raw.kind;
  const keys = Object.keys(raw);
  const UNIT_KINDS = new Set(['auto', 'full', 'fit_content']);
  if (UNIT_KINDS.has(kind)) {
    if (keys.length !== 1) throw new TypeError(`${ctx}: kind=${kind} must not carry value`);
    return kind;
  }
  const VALUE_KINDS = { px: 'px', vh: 'vh', vw: 'vw', fr: 'fr', percent: '%', spacing: null };
  if (kind in VALUE_KINDS) {
    if (keys.length !== 2 || !('value' in raw)) throw new TypeError(`${ctx}: kind=${kind} requires exactly {kind, value}`);
    const raw_v = raw.value;
    if (kind === 'spacing') {
      if (!SPACING_TOKENS.has(raw_v)) throw new TypeError(`${ctx}: invalid spacing token '${raw_v}'`);
      return `var(--tf-space-${raw_v})`;
    }
    // CBOR decodes an unsigned dimension value as BigInt; normalize to Number
    // in the safe u32 range so px/vh/vw/fr/% tokens accept it uniformly.
    const v = typeof raw_v === 'bigint' && raw_v >= 0n && raw_v <= 0xFFFFFFFFn
      ? Number(raw_v)
      : raw_v;
    if (typeof v !== 'number' || !Number.isFinite(v) || v < 0 || v !== Math.floor(v)) {
      throw new TypeError(`${ctx}: value for kind=${kind} must be non-negative integer`);
    }
    return `${v}${VALUE_KINDS[kind]}`;
  }
  throw new TypeError(`${ctx}: unknown DimensionToken kind '${kind}'`);
}

export function parseAspectRatio(raw, ctx) {
  if (raw == null) return null;
  if (typeof raw !== 'object' || !raw.kind) throw new TypeError(`${ctx}: AspectRatio must have kind`);
  const keys = Object.keys(raw);
  const KNOWN = { '1:1': '1/1', '16:9': '16/9', '4:3': '4/3', '21:9': '21/9', '3:2': '3/2', '2:1': '2/1', '9:16': '9/16', '3:4': '3/4' };
  if (raw.kind in KNOWN) {
    if (keys.length !== 1) throw new TypeError(`${ctx}: kind='${raw.kind}' must not carry extra fields`);
    return KNOWN[raw.kind];
  }
  if (raw.kind === 'custom') {
    if (keys.length !== 2 || !('ratio' in raw)) throw new TypeError(`${ctx}: custom requires exactly {kind, ratio}`);
    if (typeof raw.ratio !== 'number' || !Number.isFinite(raw.ratio) || raw.ratio <= 0) {
      throw new TypeError(`${ctx}.ratio must be finite > 0`);
    }
    return String(raw.ratio);
  }
  throw new TypeError(`${ctx}: unknown AspectRatio kind '${raw.kind}'`);
}

function renderImage(component, ctx) {
  assertOnlyKnownFields(component.fields, IMAGE_FIELD_KEYS, 'Image');

  const srcBind = ctx.readField(component.fields, 0);
  if (srcBind == null) throw new TypeError('Image.src_ref is required (BindRef)');
  assertBindRef(srcBind, 'Image.src_ref');
  const alt = requireString(ctx.readField(component.fields, 1), 'Image.alt');
  const width = parseDimensionToken(ctx.readField(component.fields, 2), 'Image.width');
  const height = parseDimensionToken(ctx.readField(component.fields, 3), 'Image.height');
  const fit = requireEnum(ctx.readField(component.fields, 4), IMAGE_FITS, 'Image.fit');
  const aspectRatio = parseAspectRatio(ctx.readField(component.fields, 5), 'Image.aspect_ratio');
  const radiusRaw = ctx.readField(component.fields, 6);
  const radius = radiusRaw != null ? requireEnum(radiusRaw, RADIUS_TOKENS, 'Image.radius') : null;
  const clickable = requireBool(ctx.readField(component.fields, 7), 'Image.clickable');
  const lazyLoad = requireBool(ctx.readField(component.fields, 8), 'Image.lazy_load');

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-image');
  if (radius != null) wrapper.classList.add(`tf-image--radius-${radius}`);
  if (clickable) wrapper.classList.add('tf-image--clickable');
  if (width != null) wrapper.style.width = width;
  if (height != null) wrapper.style.height = height;
  if (aspectRatio != null) wrapper.style.aspectRatio = aspectRatio;

  const img = document.createElement('img');
  img.classList.add('tf-image__img');
  img.alt = alt;
  img.style.objectFit = fit;
  if (lazyLoad) img.loading = 'lazy';

  const apply = () => {
    const src = resolveBindRef(srcBind, ctx.store);
    if (src == null || typeof src !== 'string') {
      img.removeAttribute('src');
      return;
    }
    // XSS: only http(s) and data: URIs; reject javascript: and friends.
    if (/^javascript:/i.test(src.trim())) {
      img.removeAttribute('src');
      return;
    }
    img.src = src;
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(srcBind, ctx.store, apply));

  if (clickable) {
    const onClick = () => {
      wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('image_click', {
        bubbles: false,
        detail: { src: img.src },
      }));
    };
    wrapper.addEventListener('click', onClick);
    ctx.registerCleanup(() => wrapper.removeEventListener('click', onClick));
    wrapper.setAttribute('role', 'button');
    wrapper.tabIndex = 0;
  }
  wrapper.appendChild(img);
  return wrapper;
}

// =============================================================================
// VisuallyHidden (0x0225)
// =============================================================================

export const VISUALLY_HIDDEN_TAG = 0x0225;
const VH_FIELD_KEYS = new Set([0, 1]);
const LIVE_REGION_POLITENESS = new Set(['off', 'polite', 'assertive']);

function renderVisuallyHidden(component, ctx) {
  assertOnlyKnownFields(component.fields, VH_FIELD_KEYS, 'VisuallyHidden');

  const contentBind = ctx.readField(component.fields, 0);
  if (contentBind == null) throw new TypeError('VisuallyHidden.content is required (BindRef)');
  assertBindRef(contentBind, 'VisuallyHidden.content');
  const asLiveRaw = ctx.readField(component.fields, 1);
  const asLive = asLiveRaw != null ? requireEnum(asLiveRaw, LIVE_REGION_POLITENESS, 'VisuallyHidden.as_live') : null;

  const el = document.createElement('span');
  el.classList.add('tf-visually-hidden');
  if (asLive != null && asLive !== 'off') el.setAttribute('aria-live', asLive);

  const apply = () => {
    const v = resolveBindRef(contentBind, ctx.store);
    el.textContent = v == null ? '' : String(v);
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(contentBind, ctx.store, apply));
  return el;
}

// =============================================================================
// LiveRegionComponent (0x0226)
// =============================================================================

export const LIVE_REGION_TAG = 0x0226;
const LR_FIELD_KEYS = new Set([0, 1, 2, 3, 4, 5]);
const LR_VALID_POLITENESS = new Set(['polite', 'assertive']);

function renderLiveRegion(component, ctx) {
  assertOnlyKnownFields(component.fields, LR_FIELD_KEYS, 'LiveRegion');

  const politeness = requireEnum(ctx.readField(component.fields, 0), LR_VALID_POLITENESS, 'LiveRegion.politeness');
  const contentBind = ctx.readField(component.fields, 1);
  if (contentBind == null) throw new TypeError('LiveRegion.content is required (BindRef)');
  assertBindRef(contentBind, 'LiveRegion.content');
  const visible = requireBool(ctx.readField(component.fields, 2), 'LiveRegion.visible');
  const toneRaw = ctx.readField(component.fields, 3);
  const tone = toneRaw != null ? requireEnum(toneRaw, TONES, 'LiveRegion.tone') : null;
  const iconRaw = ctx.readField(component.fields, 4);
  let iconEl = null;
  if (iconRaw != null) iconEl = renderIcon(iconRaw, 'LiveRegion.icon');
  const clearAfterRaw = ctx.readField(component.fields, 5);
  let clearAfterMs = null;
  if (clearAfterRaw != null) {
    if (typeof clearAfterRaw === 'bigint') {
      if (clearAfterRaw < 0n || clearAfterRaw > 0xFFFFFFFFn) {
        throw new TypeError('LiveRegion.clear_after_ms must be u32 (0..4294967295)');
      }
      clearAfterMs = Number(clearAfterRaw);
    } else {
      if (typeof clearAfterRaw !== 'number' || !Number.isInteger(clearAfterRaw) || clearAfterRaw < 0 || clearAfterRaw > 0xFFFFFFFF) {
        throw new TypeError('LiveRegion.clear_after_ms must be u32 (0..4294967295)');
      }
      clearAfterMs = clearAfterRaw;
    }
  }

  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-live-region');
  if (tone != null) wrapper.classList.add(`tf-live-region--tone-${tone}`);
  wrapper.setAttribute('aria-live', politeness);
  wrapper.setAttribute('role', 'status');
  if (!visible) wrapper.classList.add('tf-visually-hidden');

  if (iconEl != null) {
    iconEl.classList.add('tf-live-region__icon');
    wrapper.appendChild(iconEl);
  }
  const contentEl = document.createElement('span');
  contentEl.classList.add('tf-live-region__content');
  wrapper.appendChild(contentEl);

  let clearTimer = null;
  const cancelClear = () => {
    if (clearTimer != null) { clearTimeout(clearTimer); clearTimer = null; }
  };
  ctx.registerCleanup(cancelClear);

  const apply = () => {
    cancelClear();
    const v = resolveBindRef(contentBind, ctx.store);
    contentEl.textContent = v == null ? '' : String(v);
    if (clearAfterMs != null && clearAfterMs > 0 && contentEl.textContent !== '') {
      clearTimer = setTimeout(() => { contentEl.textContent = ''; }, clearAfterMs);
    }
  };
  apply();
  ctx.registerCleanup(subscribeBindRef(contentBind, ctx.store, apply));
  return wrapper;
}

// =============================================================================
// Registration
// =============================================================================


// -----------------------------------------------------------------------------
// 0x0F01 — ZoneEditor (TentaFlow extension, outside catalog v1)
// -----------------------------------------------------------------------------

const ZONE_EDITOR_TAG = 0x0f01;
const ZONE_EDITOR_FIELD_KEYS = new Set([0, 1]);

/// Parses the persisted zone shape: `[[[x,y],...], ...]` normalized 0..1.
/// Anything malformed yields an empty set — the editor then starts blank
/// instead of throwing and taking the whole panel down.
function parseZones(raw) {
  if (typeof raw !== 'string' || raw.trim() === '') return [];
  let v;
  try { v = JSON.parse(raw); } catch { return []; }
  if (!Array.isArray(v)) return [];
  const out = [];
  for (const poly of v) {
    if (!Array.isArray(poly)) continue;
    const pts = [];
    for (const p of poly) {
      if (!Array.isArray(p) || p.length < 2) continue;
      const x = Number(p[0]); const y = Number(p[1]);
      if (Number.isFinite(x) && Number.isFinite(y)) pts.push([x, y]);
    }
    if (pts.length >= 3) out.push(pts);
  }
  return out;
}

/// Polygon zone editor over a still camera frame. Click places a vertex,
/// "Zamknij" closes the polygon, "Zapisz" emits `commit` carrying the full set
/// as the same JSON string the vision engine filters on. Coordinates are stored
/// NORMALIZED so a zone keeps its meaning across resolutions.
function renderZoneEditor(component, ctx) {
  assertOnlyKnownFields(component.fields, ZONE_EDITOR_FIELD_KEYS, 'ZoneEditor');
  const imageBind = ctx.readField(component.fields, 0);
  if (imageBind == null) throw new TypeError('ZoneEditor.image_ref is required (BindRef)');
  assertBindRef(imageBind, 'ZoneEditor.image_ref');
  const zonesBind = ctx.readField(component.fields, 1);
  if (zonesBind == null) throw new TypeError('ZoneEditor.zones_json is required (BindRef)');
  assertBindRef(zonesBind, 'ZoneEditor.zones_json');

  // The drawing box owns its geometry instead of inheriting it from the image:
  // with no background frame yet (a camera that has not recorded anything) an
  // image-sized wrapper collapses to zero height and the operator cannot draw at
  // all. Aspect ratio snaps to the real frame once one loads.
  const wrapper = document.createElement('div');
  wrapper.classList.add('tf-zone-editor');
  Object.assign(wrapper.style, {
    position: 'relative', display: 'block', width: '100%',
    aspectRatio: '16 / 9', background: '#111', overflow: 'hidden',
  });

  const img = document.createElement('img');
  img.classList.add('tf-zone-editor__img');
  img.alt = 'Podgląd kamery';
  // `fill` (not `contain`) keeps normalized zone coordinates aligned with the
  // box: the box IS the frame, so no letterboxing offset can creep in.
  Object.assign(img.style, {
    position: 'absolute', left: '0', top: '0',
    width: '100%', height: '100%', objectFit: 'fill',
  });

  const canvas = document.createElement('canvas');
  canvas.classList.add('tf-zone-editor__canvas');
  Object.assign(canvas.style, {
    position: 'absolute', left: '0', top: '0',
    width: '100%', height: '100%', cursor: 'crosshair',
  });

  let zones = parseZones(resolveBindRef(zonesBind, ctx.store));
  let current = [];

  const draw = () => {
    const w = canvas.clientWidth || 0;
    const h = canvas.clientHeight || 0;
    if (!w || !h) return;
    canvas.width = w; canvas.height = h;
    const g = canvas.getContext('2d');
    if (!g) return;
    g.clearRect(0, 0, w, h);
    const paint = (pts, close, stroke, fill) => {
      if (!pts.length) return;
      g.beginPath();
      g.moveTo(pts[0][0] * w, pts[0][1] * h);
      for (let i = 1; i < pts.length; i++) g.lineTo(pts[i][0] * w, pts[i][1] * h);
      if (close) g.closePath();
      if (fill) { g.fillStyle = fill; g.fill(); }
      g.strokeStyle = stroke; g.lineWidth = 2; g.stroke();
      g.fillStyle = stroke;
      for (const [px, py] of pts) {
        g.beginPath(); g.arc(px * w, py * h, 4, 0, Math.PI * 2); g.fill();
      }
    };
    for (const z of zones) paint(z, true, '#22cc66', 'rgba(34,204,102,0.25)');
    paint(current, false, '#ffcc00', null);
  };

  const applyImage = () => {
    const src = resolveBindRef(imageBind, ctx.store);
    if (typeof src !== 'string' || /^javascript:/i.test(src.trim())) {
      img.removeAttribute('src');
      return;
    }
    img.src = src;
  };
  const applyZones = () => {
    zones = parseZones(resolveBindRef(zonesBind, ctx.store));
    current = [];
    draw();
  };
  applyImage();
  ctx.registerCleanup(subscribeBindRef(imageBind, ctx.store, applyImage));
  ctx.registerCleanup(subscribeBindRef(zonesBind, ctx.store, applyZones));

  const onLoad = () => {
    if (img.naturalWidth && img.naturalHeight) {
      wrapper.style.aspectRatio = `${img.naturalWidth} / ${img.naturalHeight}`;
    }
    draw();
  };
  img.addEventListener('load', onLoad);
  ctx.registerCleanup(() => img.removeEventListener('load', onLoad));

  const onClick = (e) => {
    const r = canvas.getBoundingClientRect();
    if (!r.width || !r.height) return;
    const x = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    const y = Math.min(1, Math.max(0, (e.clientY - r.top) / r.height));
    current.push([Number(x.toFixed(4)), Number(y.toFixed(4))]);
    draw();
  };
  canvas.addEventListener('click', onClick);
  ctx.registerCleanup(() => canvas.removeEventListener('click', onClick));

  const bar = document.createElement('div');
  bar.classList.add('tf-zone-editor__bar');
  bar.style.marginTop = '8px';
  bar.style.display = 'flex';
  bar.style.gap = '8px';
  bar.style.flexWrap = 'wrap';
  const mkBtn = (label, fn) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.textContent = label;
    b.addEventListener('click', fn);
    ctx.registerCleanup(() => b.removeEventListener('click', fn));
    bar.appendChild(b);
    return b;
  };
  // A polygon needs three vertices to bound an area; closing with fewer is a
  // no-op rather than saving a degenerate zone the engine would discard.
  mkBtn('Zamknij wielokąt', () => {
    if (current.length >= 3) { zones.push(current); current = []; draw(); }
  });
  mkBtn('Cofnij punkt', () => {
    if (current.length) current.pop();
    else if (zones.length) current = zones.pop();
    draw();
  });
  mkBtn('Wyczyść wszystko', () => { zones = []; current = []; draw(); });
  mkBtn('Zapisz strefy', () => {
    if (current.length >= 3) { zones.push(current); current = []; }
    draw();
    wrapper.dispatchEvent(new (globalThis.CustomEvent || globalThis.Event)('commit', {
      bubbles: false,
      detail: { zones_json: JSON.stringify(zones) },
    }));
  });

  wrapper.appendChild(img);
  wrapper.appendChild(canvas);
  wrapper.appendChild(bar);
  return wrapper;
}

export function registerDataSpecialisedRenderers() {
  if (!lookupComponentRenderer(CALENDAR_MONTH_TAG)) registerComponentRenderer(CALENDAR_MONTH_TAG, renderCalendarMonth);
  if (!lookupComponentRenderer(IMAGE_TAG)) registerComponentRenderer(IMAGE_TAG, renderImage);
  if (!lookupComponentRenderer(VISUALLY_HIDDEN_TAG)) registerComponentRenderer(VISUALLY_HIDDEN_TAG, renderVisuallyHidden);
  if (!lookupComponentRenderer(ZONE_EDITOR_TAG)) registerComponentRenderer(ZONE_EDITOR_TAG, renderZoneEditor);
  if (!lookupComponentRenderer(LIVE_REGION_TAG)) registerComponentRenderer(LIVE_REGION_TAG, renderLiveRegion);
}
