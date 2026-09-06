// ===== File: modules/tentaquant/runs.js — Q08, the Runy tab (laboratory and project) =====
//
// One table over `Run::List`: every run the caller may see — their own, and
// everybody's for a supervisor — with the filters of the mockup (tier, status,
// person) and a footer that says how much of the laboratory is on screen. The
// same view serves the project's own "Runy projektu" tab: a project run is the
// same row, the request just carries `project_id`, so there is one renderer
// here and not two that drift.
//
// What the mockup shows and this does NOT build: "porównaj zaznaczone" and the
// results gallery. `Run::Compare` and `runs.tile_json` are not on the wire yet,
// and a control that answers nothing is worse than no control — the comparison
// arrives with its backend.
//
// A row opens the run detail UNDER the table, which is where Q08 puts it.

import { escapeHtml, escapeAttr, fmtMs } from '/js/utils.js';
import { T, has, shortId } from '/js/modules/tentaquant/format.js';
import {
  RUN_STATUSES, canControlRun, cancelRun, filterRuns, runIsLive, runNodeName, runSourceLine,
  runStatusLabel, runStatusTone, runDurationMs, runTier, runUsers, setRunPinned,
} from '/js/modules/tentaquant/run-model.js';
import { drawRunDetail } from '/js/modules/tentaquant/run-detail.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-empty-state.js';
import '/js/components/tf-searchbox.js';
import '/js/components/tf-select.js';
import '/js/components/tf-table.js';

export function runFilterState(patch = {}) {
  return { query: '', tier: 'all', status: 'all', user: 'all', ...patch };
}

/// The tier pill of the run table is a `tf-chip`, not the `.tier` span the rest
/// of the screen draws: a cell lands INSIDE the tf-table shadow root, which
/// adopts controls.css and nothing else, so a class from tentaquant.css would
/// arrive unstyled. The outline tones carry the same two colours the mockup
/// fixes for the tiers — `info` is `--tf-info` (`--tq-browser`), `accent` is
/// `--tf-accent-3` (`--tq-core`) — and an unknown target keeps its identity
/// without a colour, exactly like `.tier.off`.
const TIER_TONE = { T0: 'info', T1: 'accent' };

/// One table row. Pure, so the two-line cells and the numbers of Q08 are
/// checkable without a DOM. Every class in the markup is a controls.css class,
/// and every pill is a component that brings its own styling into the shadow
/// root it is rendered in.
export function runTableRow(run, { projectName = '', nodes = [], now } = {}) {
  const tier = runTier(run);
  const metrics = run.metrics || {};
  const qubits = Number(metrics.qubits) || 0;
  const gates = Number(metrics.gates) || 0;
  const shots = Number(metrics.shots) || 0;
  const duration = runDurationMs(run, now);
  return {
    _run: run.runId,
    run: `<div class="tf-table__cell-title tf-table__cell-sub--mono">${escapeHtml(shortId(run.runId))}</div>`
      + `<div class="tf-table__cell-sub">${escapeHtml(run.userName || run.userId || '')}</div>`,
    project: `<div class="tf-table__cell-title">${escapeHtml(projectName || T('runs.no_project'))}</div>`
      + `<div class="tf-table__cell-sub">${escapeHtml(runSourceLine(run))}</div>`,
    target: '<div class="tf-table__cell-row">'
      + `<tf-chip variant="outline" mono status="${TIER_TONE[tier] || 'neutral'}"`
      + ` label="${escapeAttr(tier ? T(`runs.tier_${tier.toLowerCase()}`) : run.target)}"></tf-chip>`
      + `<span class="tf-table__cell-sub">${escapeHtml(runNodeName(run, nodes))}</span>`
      + '</div>',
    size: qubits ? T('runs.size_value', { q: qubits, g: gates }) : '—',
    shots: shots ? String(shots) : '—',
    time: duration === null ? '—' : fmtMs(duration),
    status: `<tf-chip status="${runStatusTone(run.status)}" label="${escapeAttr(runStatusLabel(run))}"></tf-chip>`
      + (run.pinnedAt ? `<tf-chip status="accent" label="${escapeAttr(T('runs.pinned'))}"></tf-chip>` : ''),
  };
}

/// The footer summary of Q08, as three counted phrases.
export function runFooter(all, shown) {
  return [
    T('runs.footer', { n: all.length }),
    T('runs.footer_shown', { n: shown.length }),
    T('runs.footer_pinned', { n: shown.filter((r) => r.pinnedAt).length }),
  ];
}

function tableHtml() {
  return `
    <div class="table-scroll">
      <tf-table id="tq-run-table">
        <tf-column key="run" label="${escapeAttr(T('runs.col_run'))}" renderer="html" nowrap></tf-column>
        <tf-column key="project" label="${escapeAttr(T('runs.col_project'))}" renderer="html" fill></tf-column>
        <tf-column key="target" label="${escapeAttr(T('runs.col_target'))}" renderer="html" nowrap></tf-column>
        <tf-column key="size" label="${escapeAttr(T('runs.col_size'))}" renderer="text" nowrap></tf-column>
        <tf-column key="shots" label="${escapeAttr(T('runs.col_shots'))}" renderer="text" nowrap></tf-column>
        <tf-column key="time" label="${escapeAttr(T('runs.col_time'))}" renderer="text" nowrap></tf-column>
        <tf-column key="status" label="${escapeAttr(T('runs.col_status'))}" renderer="html" nowrap></tf-column>
      </tf-table>
    </div>`;
}

/// Draws the Runy tab. `projectId` limits it to one project — the project's own
/// tab — and is what the reload request carries too, so the narrowing is the
/// server's and not a slice of somebody else's list.
export function drawRuns(screen, host, { projectId = null } = {}) {
  const filters = screen.runFilters;
  const supervisor = has(screen.lab?.myPermissions, 'quant.instruct');
  const projectNames = new Map(screen.projects.map((p) => [p.projectId, p.name]));
  const nodes = screen.lab?.nodes || [];
  const all = screen.runs;
  const rows = filterRuns(all, filters, projectNames);

  // The panel below is about to be replaced, and with it the open detail — a
  // live run's detail owns a subscription, and a filter change that hides its
  // row would otherwise leave that stream running against a detached node.
  screen.disposeRunView();

  host.innerHTML = `
    <div class="tf-toolbar">
      <tf-searchbox id="tq-run-search" placeholder="${escapeAttr(T('runs.search_placeholder'))}" debounce="200" value="${escapeAttr(filters.query)}"></tf-searchbox>
      <tf-select id="tq-run-tier" value="${escapeAttr(filters.tier)}">
        <option value="all">${escapeHtml(T('runs.filter_tier_all'))}</option>
        <option value="T0">${escapeHtml(T('runs.tier_t0'))}</option>
        <option value="T1">${escapeHtml(T('runs.tier_t1'))}</option>
      </tf-select>
      <tf-select id="tq-run-status" value="${escapeAttr(filters.status)}">
        <option value="all">${escapeHtml(T('runs.filter_status_all'))}</option>
        ${RUN_STATUSES.map((s) => `<option value="${s}">${escapeHtml(T(`runs.status_${s}`))}</option>`).join('')}
      </tf-select>
      ${supervisor ? `<tf-select id="tq-run-user" value="${escapeAttr(filters.user)}">
        <option value="all">${escapeHtml(T('runs.filter_user_all'))}</option>
        ${runUsers(all).map((u) => `<option value="${escapeAttr(u.userId)}">${escapeHtml(u.name)}</option>`).join('')}
      </tf-select>` : ''}
      <span class="tf-toolbar-spacer"></span>
      <tf-button variant="ghost" size="sm" icon="refresh" data-act="reload">${escapeHtml(T('runs.reload'))}</tf-button>
    </div>
    ${screen.runsError ? `<tf-alert tone="danger" title="${escapeAttr(T('runs.load_failed'))}" message="${escapeAttr(screen.runsError)}"></tf-alert>` : ''}
    ${rows.length
      ? tableHtml()
      : `<tf-empty-state icon="clock" title="${escapeAttr(T(all.length ? 'runs.empty_filtered' : 'runs.empty'))}"
          message="${escapeAttr(T(all.length ? 'runs.empty_filtered_sub' : 'runs.empty_sub'))}"></tf-empty-state>`}
    <div class="tq-table-footer">
      ${runFooter(all, rows).map((part) => `<span>${escapeHtml(part)}</span>`).join('')}
      <span class="tf-toolbar-spacer"></span>
      <span>${escapeHtml(T('runs.footer_hint'))}</span>
    </div>
    <div id="tq-run-detail"></div>`;

  const table = host.querySelector('#tq-run-table');
  if (table) {
    const now = Date.now();
    table.rows = rows.map((run) => runTableRow(run, {
      projectName: projectNames.get(run.projectId),
      nodes,
      now,
    }));
    table.rowActions = (row) => rowActions(screen, host, all.find((r) => r.runId === row._run), projectId);
    table.addEventListener('row-click', (e) => selectRun(screen, host, e.detail.row._run, projectId));
  }

  const redraw = () => drawRuns(screen, host, { projectId });
  host.querySelector('#tq-run-search').addEventListener('search', (e) => {
    filters.query = String(e.detail?.value ?? '');
    redraw();
  });
  host.querySelector('#tq-run-tier').addEventListener('change', (e) => {
    filters.tier = e.detail?.value || 'all';
    redraw();
  });
  host.querySelector('#tq-run-status').addEventListener('change', (e) => {
    filters.status = e.detail?.value || 'all';
    redraw();
  });
  host.querySelector('#tq-run-user')?.addEventListener('change', (e) => {
    filters.user = e.detail?.value || 'all';
    redraw();
  });
  host.querySelector('[data-act="reload"]').addEventListener('click', () => screen.reloadRuns({ projectId }));

  // A run named by the route (or just opened from the Studio) stays open across
  // a redraw — a filter change must not close what the user is reading.
  if (screen.runId && rows.some((r) => r.runId === screen.runId)) {
    drawRunDetail(screen, host.querySelector('#tq-run-detail'), screen.runId, { projectId });
  }
}

/// The two acts a row carries. They go through `tf-table.rowActions` because
/// the cell lives in the component's shadow root, where a listener on the host
/// would only ever see the table element — and for the same reason the wrapper
/// carries the controls.css class rather than one of this screen's own.
function rowActions(screen, host, run, projectId) {
  if (!run || !canControlRun(run, screen.userId)) return null;
  const wrap = document.createElement('span');
  wrap.className = 'tf-table__row-actions';
  wrap.appendChild(actionButton('star', T(run.pinnedAt ? 'runs.unpin' : 'runs.pin'), () => {
    setRunPinned(screen, run, !run.pinnedAt, { projectId });
  }));
  if (runIsLive(run)) {
    wrap.appendChild(actionButton('x', T('runs.cancel'), () => cancelRun(screen, run, { projectId })));
  }
  return wrap;
}

function actionButton(icon, title, onClick) {
  const button = document.createElement('tf-button');
  button.setAttribute('variant', 'ghost');
  button.setAttribute('size', 'sm');
  button.setAttribute('icon', icon);
  button.setAttribute('title', title);
  button.addEventListener('click', onClick);
  return button;
}

function selectRun(screen, host, runId, projectId) {
  screen.selectRun(runId === screen.runId ? null : runId);
  const detail = host.querySelector('#tq-run-detail');
  if (!detail) return;
  if (!screen.runId) { detail.innerHTML = ''; return; }
  drawRunDetail(screen, detail, screen.runId, { projectId });
}
