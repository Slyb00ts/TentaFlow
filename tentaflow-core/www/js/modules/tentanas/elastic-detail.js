// =============================================================================
// Plik: modules/tentanas/elastic-detail.js
// Opis: Karta Elastic Array ze stanem, montowaniami i historią operacji SnapRAID.
// Przykład: drawElasticDetail(screen, body) korzysta z nazwy screen.array.
// =============================================================================

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import { T, sprite, fmtOptionalBytes, fmtDate, fmtDuration, fmtSchedule, errMessage, healthClass, POLL_POOLS_MS, ADMIN_TIMEOUT_MS } from '/js/modules/tentanas/format.js';
import { openScheduleEditor, scheduleFieldsHtml, wireScheduleFields, readScheduleFields } from '/js/modules/tentanas/schedule-editor.js';
import '/js/components/tf-button.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-alert.js';
import '/js/components/tf-stat-card.js';
import '/js/components/tf-breadcrumb.js';
import '/js/components/tf-window.js';
import '/js/components/tf-select.js';
import '/js/components/tf-toggle.js';

const knownBytes = (value) => value != null && Number.isFinite(Number(value)) && Number(value) >= 0;
const row = (label, value) => `<div class="sr"><span class="k">${escapeHtml(label)}</span><span class="v">${escapeHtml(value)}</span></div>`;
const triState = (value) => value === true ? T('elastic.yes') : value === false ? T('elastic.no') : T('elastic.unknown');

// One entry per mutation the detail can send, so a new action cannot reach the
// transport without also naming the sentence the admin sees for it.
const ACTION_REQUEST = { restore: 'tentaNasElasticArrayRestoreRequest', sync: 'tentaNasElasticArraySyncRequest', scrub: 'tentaNasElasticArrayScrubRequest', mover: 'tentaNasElasticArrayMoverRequest' };
const ACTION_TITLE = { restore: 'elastic.restore', sync: 'elastic.sync_now', scrub: 'elastic.scrub_now', mover: 'elastic.mover_run_now' };
const ACTION_ACCEPTED = { restore: 'elastic.job_running', sync: 'elastic.maintenance_accepted', scrub: 'elastic.maintenance_accepted', mover: 'elastic.mover_accepted' };
const ACTION_APPROVAL = { restore: 'elastic.approval', sync: 'elastic.maintenance_approval', scrub: 'elastic.maintenance_approval', mover: 'elastic.mover_approval' };

export function elasticState(array) {
  const labels = { active: T('elastic.active'), pending: T('elastic.pending'), creating: T('elastic.creating'), needs_attention: T('elastic.error'), error: T('elastic.error'), disabled: T('elastic.disabled'), unknown: T('elastic.unknown') };
  return { label: labels[array.state] || labels.unknown, tone: array.state === 'active' ? 'ok' : ['error', 'needs_attention'].includes(array.state) ? 'err' : 'warn' };
}

function protectionLabel(array) {
  if (!(array.parityDisks || []).length) return T('elastic.no_parity');
  const labels = { protected: T('elastic.protected'), window_open: T('elastic.window_open'), unprotected: T('elastic.unprotected'), unknown: T('elastic.unknown') };
  return labels[array.protection?.status] || labels.unknown;
}

export function elasticCapacity(array) {
  const parity = array.parityDisks || [];
  const parityBytes = parity.every((d) => knownBytes(d.sizeBytes)) ? parity.reduce((n, d) => n + Number(d.sizeBytes), 0) : null;
  const measured = knownBytes(array.usableBytes) && knownBytes(array.usedBytes) && Number(array.usedBytes) <= Number(array.usableBytes);
  const free = measured ? Number(array.usableBytes) - Number(array.usedBytes) : null;
  const raw = measured && parityBytes != null ? Number(array.usableBytes) + parityBytes : null;
  return { measured, free, parityBytes, raw };
}

export function elasticCardHtml(array) {
  const state = elasticState(array);
  const c = elasticCapacity(array);
  const widths = c.raw > 0 ? [Number(array.usedBytes), c.free, c.parityBytes].map((n) => n / c.raw * 100) : null;
  return `<div class="pool-card nas-elastic-card" data-array="${escapeAttr(array.name)}">
    <div class="pc-head">
      <div class="pc-ico">${sprite('cylinder')}</div>
      <div class="pc-meta"><span class="pc-name">${escapeHtml(array.name)}</span>
        <tf-chip status="${state.tone}" dot label="${escapeAttr(state.label)}"></tf-chip>
        <tf-chip status="accent" label="Elastic Array"></tf-chip>
        <div class="pc-desc">${escapeHtml(T('elastic.topology', { data: (array.dataDisks || []).length, parity: (array.parityDisks || []).length, fs: array.filesystem.toUpperCase() }))}</div>
      </div>
      <div class="pc-actions"><tf-button variant="secondary" size="sm" icon="external-link" data-act="array-details">${escapeHtml(T('elastic.details'))}</tf-button></div>
    </div>
    <div class="pc-body">
      <div><div class="pc-cap"><span>${escapeHtml(T('elastic.capacity'))}</span><span class="v">${escapeHtml(fmtOptionalBytes(array.usedBytes))} / ${escapeHtml(fmtOptionalBytes(array.usableBytes))}</span></div>
        <div class="split-bar ${widths ? '' : 'nas-unmeasured'}" aria-label="${escapeAttr(widths ? T('elastic.capacity') : T('elastic.unmeasured'))}">${widths ? widths.map((w, i) => `<span class="${['data', 'free', 'parity'][i]}" style="width:${w}%"></span>`).join('') : ''}</div>
        <div class="legend-rows mt-sm">
          <div class="lr"><span class="sw data"></span>${escapeHtml(T('elastic.used'))}<span class="v">${escapeHtml(fmtOptionalBytes(array.usedBytes))}</span></div>
          <div class="lr"><span class="sw free"></span>${escapeHtml(T('elastic.free'))}<span class="v">${escapeHtml(fmtOptionalBytes(c.free))}</span></div>
          <div class="lr"><span class="sw parity"></span>${escapeHtml(T('elastic.parity'))}<span class="v">${escapeHtml(fmtOptionalBytes(c.parityBytes))}</span></div>
        </div>${!widths ? `<div class="hint">${escapeHtml(T('elastic.unmeasured'))}</div>` : ''}
      </div>
      <div class="stat-rows">${row(T('elastic.mountpoint'), array.unionPath || '—')}${row(T('elastic.last_sync'), fmtDate(array.protection?.protectedAsOf))}${row(T('elastic.protection'), protectionLabel(array))}</div>
    </div>
    ${array.stateDetail ? `<div class="pc-reason">${escapeHtml(array.stateDetail)}</div>` : ''}
  </div>`;
}

function diskHtml(disk, filesystem) {
  return `<div class="disk-cell" data-disk="${escapeAttr(disk.diskId)}">
    <span class="health-dot ${healthClass(disk.health)}"></span>
    <div class="dc-main"><div class="dc-name"><span class="mono">${escapeHtml(disk.name)}</span></div>
      <div class="dc-sub">${escapeHtml(fmtOptionalBytes(disk.usedBytes))} / ${escapeHtml(fmtOptionalBytes(disk.sizeBytes))} · ${escapeHtml(filesystem.toUpperCase())}</div>
      <div class="dc-sub">${escapeHtml(disk.device || disk.diskId)}</div>
      <div class="dc-sub">${escapeHtml(T('elastic.mounted'))}: ${escapeHtml(triState(disk.mounted))} · ${escapeHtml(T('elastic.present'))}: ${escapeHtml(triState(disk.devicePresent))}</div>
      <div class="dc-sub mono">${escapeHtml(disk.mountpoint)}</div>
    </div><tf-button variant="ghost" size="sm" icon="external-link" data-act="disk" title="${escapeAttr(T('elastic.disk_details', { name: disk.name }))}"></tf-button>
  </div>`;
}

function snapraidHistoryHtml(history, expanded) {
  const outcomes = { running: 'run_running', ok: 'run_ok', failed: 'run_failed', needs_attention: 'error', refused: 'run_refused' };
  const refusals = { no_parity: 'no_parity', precondition_failed: 'refused_precondition', unsynced_changes: 'refused_dirty', empty_parity: 'refused_empty' };
  return `<div class="nas-snapraid-history mt-md"><div class="title">${escapeHtml(T('elastic.history'))}</div>${history.length ? `<ol>${history.map((run) => {
    const label = run.kind === 'sync' ? 'Sync' : run.kind === 'scrub' ? 'Scrub' : run.kind;
    const result = T(`elastic.${outcomes[run.outcome] || 'unknown'}`);
    const key = JSON.stringify([run.operationId, run.jobId, run.startedAt, run.kind]);
    const detail = run.outcome === 'refused' && refusals[run.detail] ? T(`elastic.${refusals[run.detail]}`) : run.detail;
    return `<li><details data-run="${escapeAttr(key)}" ${expanded.has(key) ? 'open' : ''}><summary><strong>${escapeHtml(label)}</strong><span class="hint">${escapeHtml(fmtDate(run.finishedAt || run.startedAt))}</span><tf-chip status="${run.outcome === 'ok' ? 'ok' : run.outcome === 'failed' || run.outcome === 'needs_attention' ? 'err' : 'warn'}" label="${escapeAttr(result)}"></tf-chip></summary>
      <div class="stat-rows">${row(T('elastic.run_started'), fmtDate(run.startedAt))}${row(T('elastic.run_finished'), fmtDate(run.finishedAt))}${row(T('elastic.run_blocks'), `${run.checkedBlocks ?? '—'} / ${run.totalBlocks ?? '—'}`)}${row(T('elastic.run_errors'), `${run.errorsFile ?? '—'} / ${run.errorsIo ?? '—'} / ${run.errorsData ?? '—'}`)}${row(T('elastic.run_exit'), run.exitCode ?? '—')}</div>
      ${detail ? `<div class="hint">${escapeHtml(detail)}</div>` : ''}</details>${run.jobId ? `<tf-button variant="ghost" size="sm" data-act="history-job" data-job="${escapeAttr(run.jobId)}">${escapeHtml(T('elastic.history_job'))}</tf-button>` : ''}</li>`;
  }).join('')}</ol>` : `<div class="hint mt-sm">${escapeHtml(T('elastic.history_empty'))}</div>`}</div>`;
}

const moverMoved = (run) => !run ? '—' : `${fmtOptionalBytes(run.movedBytes)} · ${Number(run.movedFiles) || 0}`;

// `countsKnown: false` means the walk never finished: such a run knows what it
// MOVED and NOT what it left behind. A `0` here would say "nothing was skipped",
// which is the one thing that run cannot say — so it reads "nie zmierzono".
const moverSkipped = (run) => !run ? '—'
  : run.countsKnown ? `${fmtOptionalBytes(run.skippedBytes)} · ${Number(run.skippedFiles) || 0}`
    : T('elastic.mover_counts_unmeasured');

const moverSync = (run) => !run.coupledSync ? T('elastic.mover_sync_none')
  : run.coupledSync.outcome === 'ok' ? T('elastic.mover_sync_ok') : T('elastic.mover_sync_failed');

const moverLastRun = (run) => !run ? '—'
  : `${fmtDate(run.finishedAt || run.startedAt)} · ${fmtOptionalBytes(run.movedBytes)} → ${moverSync(run)}`;

// The age rule is a duration and the cache rule a FILL level, while the setting
// is the minimum FREE percentage — so the sentence n11 shows is its complement.
// Absent settings render as `—`; neither half is invented.
// With nothing configured these numbers are the built-in defaults a run falls
// back on — real, but nobody's decision. They are shown (a manual run WILL
// apply them) and labelled as defaults, rather than presented as settings.
const moverRulesValue = (m) => {
  const age = Number(m.minAgeSecs);
  const free = Number(m.cacheMinFreePct);
  if (m.minAgeSecs == null || m.cacheMinFreePct == null || !Number.isFinite(age) || !Number.isFinite(free)) return '—';
  const params = { age: fmtDuration(age), pct: 100 - free };
  return m.configured ? T('elastic.mover_rules_value', params) : T('elastic.mover_rules_default', params);
};

// The CADENCE is its own fact, separate from `configured`: a schedule row and
// a rules row are saved independently, so the panel asks the schedule whether
// there is a schedule instead of asking the rules.
const moverScheduleValue = (m) => (m.schedule ? fmtSchedule(m.schedule) : T('elastic.mover_schedule_none'));

// A cadence and its switch are two facts. A schedule that is saved but off
// says so, because rendering it like a live one would promise a safety net
// that is not running.
const cadenceValue = (schedule, enabled) => (!schedule ? T('elastic.mover_schedule_none')
  : enabled ? fmtSchedule(schedule) : `${fmtSchedule(schedule)} · ${T('schedule.off')}`);

// The pill is the control: clicking the cadence is how n11 reaches the dialog
// that sets it, which is where an admin looks for it first.
const schedulePill = (label, value, act, admin) => (admin
  ? `<div class="sr"><span class="k">${escapeHtml(label)}</span><span class="v"><button type="button" class="sched-pill" data-act="${escapeAttr(act)}" title="${escapeAttr(T('elastic.schedule_edit'))}">${sprite('clock')} ${escapeHtml(value)}</button></span></div>`
  : row(label, value));

function moverPanelHtml(array, disabled, reason, admin) {
  const m = array.mover || {};
  const last = m.lastRun || null;
  const history = m.history || [];
  return `<div class="section-card nas-mover"><div class="section-card-head"><div class="title">${sprite('transform')} ${escapeHtml(T('elastic.mover'))}</div><div class="actions">
    ${admin ? `<tf-button variant="ghost" size="sm" icon="edit" data-act="mover-schedule">${escapeHtml(T('elastic.schedule_edit'))}</tf-button>` : ''}
    <tf-button variant="primary" size="sm" icon="play" data-act="mover" ${disabled ? 'disabled' : ''}>${escapeHtml(T('elastic.mover_run_now'))}</tf-button></div></div>
    ${reason ? `<div class="hint mb-sm">${escapeHtml(reason)}</div>` : ''}
    ${m.enabled === false ? `<div class="hint mb-sm">${escapeHtml(T('elastic.mover_disabled'))}</div>` : ''}
    <div class="stat-rows">${schedulePill(T('elastic.mover_schedule'), moverScheduleValue(m), 'mover-schedule', admin)}${row(T('elastic.mover_rules'), moverRulesValue(m))}${row(T('elastic.mover_open_files'), T('elastic.mover_open_files_skipped'))}${row(T('elastic.mover_last_run'), moverLastRun(last))}${row(T('elastic.mover_moved'), moverMoved(last))}${row(T('elastic.mover_skipped'), moverSkipped(last))}</div>
    <div class="mover-hist">${escapeHtml(T('elastic.mover_history'))}: ${history.length ? history.map((run) => `<span>${escapeHtml(fmtDate(run.finishedAt || run.startedAt))} · ${escapeHtml(fmtOptionalBytes(run.movedBytes))}</span>`).join('') : `<span>${escapeHtml(T('elastic.mover_history_empty'))}</span>`}</div>
    <div class="explain-box mt-md">${escapeHtml(m.coupledSync === false ? T('elastic.mover_coupled_off') : T('elastic.mover_coupled_warning'))}</div>
  </div>`;
}

// The mockup's four choices (n15). 0 is "no age limit" and is a real setting,
// not an absent one — it means every file is old enough to move.
const MOVER_AGE_OPTIONS = [0, 1800, 7200, 86400];
const MOVER_FREE_OPTIONS = [10, 20, 30];
// The mover is the one cadence that runs sub-daily: it is cheap and its whole
// job is to keep the cache drained between the slower parity runs.
const MOVER_EVERY = ['15m', '30m', '1h', '6h', 'daily'];
const MOVER_SCHEDULE_DEFAULT = { every: '1h', hour: 0, minute: 0, weekday: 0, day: 1 };

/**
 * n15's mover dialog: the cadence AND the rules in one window, because the
 * mockup is one form. Sends both in one request, so an admin who changes the
 * age and the cadence together cannot end up with half of it saved.
 */
export function openMoverScheduleEditor(screen, array, onDone) {
  const m = array.mover || {};
  const schedule = m.schedule || MOVER_SCHEDULE_DEFAULT;
  const win = document.createElement('tf-window');
  win.className = 'nas-modal nas-mover-schedule';
  win.setAttribute('title', T('elastic.mover_schedule_title', { name: array.name }));
  win.setAttribute('icon', 'transform');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '560');
  win.setAttribute('min-width', '460');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('schedule.enabled'))}</span><span class="tc-sub">${escapeHtml(T('schedule.enabled_sub'))}</span></div>
        <tf-toggle id="nas-mover-enabled" ${m.enabled ? 'checked' : ''}></tf-toggle>
      </div>
      ${scheduleFieldsHtml('nas-mover', schedule, { allowed: MOVER_EVERY })}
      <div class="form-grid-2">
        <div><tf-select id="nas-mover-age" label="${escapeAttr(T('elastic.mover_min_age'))}"></tf-select><div class="hint">${escapeHtml(T('elastic.mover_min_age_hint'))}</div></div>
        <div><tf-select id="nas-mover-free" label="${escapeAttr(T('elastic.mover_min_free'))}"></tf-select><div class="hint">${escapeHtml(T('elastic.mover_min_free_hint'))}</div></div>
      </div>
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('elastic.mover_coupled_label'))}</span><span class="tc-sub">${escapeHtml(T('elastic.mover_coupled_sub'))}</span></div>
        <tf-toggle id="nas-mover-coupled" ${m.coupledSync === false ? '' : 'checked'}></tf-toggle>
      </div>
      <div class="wizard-warning info">${sprite('info')}<div>${escapeHtml(T('elastic.mover_dialog_note'))}</div></div>
      <div class="num-err" id="nas-mover-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(T('schedule.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  wireScheduleFields(win, 'nas-mover', schedule);
  // An unrecognised stored value falls back to the default rather than being
  // added as a silent fifth option nobody offered.
  const age = Number(m.minAgeSecs);
  win.querySelector('#nas-mover-age').setOptions(
    MOVER_AGE_OPTIONS.map((v) => ({ value: String(v), label: v === 0 ? T('elastic.mover_age_none') : fmtDuration(v) })),
    String(MOVER_AGE_OPTIONS.includes(age) ? age : 7200),
  );
  const free = Number(m.cacheMinFreePct);
  win.querySelector('#nas-mover-free').setOptions(
    MOVER_FREE_OPTIONS.map((v) => ({ value: String(v), label: `${v}%` })),
    String(MOVER_FREE_OPTIONS.includes(free) ? free : 20),
  );
  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    busy = true;
    const btn = win.querySelector('[data-action="confirm"]');
    btn.setAttribute('disabled', '');
    try {
      await screen.nas('tentaNasElasticMoverScheduleSetRequest', {
        name: array.name,
        enabled: Boolean(win.querySelector('#nas-mover-enabled').checked),
        schedule: readScheduleFields(win, 'nas-mover'),
        minAgeSecs: Number(win.querySelector('#nas-mover-age').value),
        cacheMinFreePct: Number(win.querySelector('#nas-mover-free').value),
        coupledSync: Boolean(win.querySelector('#nas-mover-coupled').checked),
      });
      toast(T('schedule.saved'), 'success');
      win.close(true);
      if (onDone) onDone();
    } catch (err) {
      busy = false;
      btn.removeAttribute('disabled');
      const errEl = win.querySelector('#nas-mover-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}

const ELASTIC_CADENCE_DEFAULT = {
  sync: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 },
  scrub: { every: 'weekly', hour: 4, minute: 0, weekday: 0, day: 1 },
};

/**
 * The two SnapRAID cadences. Plain schedules with no extra settings, so they
 * reuse the shared editor the scrub and TRIM of a pool use.
 *
 * Takes the cadence and its switch rather than a whole array, because n15 has
 * only the schedule row and n11 has only `snapraid` — neither has the other's
 * shape.
 */
export function openElasticScheduleEditor(screen, { name, kind, schedule, enabled }, onDone) {
  openScheduleEditor({
    title: T(`elastic.${kind}_schedule_title`, { name }),
    icon: kind === 'sync' ? 'refresh' : 'search',
    schedule: schedule || ELASTIC_CADENCE_DEFAULT[kind],
    enabled: Boolean(enabled),
    allowed: kind === 'sync' ? ['daily', 'weekly'] : ['weekly', 'monthly'],
    note: T(`elastic.${kind}_schedule_note`),
    onSave: async ({ enabled: on, schedule: next }) => {
      const request = kind === 'sync' ? 'tentaNasElasticSyncScheduleSetRequest' : 'tentaNasElasticScrubScheduleSetRequest';
      await screen.nas(request, { name, enabled: on, schedule: next });
      toast(T('schedule.saved'), 'success');
      if (onDone) onDone();
    },
  });
}

export async function drawElasticDetail(screen, body) {
  const name = screen.array;
  const sourceNodeId = screen.currentNode()?.nodeId;
  const view = document.createElement('div');
  view.className = 'stack nas-elastic-detail';
  body.replaceChildren(view);
  const isCurrent = () => !screen.disposed && view.isConnected && screen.currentNode()?.nodeId === sourceNodeId && screen.array === name;
  let epoch = 0;
  let array = null;
  let busy = false;
  let submitted = false;
  let message = '';
  let submittedJobId = null;
  const expanded = new Set();
  const canRestore = () => array && array.enabled && !['active', 'creating'].includes(array.state)
    && !(array.snapraid?.history || []).some((run) => ['sync', 'scrub'].includes(run.kind) && ['running', 'failed', 'needs_attention'].includes(run.outcome));
  const maintenanceReason = () => !screen.isAdmin ? T('elevation.admin_only')
    : !(array?.parityDisks || []).length ? T('elastic.no_parity')
      : !array.enabled || array.state !== 'active' ? T('elastic.maintenance_not_ready')
        : (array.snapraid?.history || []).some((run) => run.outcome === 'running') ? T('elastic.run_running') : '';
  // The mover needs no parity — the helper skips the coupled sync on an array
  // without one — but it does need something to move, so a cacheless array is
  // refused outright rather than offered a run that would walk nothing.
  // `unresolvedOperation` is its own fact and NOT derivable from the SnapRAID
  // history: the blocking row may be a mover's, and it outlives the array
  // returning to 'active' after a Restore. Without it the button would be
  // offered for a request the node can only refuse.
  const moverReason = () => !screen.isAdmin ? T('elevation.admin_only')
    : !(array?.cacheDisks || []).length ? T('elastic.mover_no_cache')
      : !array.enabled || array.state !== 'active' ? T('elastic.maintenance_not_ready')
        : array.unresolvedOperation ? T('elastic.mover_unresolved')
          : (array.snapraid?.history || []).some((run) => run.outcome === 'running') ? T('elastic.run_running')
            : array.mover?.lastRun?.outcome === 'running' ? T('elastic.mover_running') : '';

  const draw = (error = '') => {
    if (!isCurrent()) return;
    const status = array && elasticState(array);
    const maintenanceDisabled = busy || submitted || Boolean(maintenanceReason());
    const moverDisabled = busy || submitted || Boolean(moverReason());
    view.innerHTML = `<tf-breadcrumb class="nas-crumbs"><tf-breadcrumb-item href="#">${escapeHtml(T('tabs.pools'))}</tf-breadcrumb-item><tf-breadcrumb-item current>${escapeHtml(name)}</tf-breadcrumb-item></tf-breadcrumb><div class="section-card-head nas-elastic-heading"><div class="title">${sprite('layers')} <span class="mono">${escapeHtml(name)}</span> <tf-chip status="accent" label="Elastic Array"></tf-chip></div><div class="actions">
      <tf-button variant="ghost" data-act="back">${escapeHtml(T('elastic.back'))}</tf-button><tf-button variant="secondary" icon="refresh" data-act="refresh">${escapeHtml(T('elastic.refresh'))}</tf-button></div></div>
      ${error ? `<tf-alert tone="danger" title="${escapeAttr(T('load_failed'))}" message="${escapeAttr(error)}"></tf-alert>` : ''}
      ${array ? `<div class="kpi">
        <tf-stat-card icon="cylinder" label="${escapeAttr(T('elastic.capacity'))}" value="${escapeAttr(fmtOptionalBytes(array.usedBytes))}" suffix="${escapeAttr('/ ' + fmtOptionalBytes(array.usableBytes))}"></tf-stat-card>
        <tf-stat-card icon="shield" label="${escapeAttr(T('elastic.protection'))}" value="${escapeAttr(protectionLabel(array))}" delta="${escapeAttr(T('elastic.last_sync') + ': ' + fmtDate(array.protection?.protectedAsOf))}"></tf-stat-card>
        <tf-stat-card icon="database" label="${escapeAttr(T('elastic.parity'))}" value="${(array.parityDisks || []).length}" delta="${escapeAttr(T('elastic.tolerance', { n: array.protection?.faultTolerance ?? '—' }))}"></tf-stat-card>
        <tf-stat-card icon="database" label="${escapeAttr(T('elastic.cache'))}" value="${escapeAttr(fmtOptionalBytes(array.cacheUsedBytes))}" suffix="${escapeAttr('/ ' + fmtOptionalBytes(array.cacheSizeBytes))}"></tf-stat-card>
      </div>
      <div class="section-card"><div class="section-card-head"><div class="title">${sprite('cylinder')} ${escapeHtml(T('elastic.disks'))}</div><span class="hint">${escapeHtml(T('elastic.independent_fs'))}</span></div>
        <div class="vdev-group"><div class="vg-head"><span class="vg-type">${escapeHtml(T('elastic.data'))} · MERGERFS</span><span class="mono">${escapeHtml(array.unionPath)}</span><span class="hint">${escapeHtml(T('elastic.policy'))}: ${escapeHtml(array.createPolicy)}</span></div>
          <div class="disk-cells">${(array.dataDisks || []).map((d) => diskHtml(d, d.filesystem || array.filesystem)).join('')}</div></div>
        <div class="vdev-group"><div class="vg-head"><span class="vg-type">PARITY · SNAPRAID</span></div><div class="disk-cells">${(array.parityDisks || []).map((d) => diskHtml(d, array.filesystem)).join('')}</div>${!(array.parityDisks || []).length ? `<div class="hint">${escapeHtml(T('elastic.no_parity'))}</div>` : ''}</div>
        <div class="vdev-group"><div class="vg-head"><span class="vg-type">${escapeHtml(T('elastic.cache'))}</span><span class="hint">${escapeHtml(T('elastic.cache_no_protection'))}</span></div><div class="disk-cells">${(array.cacheDisks || []).map((d) => diskHtml(d, array.filesystem)).join('')}</div>${!(array.cacheDisks || []).length ? `<div class="hint">${escapeHtml(T('elastic.cache_none'))}</div>` : ''}</div>
      </div>
      <div class="grid-2"><div class="section-card"><div class="section-card-head"><div class="title">${sprite('shield')} ${escapeHtml(T('elastic.state'))}</div><tf-chip status="${status.tone}" dot label="${escapeAttr(status.label)}"></tf-chip></div>
        <div class="stat-rows">${row(T('elastic.mountpoint'), array.unionPath)}${row(T('elastic.state'), array.stateDetail || status.label)}${row(T('elastic.unprotected_bytes'), fmtOptionalBytes(array.protection?.movedUnsyncedBytes))}${row(T('elastic.updated'), fmtDate(array.updatedAt))}</div>
        <div class="explain-box mt-md">${escapeHtml(T('elastic.restore_hint'))}</div>
        ${screen.isAdmin && canRestore() ? `<tf-button variant="secondary" class="mt-md" data-act="restore" ${busy || submitted ? 'disabled' : ''}>${escapeHtml(T('elastic.restore'))}</tf-button>` : ''}
        ${!screen.isAdmin ? `<div class="hint mt-sm">${escapeHtml(T('elevation.admin_only'))}</div>` : ''}
        ${message ? `<div class="explain-box mt-md" role="status">${escapeHtml(message)}</div><tf-button variant="ghost" data-act="jobs">${escapeHtml(T('elastic.jobs'))}</tf-button>` : ''}
      </div><div class="section-card nas-snapraid"><div class="section-card-head"><div class="title">${sprite('shield')} SnapRAID</div><div class="actions">
        <tf-button variant="secondary" size="sm" icon="refresh" data-act="sync" ${maintenanceDisabled ? 'disabled' : ''}>${escapeHtml(T('elastic.sync_now'))}</tf-button><tf-button variant="ghost" size="sm" icon="search" data-act="scrub" ${maintenanceDisabled ? 'disabled' : ''}>${escapeHtml(T('elastic.scrub_now'))}</tf-button></div></div>
        ${maintenanceReason() ? `<div class="hint mb-sm">${escapeHtml(maintenanceReason())}</div>` : ''}<div class="stat-rows">
        ${row(T('elastic.last_sync'), fmtDate(array.protection?.protectedAsOf))}${row(T('elastic.last_scrub'), fmtDate(array.snapraid?.lastScrub?.finishedAt))}${schedulePill(T('elastic.sync_schedule'), cadenceValue(array.snapraid?.syncSchedule, array.snapraid?.syncScheduleEnabled), 'sync-schedule', screen.isAdmin)}${schedulePill(T('elastic.scrub_schedule'), cadenceValue(array.snapraid?.scrubSchedule, array.snapraid?.scrubScheduleEnabled), 'scrub-schedule', screen.isAdmin)}${row(T('elastic.parity_errors'), array.snapraid?.parityErrors ?? '—')}${row(T('elastic.config'), array.snapraid?.configPath || '—')}
      </div><div class="explain-box mt-md">${escapeHtml(T('elastic.snapshot_only'))}</div><div class="hint mt-sm">${escapeHtml(T('elastic.maintenance_hint'))}</div>${snapraidHistoryHtml(array.snapraid?.history || [], expanded)}</div>${moverPanelHtml(array, moverDisabled, moverReason(), screen.isAdmin)}</div>` : error ? '' : `<div class="muted">${escapeHtml(I18n.t('common.loading'))}</div>`}`;
    view.querySelector('[data-act="back"]').addEventListener('click', () => { if (isCurrent()) screen.openArray(null); });
    view.querySelector('.nas-crumbs').addEventListener('click', (event) => {
      if (!event.target.closest('a')) return;
      event.preventDefault();
      if (isCurrent()) screen.openArray(null);
    });
    view.querySelector('[data-act="refresh"]').addEventListener('click', refresh);
    view.querySelector('[data-act="jobs"]')?.addEventListener('click', () => { if (isCurrent()) screen.switchTab('jobs'); });
    view.querySelectorAll('[data-act="disk"]').forEach((button) => {
      button.querySelector('button')?.setAttribute('aria-label', button.title);
      button.addEventListener('click', () => { if (isCurrent()) screen.openDisk(button.closest('[data-disk]').dataset.disk); });
    });
    for (const action of ['restore', 'sync', 'scrub', 'mover']) view.querySelector(`[data-act="${action}"]`)?.addEventListener('click', () => execute(action));
    // querySelectorAll, not querySelector: the header button AND the pill both
    // carry this action, and binding only the first match left the pill — the
    // control this panel documents as the way in — doing nothing at all.
    view.querySelectorAll('[data-act="mover-schedule"]').forEach((el) => el.addEventListener('click', () => {
      if (isCurrent() && array) openMoverScheduleEditor(screen, array, refresh);
    }));
    for (const kind of ['sync', 'scrub']) view.querySelectorAll(`[data-act="${kind}-schedule"]`).forEach((el) => el.addEventListener('click', () => {
      if (!isCurrent() || !array) return;
      const s = array.snapraid || {};
      openElasticScheduleEditor(screen, {
        name,
        kind,
        schedule: kind === 'sync' ? s.syncSchedule : s.scrubSchedule,
        enabled: kind === 'sync' ? s.syncScheduleEnabled : s.scrubScheduleEnabled,
      }, refresh);
    }));
    view.querySelectorAll('[data-act="history-job"]').forEach((button) => button.addEventListener('click', () => {
      if (isCurrent()) screen.openJobLog(button.dataset.job, finishJob);
    }));
    view.querySelectorAll('details[data-run]').forEach((details) => details.addEventListener('toggle', () => {
      if (!isCurrent() || !details.isConnected) return;
      if (details.open) expanded.add(details.dataset.run); else expanded.delete(details.dataset.run);
    }));
  };

  const finishJob = async (job) => {
    if (!isCurrent()) return;
    const terminal = ['succeeded', 'failed'].includes(job?.status);
    if (terminal) message = T('elastic.job_finished');
    const refreshed = await refresh();
    if (terminal && refreshed && isCurrent() && submittedJobId && job.jobId === submittedJobId) {
      submitted = false;
      submittedJobId = null;
      draw();
    }
  };

  const refresh = async () => {
    if (!isCurrent()) return;
    const request = ++epoch;
    try {
      const result = await screen.nas('tentaNasElasticArrayGetRequest', { name });
      if (!isCurrent() || request !== epoch) return;
      if (!result.array || result.array.name !== name || result.array.kind !== 'elastic-array') throw new Error(T('elastic.bad_response'));
      array = result.array;
      draw();
      return true;
    } catch (error) {
      if (isCurrent() && request === epoch) { array = null; draw(errMessage(error)); }
    }
  };

  const execute = async (action) => {
    if (!isCurrent() || !screen.isAdmin || busy || submitted) return;
    const blocked = action === 'restore' ? () => !canRestore() : action === 'mover' ? moverReason : maintenanceReason;
    const allowed = () => isCurrent() && screen.isAdmin && !blocked();
    if (!allowed()) return;
    busy = true;
    draw();
    let sent = false;
    try {
      const result = await screen.withSudo((sudoPassword) => {
        if (!allowed()) return null;
        sent = true;
        submitted = true;
        return screen.nas(ACTION_REQUEST[action], { name, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS });
      }, T(ACTION_TITLE[action]), allowed);
      if (!isCurrent()) return;
      if (result?.job?.jobId) {
        message = T(ACTION_ACCEPTED[action]);
        if (action !== 'restore') submittedJobId = result.job.jobId;
        screen.openJobLog(result.job.jobId, finishJob);
      } else if (result?.approval?.requestId) message = T(ACTION_APPROVAL[action]);
      else if (sent) message = T('elastic.request_unknown');
    } catch (error) {
      if (isCurrent()) message = sent ? T('elastic.request_unknown') : errMessage(error);
    } finally {
      busy = false;
      if (isCurrent()) { draw(); if (sent) await refresh(); }
    }
  };

  const poll = async () => { await refresh(); if (isCurrent()) screen.later(poll, POLL_POOLS_MS); };
  draw();
  await poll();
}
