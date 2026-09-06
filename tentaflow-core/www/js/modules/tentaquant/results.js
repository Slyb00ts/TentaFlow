// ===== File: modules/tentaquant/results.js — the project's "Wyniki" gallery (Q16) =====
//
// A run has two homes (plan §13.6): the laboratory's Runy table, and this
// gallery. The gallery is the one a person comes back to — it is a wall of
// PICTURES, so a result is recognised before it is read.
//
// Every thumbnail is drawn HERE, in the browser, from `runs.tile_json`: a
// handful of numbers written once when the run closed (`RunTile` — the kind,
// the heaviest counts, a convergence series or a short row of Bloch vectors).
// That is decision §18.27 and it is load-bearing: a server-drawn thumbnail
// would mean reading a state vector back off disk per tile and an image
// endpoint to serve it, and neither exists. A run with no tile is still listed
// — with the reason in its place, never with a fake picture.
//
// The gallery selects and compares: up to `COMPARE_MAX` runs travel to the run
// view's Porównanie tab, which is the only screen that draws them together.
//
// The mockup's toolbar also offers "Eksportuj zaznaczone"; this toolbar does
// not. `RunExport` is per-run and the wire has no bulk variant, so the button
// could only fire N exports and hand back N archives — a different feature
// with a different result, wearing the name of the one the mockup promises.
// The empty state likewise offers two of the mockup's three starts: the third
// ("Zacznij od przykładu") opens the examples catalogue, which this build does
// not have — the same reason `dash.start_hint` says so on the dashboard.

import { escapeHtml, escapeAttr } from '/js/utils.js';
import { T, sprite, fmtDate, parseServerTs, shortId } from '/js/modules/tentaquant/format.js';
import { runTier, runNodeName, runSourceLabel } from '/js/modules/tentaquant/run-model.js';
import '/js/components/tf-button.js';
import '/js/components/tf-checkbox.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-select.js';

/// Runs one comparison may hold — `RUN_COMPARE_MAX` on the wire.
export const COMPARE_MAX = 8;

/// The three tile shapes of Q16, as `RunTile::kind` names them.
export const TILE_KINDS = ['histogram', 'convergence', 'state'];

const PERIODS = { all: 0, day: 1, week: 7, month: 30 };

// ---------------------------------------------------------------------------
// The tile — pure
// ---------------------------------------------------------------------------

/// `runs.tile_json` as the gallery reads it, or null. The column carries the
/// document VERBATIM, so a tile written by a newer node with a kind this build
/// does not draw is reported as unknown rather than mis-drawn.
export function parseTile(run) {
  const raw = run && (run.tileJson ?? run.tile_json);
  if (!raw) return null;
  let value = null;
  try {
    value = typeof raw === 'string' ? JSON.parse(raw) : raw;
  } catch {
    return null;
  }
  if (!value || typeof value !== 'object') return null;
  const kind = String(value.kind || '');
  return {
    kind: TILE_KINDS.includes(kind) ? kind : '',
    countsTop: (value.counts_top ?? value.countsTop ?? [])
      .map((row) => ({ bitstring: String(row[0]), probability: Number(row[1]) || 0 })),
    series: (value.series || []).map(Number).filter(Number.isFinite),
    bloch: (value.bloch || []).map((v) => Array.from(v || [], Number).slice(0, 3)),
  };
}

/// A polyline over a series, fitted to the box. Flat series (every value the
/// same) sit on the middle line instead of collapsing onto the floor.
export function seriesPoints(series, width, height) {
  const list = (series || []).map(Number).filter(Number.isFinite);
  if (!list.length) return '';
  const min = Math.min(...list);
  const max = Math.max(...list);
  const span = max - min;
  const step = list.length > 1 ? width / (list.length - 1) : 0;
  return list.map((value, index) => {
    const y = span > 0 ? height - ((value - min) / span) * height : height / 2;
    return `${(index * step).toFixed(1)},${y.toFixed(1)}`;
  }).join(' ');
}

/// The tile picture as SVG markup. It is a string because a gallery repaints
/// dozens of these at once and the tiles carry no behaviour of their own —
/// every value in it is a number this function computed, so there is nothing
/// to escape but the bitstrings, which are drawn as bars and never as text.
export function tileSvg(tile) {
  if (!tile || !tile.kind) return '';
  if (tile.kind === 'histogram') {
    const bars = tile.countsTop.slice(0, 8);
    if (!bars.length) return '';
    const peak = Math.max(...bars.map((b) => b.probability), 1e-9);
    const slot = 160 / bars.length;
    return svg(bars.map((bar, i) => {
      const h = Math.max(2, (bar.probability / peak) * 74);
      return `<rect class="rt-bar" x="${(i * slot + slot * 0.18).toFixed(1)}" y="${(78 - h).toFixed(1)}" width="${(slot * 0.64).toFixed(1)}" height="${h.toFixed(1)}" rx="2"/>`;
    }).join(''));
  }
  if (tile.kind === 'convergence') {
    const points = seriesPoints(tile.series, 156, 70);
    if (!points) return '';
    return svg(`<line class="rt-grid" x1="0" y1="40" x2="160" y2="40"/>`
      + `<polyline class="rt-line" points="${points}"/>`);
  }
  const spheres = tile.bloch.slice(0, 4);
  if (!spheres.length) return '';
  const gap = 160 / spheres.length;
  return svg(spheres.map((vector, i) => {
    const cx = gap * i + gap / 2;
    const length = Math.min(1, Math.hypot(vector[0] || 0, vector[1] || 0, vector[2] || 0));
    const radius = Math.min(28, gap * 0.38);
    // The projection is the same "x to the right, z up" the Bloch row uses, so
    // a tile and the full sphere point the same way.
    const x2 = cx + (vector[0] || 0) * radius;
    const y2 = 40 - (vector[2] || 0) * radius;
    return `<circle class="rt-sph" cx="${cx.toFixed(1)}" cy="40" r="${radius.toFixed(1)}"/>`
      + `<line class="rt-vec${length < 0.95 ? ' is-mixed' : ''}" x1="${cx.toFixed(1)}" y1="40" x2="${x2.toFixed(1)}" y2="${y2.toFixed(1)}"/>`;
  }).join(''));
}

const svg = (body) => `<svg class="rt-svg" viewBox="0 0 160 80" preserveAspectRatio="none" aria-hidden="true">${body}</svg>`;

// ---------------------------------------------------------------------------
// Filtering — pure
// ---------------------------------------------------------------------------

export function resultFilterState(patch = {}) {
  return { query: '', tier: 'all', kind: 'all', period: 'month', ...patch };
}

/// The runs the gallery shows. A run with no tile still passes the KIND filter
/// only when the filter is "all": asking for histograms and being given a run
/// with no picture at all would be an answer to a different question.
export function filterResults(runs, filters = {}, { now = Date.now(), projectNames = new Map() } = {}) {
  const query = String(filters.query || '').trim().toLowerCase();
  const days = PERIODS[filters.period] ?? 0;
  const cutoff = days ? now - days * 86400000 : 0;
  return (runs || []).filter((run) => {
    if (filters.tier && filters.tier !== 'all' && runTier(run) !== filters.tier) return false;
    if (filters.kind && filters.kind !== 'all') {
      const tile = parseTile(run);
      if (!tile || tile.kind !== filters.kind) return false;
    }
    if (cutoff) {
      const started = parseServerTs(run.startedAt);
      if (started && started.getTime() < cutoff) return false;
    }
    if (!query) return true;
    return [run.runId, run.target, run.userName, projectNames.get(run.projectId), runTier(run)]
      .filter(Boolean).join(' ').toLowerCase()
      .includes(query);
  });
}

/// A run's headline in the gallery. A run carries no title of its own, so it is
/// named by what it computed — the same rule `RunComparison::label` follows.
export function resultTitle(run) {
  const metrics = run.metrics || {};
  const qubits = Number(metrics.qubits) || 0;
  const shots = Number(metrics.shots) || 0;
  if (qubits && shots) return T('results.title_shots', { q: qubits, n: shots });
  if (qubits) return T('results.title_state', { q: qubits });
  return runSourceLabel(run);
}

// ---------------------------------------------------------------------------
// The gallery
// ---------------------------------------------------------------------------

/// Draws the project's Wyniki tab into `host`. The selection lives on the
/// screen, not here: it survives a reload of the listing and is what the
/// "Porównaj zaznaczone" button hands to the run view.
export function drawResults(screen, host) {
  const state = screen.resultFilters;
  const runs = (screen.runs || []).filter((run) => run.status === 'succeeded');
  const projectNames = new Map((screen.projects || []).map((p) => [p.projectId, p.name]));
  const visible = filterResults(runs, state, { projectNames });
  const pinned = visible.filter((run) => run.pinnedAt);
  const rest = visible.filter((run) => !run.pinnedAt);
  const selected = screen.selectedResults;

  host.innerHTML = `
    <div class="tf-toolbar">
      <tf-searchbox id="tq-res-search" placeholder="${escapeAttr(T('results.search_placeholder'))}" debounce="200" value="${escapeAttr(state.query)}"></tf-searchbox>
      <tf-select id="tq-res-tier" value="${escapeAttr(state.tier)}">
        <option value="all">${escapeHtml(T('results.filter_tier_all'))}</option>
        <option value="T0">${escapeHtml(T('runs.tier_t0'))}</option>
        <option value="T1">${escapeHtml(T('runs.tier_t1'))}</option>
      </tf-select>
      <tf-select id="tq-res-kind" value="${escapeAttr(state.kind)}">
        <option value="all">${escapeHtml(T('results.filter_kind_all'))}</option>
        ${TILE_KINDS.map((kind) => `<option value="${kind}">${escapeHtml(T('results.kind_' + kind))}</option>`).join('')}
      </tf-select>
      <tf-select id="tq-res-period" value="${escapeAttr(state.period)}">
        <option value="day">${escapeHtml(T('results.period_day'))}</option>
        <option value="week">${escapeHtml(T('results.period_week'))}</option>
        <option value="month">${escapeHtml(T('results.period_month'))}</option>
        <option value="all">${escapeHtml(T('results.period_all'))}</option>
      </tf-select>
      <span class="tf-toolbar-spacer"></span>
      <tf-button variant="primary" icon="bar-chart" data-act="compare" ${selected.size >= 2 ? '' : 'disabled'}>${escapeHtml(T('results.compare', { n: selected.size }))}</tf-button>
    </div>
    ${visible.length ? `
      ${pinned.length ? sectionHtml(screen, 'pinned', pinned) : ''}
      ${sectionHtml(screen, 'all', rest)}
      <div class="tq-table-footer">
        <span>${escapeHtml(T('results.footer', { n: runs.length }))}</span>
        <span>${escapeHtml(T('results.footer_shown', { n: visible.length }))}</span>
        <span>${escapeHtml(T('results.footer_pinned', { n: pinned.length }))}</span>
        <span>${escapeHtml(T('results.footer_selected', { n: selected.size }))}</span>
      </div>`
      : `<tf-empty-state icon="bar-chart" title="${escapeAttr(T(runs.length ? 'results.empty_filtered' : 'results.empty'))}" message="${escapeAttr(T(runs.length ? 'results.empty_filtered_sub' : 'results.empty_sub'))}">
          ${runs.length ? '' : `
            <tf-button variant="primary" icon="play" data-act="start-notebook">${escapeHtml(T('results.empty_notebook'))}</tf-button>
            <tf-button variant="secondary" icon="chip" data-act="start-studio">${escapeHtml(T('results.empty_studio'))}</tf-button>`}
        </tf-empty-state>`}`;

  host.querySelector('#tq-res-search').addEventListener('search', (e) => {
    state.query = String(e.detail?.value ?? '');
    drawResults(screen, host);
  });
  for (const [id, key] of [['#tq-res-tier', 'tier'], ['#tq-res-kind', 'kind'], ['#tq-res-period', 'period']]) {
    host.querySelector(id).addEventListener('change', (e) => {
      state[key] = e.detail?.value ?? 'all';
      drawResults(screen, host);
    });
  }
  host.querySelector('[data-act="compare"]').addEventListener('click', () => {
    screen.openRunResult(Array.from(selected)[0], { tab: 'compare', compare: Array.from(selected) });
  });
  // The empty state's two starts are the mockup's own: a project with no runs
  // is one keystroke away from making one.
  for (const [act, tab] of [['start-notebook', 'notebook'], ['start-studio', 'studio']]) {
    host.querySelector(`[data-act="${act}"]`)?.addEventListener('click', () => screen.selectProjectTab(tab));
  }
  // The gallery redraws itself on every filter change, and the host OUTLIVES
  // those redraws — so the delegated listener is attached exactly once, or a
  // session of typing in the searchbox would open one run per keystroke.
  if (!host.dataset.tqResults) {
    host.dataset.tqResults = '1';
    host.addEventListener('click', (event) => {
      const star = event.target.closest('[data-pin]');
      if (star) {
        event.stopPropagation();
        screen.toggleResultPin(star.dataset.pin);
        return;
      }
      // A tick is a selection, never a way into the run.
      if (event.target.closest('tf-checkbox[data-select]')) {
        event.stopPropagation();
        return;
      }
      const tile = event.target.closest('[data-result]');
      if (tile) screen.openRunResult(tile.dataset.result);
    });
    // A tile is a button, so it answers the keyboard as one.
    host.addEventListener('keydown', (event) => {
      if (event.key !== 'Enter' && event.key !== ' ') return;
      const tile = event.target.closest('[data-result]');
      if (!tile) return;
      event.preventDefault();
      screen.openRunResult(tile.dataset.result);
    });
  }
  for (const box of host.querySelectorAll('tf-checkbox[data-select]')) {
    box.addEventListener('change', (event) => {
      event.stopPropagation();
      const runId = box.dataset.select;
      if (event.detail?.checked && selected.size < COMPARE_MAX) selected.add(runId);
      else selected.delete(runId);
      drawResults(screen, host);
    });
  }
}

function sectionHtml(screen, section, runs) {
  return `
    <div class="tq-section-head">
      <h3>${sprite(section === 'pinned' ? 'star' : 'grid-2x2')}${escapeHtml(T('results.section_' + section))}
        <span class="count">${runs.length}</span></h3>
      <span class="sub">${escapeHtml(T('results.section_' + section + '_sub'))}</span>
    </div>
    ${runs.length
      ? `<div class="res-grid">${runs.map((run) => tileHtml(screen, run)).join('')}</div>`
      : `<div class="hint">${escapeHtml(T('results.section_empty'))}</div>`}`;
}

function tileHtml(screen, run) {
  const tile = parseTile(run);
  const tier = runTier(run);
  const metrics = run.metrics || {};
  const selected = screen.selectedResults.has(run.runId);
  const picture = tileSvg(tile);
  return `
    <div class="res-tile${selected ? ' is-selected' : ''}" data-result="${escapeAttr(run.runId)}" role="button" tabindex="0">
      <div class="rt-top">
        <tf-checkbox data-select="${escapeAttr(run.runId)}" ${selected ? 'checked' : ''}></tf-checkbox>
        <span class="rt-kind">${escapeHtml(tile && tile.kind ? T('results.kind_' + tile.kind) : T('results.kind_none'))}</span>
        <tf-button variant="ghost" size="sm" icon="star" data-pin="${escapeAttr(run.runId)}"
          title="${escapeAttr(T(run.pinnedAt ? 'runs.unpin' : 'runs.pin'))}"
          class="${run.pinnedAt ? 'is-pinned' : ''}"></tf-button>
      </div>
      <div class="rt-chart">${picture || `<span class="rt-none">${escapeHtml(T('results.no_tile'))}</span>`}</div>
      <div class="rt-title">${escapeHtml(resultTitle(run))}</div>
      <div class="rt-sub mono">${escapeHtml(shortId(run.runId))} · ${escapeHtml(runSourceLabel(run))}</div>
      <div class="rt-meta">
        <span class="tier ${tier ? tier.toLowerCase() : 'off'}">${escapeHtml(tier ? `${T('runs.tier_' + tier.toLowerCase())} · ${runNodeName(run, screen.lab?.nodes || [])}` : run.target)}</span>
        ${metrics.shots ? `<span class="m">${escapeHtml(T('results.meta_shots', { n: Number(metrics.shots) }))}</span>` : ''}
        ${metrics.keyframes ? `<span class="m">${escapeHtml(T('results.meta_frames', { n: Number(metrics.keyframes) }))}</span>` : ''}
        <span class="date">${escapeHtml(fmtDate(run.startedAt))}</span>
      </div>
    </div>`;
}
