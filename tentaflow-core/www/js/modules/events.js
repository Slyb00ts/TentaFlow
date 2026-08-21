// =============================================================================
// File: modules/events.js
// Description: Zdarzenia — the browser over the run event log (§2.10). Three
//   bands: filters, <tf-run-timeline>, and the turn-grouped ledger next to the
//   record inspector.
//
//   The server returns STORED ROWS, not bars. Turning them into spans is
//   `lib/run-events.js`, shared with the Code Studio session tab so both hosts
//   plot the same numbers; this module owns only the screen: filters, the
//   grouped ledger and the inspector.
//
//   Filtering and paging are the SERVER's: every filter goes into the request
//   and the next page is the keyset cursor from the previous response. Arriving
//   with `?correlation=<id>` (the audit deep link) preselects that filter and
//   shows it as a clearable chip — nothing else is narrowed.
// =============================================================================

import { ApiBinary } from '/js/protocol/api-binary-shim.js';
import { byId, escapeHtml, escapeAttr, fmtMs, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { createVirtualList } from '/js/lib/virtual-list.js';
import {
  normalizeRow, plotFrom, actorLabel, rowName, rowDetail,
} from '/js/lib/run-events.js';
import '/js/components/tf-run-timeline.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-combobox.js';
import '/js/components/tf-segmented.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-button.js';

const PAGE_SIZE = 200;
// Loading the next page this far from the bottom keeps the list ahead of the
// scroll instead of stalling at the last row.
const LOAD_MORE_PX = 600;

// Every FlowOrigin slug (flow_engine/dispatcher.rs). The list is closed on the
// server — an unknown slug is a bad request — so it is spelled out here rather
// than collected from the rows: a chip must be offerable before any row of that
// origin has been seen.
const ORIGINS = [
  'chat', 'dashboard', 'project', 'code_studio', 'api', 'addon',
  'camera', 'meeting', 'scheduler', 'mesh', 'agent', 'system',
];

// Origin → colour class in events.css. Two slugs are abbreviated there
// (code_studio → o-code, scheduler → o-sched), so the mapping is explicit.
const ORIGIN_CLASS = {
  chat: 'o-chat',
  dashboard: 'o-dashboard',
  project: 'o-project',
  code_studio: 'o-code',
  api: 'o-api',
  addon: 'o-addon',
  camera: 'o-camera',
  meeting: 'o-meeting',
  scheduler: 'o-sched',
  mesh: 'o-mesh',
  agent: 'o-agent',
  system: 'o-system',
};

// Time windows offered by the range control; `all` sends no lower bound.
const RANGES = { '15m': 900_000, '1h': 3_600_000, '24h': 86_400_000, all: null };

// Item heights for the virtualizer. Two sets because under 720px events.css
// lays a row out on two lines.
const ROW_H = 33;
const HEAD_H = 37;
const ROW_H_NARROW = 52;
const HEAD_H_NARROW = 45;

const state = {
  origins: new Set(ORIGINS),
  actorId: null,
  // Set only by the audit deep link (`#/events?correlation=<id>`). It is a
  // server-side filter like every other one, and it is shown as a clearable
  // chip so an operator can tell a narrowed page from a full one.
  correlationId: null,
  range: 'all',
  search: '',
  rows: [],
  records: [],
  items: [],
  epoch: 0,
  cursor: null,
  hasMore: false,
  loading: false,
  loadError: null,
  scopedToSelf: false,
  selectedKey: null,
  hotBandId: null,
  // Guards against a slower answer to an older filter overwriting a newer one.
  requestSeq: 0,
};

let list = null;
let timeline = null;
let narrowQuery = null;
let onNarrowChange = null;

function t(key, vars) { return I18n.t(`events.${key}`, vars ?? null); }

function lang() { return I18n.getLanguage ? I18n.getLanguage() : 'pl'; }

/** Wall clock of one event. */
function clock(ms) {
  return new Date(ms).toLocaleTimeString(lang(), { hour12: false });
}

function originClass(origin) { return ORIGIN_CLASS[origin] ?? 'o-system'; }

// -----------------------------------------------------------------------------
// Ledger grouping
// -----------------------------------------------------------------------------

/**
 * Groups newest-first (the order the cursor pages in, so scrolling down goes
 * back in time), rows inside a group oldest-first (a turn reads forward).
 */
function buildItems(rows) {
  const groups = new Map();
  for (const row of rows) {
    const key = `${row.runId}#${row.turn ?? '-'}`;
    let group = groups.get(key);
    if (!group) {
      group = { key, runId: row.runId, turn: row.turn, rows: [], newestMs: row.atMs, first: row };
      groups.set(key, group);
    }
    group.rows.push(row);
    if (row.atMs > group.newestMs) group.newestMs = row.atMs;
    if (row.seq < group.first.seq) group.first = row;
  }

  const ordered = [...groups.values()].sort((a, b) => b.newestMs - a.newestMs);
  const items = [];
  for (const group of ordered) {
    group.rows.sort((a, b) => a.seq - b.seq);
    items.push({ type: 'head', group });
    for (const row of group.rows) items.push({ type: 'row', row });
  }
  return items;
}

function rowDuration(row) {
  return row.durationMs === null ? '—' : fmtMs(row.durationMs, lang());
}

// -----------------------------------------------------------------------------
// The API key ↔ user binding — the finding this screen exists to surface
// -----------------------------------------------------------------------------

/** A bound key names its user; an unbound one is a service key and says so. */
function bindingTag(row) {
  if (row.actorKind !== 'api_key') return '';
  return row.actorUserId
    ? `<span class="tag bound">${escapeHtml(t('key_bound'))}</span>`
    : `<span class="tag unbound">${escapeHtml(t('key_unbound'))}</span>`;
}

function bindingText(row) {
  if (row.actorUserId) return t('key_bound_to', { user: row.actorUserId });
  return t('key_service_no_binding');
}

// -----------------------------------------------------------------------------
// Rendering
// -----------------------------------------------------------------------------

function originChips() {
  return ORIGINS.map((origin) => `
    <tf-chip class="ev-chip" clickable variant="outline" status="accent"
             data-origin="${escapeAttr(origin)}"
             label="${escapeAttr(t(`origin_${origin}`))}">
      <span slot="lead" class="dot ${originClass(origin)}"></span>
    </tf-chip>
  `).join('');
}

function rangeOptions() {
  return Object.keys(RANGES)
    .map((key) => `<option value="${escapeAttr(key)}">${escapeHtml(t(`range_${key}`))}</option>`)
    .join('');
}

function renderHead(item) {
  const g = item.group;
  const row = g.first;
  const title = g.turn === null
    ? t('group_run', { run: g.runId.slice(0, 12) })
    : t('group_turn', { turn: g.turn });
  const sub = [
    t(`origin_${row.origin}`),
    actorLabel(row),
  ].filter(Boolean).map(escapeHtml).join(' · ');
  return `<div class="ev-turnhead">${escapeHtml(title)}
    <span class="sub">${sub} ${bindingTag(row)} · ${escapeHtml(clock(g.first.atMs))}</span>
  </div>`;
}

function renderRow(item) {
  const row = item.row;
  const selected = state.selectedKey === row.key ? ' data-sel' : '';
  const hot = state.hotBandId && row.bandId === state.hotBandId ? ' hot' : '';
  const band = row.bandId ? ` data-band="${escapeAttr(row.bandId)}"` : '';
  return `<div class="ev-row${hot}" data-key="${escapeAttr(row.key)}"${band}${selected}>
    <span class="ix">${escapeHtml(String(row.seq).padStart(4, '0'))}</span>
    <span class="kind"><span class="dot ${originClass(row.origin)}"></span>${escapeHtml(t(`kind_${row.kind}`))}</span>
    <span class="who">${escapeHtml(rowName(row))}</span>
    <span class="what">${escapeHtml(rowDetail(row))}</span>
    <span class="ms">${escapeHtml(rowDuration(row))}</span>
  </div>`;
}

function renderItem(_index, item) {
  return item.type === 'head' ? renderHead(item) : renderRow(item);
}

function itemHeight(_index, item) {
  const narrow = narrowQuery ? narrowQuery.matches : false;
  if (item.type === 'head') return narrow ? HEAD_H_NARROW : HEAD_H;
  return narrow ? ROW_H_NARROW : ROW_H;
}

function kv(label, value, muted = false) {
  const cls = muted ? ' class="muted"' : '';
  return `<dt>${escapeHtml(label)}</dt><dd${cls}>${value}</dd>`;
}

function inspectorKeySection(row) {
  if (row.actorKind !== 'api_key') return '';
  const binding = row.actorUserId
    ? `${escapeHtml(row.actorUserId)} <span class="tag bound">${escapeHtml(t('key_bound'))}</span>`
    : `<span class="tag unbound">${escapeHtml(t('key_unbound'))}</span> ${escapeHtml(t('key_service'))}`;
  return `<div class="ev-sec">
    <h4>${escapeHtml(t('key_section'))}</h4>
    <dl class="ev-kv">
      ${kv(t('key_name'), escapeHtml(row.actorId ?? t('unknown_actor')))}
      ${kv(t('key_binding'), binding)}
    </dl>
    <div class="ev-sec-actions">
      <tf-button variant="ghost" size="sm" id="events-filter-actor">${escapeHtml(t('key_show_runs'))}</tf-button>
    </div>
  </div>`;
}

function inspectorLinks(row) {
  const links = [
    [t('link_run'), row.runId],
    [t('link_session'), row.sessionId],
    [t('link_node'), row.nodeId],
    [t('link_call'), row.callId],
    [t('link_org'), row.orgId],
  ].filter(([, v]) => v);
  if (!links.length) return '';
  const text = links.map(([label, value]) => `${label}  ${value}`).join('\n');
  return `<div class="ev-sec">
    <h4>${escapeHtml(t('links'))}</h4>
    <div class="ev-pre">${escapeHtml(text)}</div>
  </div>`;
}

function chronometry(row) {
  const band = state.records.find((r) => r.id === row.bandId);
  if (!band || band.duration === null || band.ttft === null) return '';
  const waitPct = Math.min(90, (band.ttft / Math.max(1, band.duration)) * 100);
  return `<div class="ev-track-bar">
    <span class="ttft" style="width:${waitPct.toFixed(1)}%"></span>
    <span class="decode" style="width:${(100 - waitPct).toFixed(1)}%"></span>
  </div>`;
}

function renderInspector() {
  const insp = byId('events-inspector');
  const body = byId('events-body');
  if (!insp || !body) return;
  const row = state.rows.find((r) => r.key === state.selectedKey);
  if (!row) {
    insp.hidden = true;
    insp.innerHTML = '';
    body.classList.remove('with-inspector');
    return;
  }
  insp.hidden = false;
  body.classList.add('with-inspector');

  const band = state.records.find((r) => r.id === row.bandId) ?? null;
  const name = rowName(row);
  const heading = name ? `${t(`kind_${row.kind}`)} · ${name}` : t(`kind_${row.kind}`);
  const subParts = [
    t('seq_label', { seq: String(row.seq).padStart(4, '0') }),
    row.turn === null ? t('turn_unknown') : t('detail_turn', { turn: row.turn }),
    clock(row.atMs),
  ];

  const timing = [];
  if (band) {
    timing.push(kv(
      t('duration'),
      band.duration === null
        ? `<span class="muted">${escapeHtml(t('in_flight'))}</span>`
        : escapeHtml(fmtMs(band.duration, lang())),
    ));
    if (band.ttft !== null) {
      timing.push(kv(t('ttft'), escapeHtml(fmtMs(band.ttft, lang()))));
      if (band.duration !== null) {
        timing.push(kv(t('decoding'), escapeHtml(fmtMs(Math.max(0, band.duration - band.ttft), lang()))));
      }
    }
  } else if (row.durationMs !== null) {
    timing.push(kv(t('duration'), escapeHtml(fmtMs(row.durationMs, lang()))));
  }

  insp.innerHTML = `
    <div class="ev-insp-head">
      <div class="ev-insp-title">${escapeHtml(heading)}</div>
      <div class="ev-insp-sub">${escapeHtml(subParts.join(' · '))}</div>
    </div>
    <dl class="ev-kv">
      ${kv(t('origin'), `<span class="ev-origin"><span class="dot ${originClass(row.origin)}"></span>${escapeHtml(t(`origin_${row.origin}`))}</span>`)}
      ${kv(t('actor'), `${escapeHtml(actorLabel(row))} <span class="muted">(${escapeHtml(t(`actor_kind_${row.actorKind}`))})</span>`)}
      ${timing.join('\n      ')}
      ${kv(
        t('correlation'),
        row.correlationId
          ? escapeHtml(row.correlationId)
          : `<span class="muted">${escapeHtml(t('none_recorded'))}</span>`,
      )}
    </dl>
    ${inspectorKeySection(row)}
    <div class="ev-sec">
      <h4>${escapeHtml(t('details'))}</h4>
      ${chronometry(row)}
      <div class="ev-pre">${escapeHtml(prettyPayload(row))}</div>
    </div>
    ${inspectorLinks(row)}
  `;

  byId('events-filter-actor')?.addEventListener('click', () => {
    setActor(row.actorId ?? null);
  });
}

function prettyPayload(row) {
  if (row.payload) return JSON.stringify(row.payload, null, 2);
  return row.payloadJson || t('payload_unreadable');
}

function renderEmpty() {
  const host = byId('events-ledger');
  if (!host) return;
  const message = state.loadError
    ? t('load_failed', { error: state.loadError })
    : state.origins.size === 0
      ? t('empty_no_origin')
      : t('empty');
  host.classList.remove('vlist-host');
  host.innerHTML = `<div class="ev-empty">${escapeHtml(message)}</div>`;
}

// -----------------------------------------------------------------------------
// Coupling: ledger row ↔ timeline band
// -----------------------------------------------------------------------------

function setHot(bandId) {
  const next = bandId ?? null;
  if (next === state.hotBandId) return;
  state.hotBandId = next;
  const host = byId('events-ledger');
  if (host) {
    host.querySelectorAll('.ev-row.hot').forEach((el) => el.classList.remove('hot'));
    if (next) {
      host.querySelectorAll(`.ev-row[data-band="${CSS.escape(next)}"]`)
        .forEach((el) => el.classList.add('hot'));
    }
  }
  timeline?.highlight(next);
}

/**
 * Brings the band's first ledger row into view. A rendered row scrolls itself;
 * one the virtualizer has not materialised is reached by index instead.
 */
function scrollBandIntoView(bandId) {
  if (!bandId) return;
  const host = byId('events-ledger');
  const rendered = host?.querySelector(`.ev-row[data-band="${CSS.escape(bandId)}"]`);
  if (rendered) {
    rendered.scrollIntoView({ block: 'nearest' });
    return;
  }
  const idx = state.items.findIndex((it) => it.type === 'row' && it.row.bandId === bandId);
  if (idx >= 0) list?.scrollToIndex(idx);
}

function select(key) {
  if (state.selectedKey === key) return;
  const host = byId('events-ledger');
  host?.querySelectorAll('.ev-row[data-sel]').forEach((el) => el.removeAttribute('data-sel'));
  state.selectedKey = key;
  if (key) {
    host?.querySelector(`.ev-row[data-key="${CSS.escape(key)}"]`)?.setAttribute('data-sel', '');
  }
  const row = state.rows.find((r) => r.key === key);
  if (timeline) timeline.selected = row?.bandId ?? null;
  renderInspector();
}

// -----------------------------------------------------------------------------
// Loading
// -----------------------------------------------------------------------------

function fromMs() {
  const window = RANGES[state.range];
  return window === null ? null : Date.now() - window;
}

function buildRequest(cursor) {
  return {
    // Some([]) is not None: with every chip off NOTHING matches, which is the
    // opposite of no constraint, and the server keeps the two apart.
    origins: [...state.origins],
    actorId: state.actorId,
    correlationId: state.correlationId,
    fromMs: fromMs(),
    search: state.search || null,
    cursor,
    limit: PAGE_SIZE,
  };
}

async function loadPage({ reset }) {
  if (state.loading && !reset) return;
  if (!reset && !state.hasMore) return;
  state.loading = true;
  const ticket = reset ? ++state.requestSeq : state.requestSeq;

  try {
    const body = await ApiBinary.one('eventsBrowseRequest', buildRequest(reset ? null : state.cursor));
    if (ticket !== state.requestSeq) return;
    const rawRows = body.rows ?? [];
    const rows = rawRows.map(normalizeRow);
    state.rows = reset ? rows : state.rows.concat(rows);
    state.cursor = body.nextCursor ?? body.next_cursor ?? null;
    state.hasMore = !!state.cursor && rawRows.length > 0;
    state.scopedToSelf = !!(body.scopedToSelf ?? body.scoped_to_self);
    state.loadError = null;
    applyRows();
  } catch (err) {
    if (ticket !== state.requestSeq) return;
    state.loadError = err?.message ?? String(err);
    if (reset) {
      state.rows = [];
      state.records = [];
      state.items = [];
      state.cursor = null;
      state.hasMore = false;
      applyRows();
    }
    toast(t('load_failed', { error: state.loadError }), 'err');
  } finally {
    state.loading = false;
  }
}

/** Re-derives everything that follows from the loaded rows and patches the UI. */
function applyRows() {
  const plot = plotFrom(state.rows);
  state.records = plot.records;
  state.epoch = plot.epoch;
  state.items = buildItems(state.rows);

  if (timeline) {
    timeline.epoch = state.epoch;
    timeline.records = plot.shifted;
  }

  if (state.selectedKey && !state.rows.some((r) => r.key === state.selectedKey)) {
    state.selectedKey = null;
  }
  renderLedger();
  renderInspector();
  renderStatus();
  refreshActorOptions();
}

function renderLedger() {
  const host = byId('events-ledger');
  if (!host) return;
  if (!state.items.length) {
    if (list) { list.destroy(); list = null; }
    renderEmpty();
    return;
  }
  if (!list) {
    host.innerHTML = '';
    list = createVirtualList(host, {
      items: state.items,
      getItemHeight: itemHeight,
      renderItem,
      overscan: 8,
      onScroll: (_top, distanceFromBottom) => {
        if (distanceFromBottom < LOAD_MORE_PX) loadPage({ reset: false });
      },
    });
    return;
  }
  list.setItems(state.items);
}

function renderStatus() {
  const count = byId('events-count');
  if (count) count.setAttribute('label', t('loaded_count', { count: state.rows.length }));
  const scope = byId('events-scope');
  if (scope) {
    scope.hidden = !state.scopedToSelf;
    scope.setAttribute('label', t('scoped_to_self'));
  }
}

/**
 * The actor picker is built from the rows on screen. There is no catalogue
 * endpoint, so this is NOT every actor on the node and the label says so.
 */
function refreshActorOptions() {
  const picker = byId('events-actor');
  if (!picker) return;
  const seen = new Map();
  for (const row of state.rows) {
    if (!row.actorId || seen.has(row.actorId)) continue;
    seen.set(row.actorId, row);
  }
  const options = [...seen.values()]
    .sort((a, b) => a.actorId.localeCompare(b.actorId))
    .map((row) => ({
      value: row.actorId,
      label: row.actorId,
      description: row.actorKind === 'api_key'
        ? bindingText(row)
        : t(`actor_kind_${row.actorKind}`),
    }));
  picker.options = options;
}

/** Paints the deep-link chip. Hidden when nothing narrows the page. */
function renderCorrelationChip() {
  const chip = byId('events-correlation');
  if (!chip) return;
  chip.hidden = !state.correlationId;
  if (state.correlationId) {
    chip.setAttribute('label', t('correlation_filter', { id: state.correlationId }));
  }
}

function setCorrelation(correlationId) {
  state.correlationId = correlationId || null;
  renderCorrelationChip();
  loadPage({ reset: true });
}

function setActor(actorId) {
  state.actorId = actorId || null;
  const picker = byId('events-actor');
  if (picker && picker.value !== (actorId ?? '')) picker.value = actorId ?? '';
  loadPage({ reset: true });
}

// -----------------------------------------------------------------------------
// Screen
// -----------------------------------------------------------------------------

const EventsScreen = {
  get title() { return I18n.t('events.title'); },

  render(params = null) {
    // The filter is seeded BEFORE the markup so the chip renders narrowed on
    // the very first paint instead of flashing the unfiltered page.
    state.correlationId = params?.correlation || null;
    return `
      <div class="ev-shell">
        <div class="tf-toolbar ev-bar" id="events-bar">
          ${originChips()}
          <tf-combobox id="events-actor" clearable
            label="${escapeAttr(t('actor_picker_label'))}"
            placeholder="${escapeAttr(t('actor_placeholder'))}"></tf-combobox>
          <tf-chip variant="tag" tone="muted" size="xs"
            label="${escapeAttr(t('actor_scope_note'))}"></tf-chip>
          <tf-segmented id="events-range" value="all" size="sm">
            ${rangeOptions()}
          </tf-segmented>
          <tf-searchbox id="events-search" debounce="350"
            placeholder="${escapeAttr(t('search_placeholder'))}"></tf-searchbox>
          <tf-chip class="ev-chip" id="events-correlation" status="accent" removable
            label="" hidden></tf-chip>
          <tf-chip id="events-count" variant="tag" tone="muted" size="xs" label=""></tf-chip>
          <tf-chip class="ev-chip" id="events-scope" status="warn" dot label="" hidden></tf-chip>
        </div>

        <div class="ev-timeline">
          <tf-run-timeline class="ev-plot" id="events-timeline"></tf-run-timeline>
        </div>

        <div class="ev-body" id="events-body">
          <div class="ev-ledger" id="events-ledger">
            <div class="ev-empty">${escapeHtml(I18n.t('common.loading'))}</div>
          </div>
          <aside class="ev-inspector" id="events-inspector" hidden></aside>
        </div>
      </div>
    `;
  },

  async mount() {
    timeline = byId('events-timeline');
    renderCorrelationChip();
    byId('events-correlation')?.addEventListener('remove', () => setCorrelation(null));
    narrowQuery = window.matchMedia('(max-width: 720px)');
    onNarrowChange = () => list?.refresh();
    narrowQuery.addEventListener('change', onNarrowChange);

    byId('events-bar')?.addEventListener('click', (e) => {
      const chip = e.target.closest('tf-chip[data-origin]');
      if (!chip) return;
      const origin = chip.dataset.origin;
      if (state.origins.has(origin)) state.origins.delete(origin);
      else state.origins.add(origin);
      chip.setAttribute('status', state.origins.has(origin) ? 'accent' : 'neutral');
      loadPage({ reset: true });
    });

    byId('events-actor')?.addEventListener('change', (e) => {
      state.actorId = e.detail?.value ?? null;
      loadPage({ reset: true });
    });

    byId('events-range')?.addEventListener('change', (e) => {
      state.range = e.detail?.value ?? 'all';
      loadPage({ reset: true });
    });

    byId('events-search')?.addEventListener('search', (e) => {
      state.search = e.detail?.value ?? '';
      loadPage({ reset: true });
    });

    const ledger = byId('events-ledger');
    ledger?.addEventListener('pointerover', (e) => {
      const row = e.target.closest('.ev-row');
      setHot(row?.dataset.band ?? null);
    });
    ledger?.addEventListener('pointerleave', () => setHot(null));
    ledger?.addEventListener('click', (e) => {
      const row = e.target.closest('.ev-row');
      if (row) select(row.dataset.key);
    });

    timeline.addEventListener('record-hover', (e) => {
      const id = e.detail?.id ?? null;
      setHot(id);
      scrollBandIntoView(id);
    });
    timeline.addEventListener('record-select', (e) => {
      // A band id IS the key of the row that opened it, so the selection maps
      // straight back onto the ledger.
      if (e.detail?.id) select(e.detail.id);
    });

    await loadPage({ reset: true });
  },

  unmount() {
    if (narrowQuery && onNarrowChange) narrowQuery.removeEventListener('change', onNarrowChange);
    narrowQuery = null;
    onNarrowChange = null;
    if (list) { list.destroy(); list = null; }
    if (timeline) { timeline.destroy(); timeline = null; }
    state.rows = [];
    state.records = [];
    state.items = [];
    state.cursor = null;
    state.hasMore = false;
    state.loading = false;
    state.loadError = null;
    state.selectedKey = null;
    state.hotBandId = null;
    state.epoch = 0;
    state.scopedToSelf = false;
    state.origins = new Set(ORIGINS);
    state.actorId = null;
    state.correlationId = null;
    state.range = 'all';
    state.search = '';
  },
};

export default EventsScreen;
