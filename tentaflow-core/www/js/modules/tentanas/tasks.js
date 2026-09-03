// ===== File: modules/tentanas/tasks.js — the Tasks tab (n15): running jobs, the protection status strip, every schedule (scrub / snapshot / SMART) and the job history =====
//
// The tab polls two lists: jobs every POLL_JOBS_MS (so a running scrub or
// resilver moves visibly) and schedules every POLL_SCHEDULES_MS (the next-run
// column only changes when a schedule fires). Editing a schedule reuses the
// same field set as the pool detail, so an admin sees identical forms
// wherever a cadence is set.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import {
  T, sprite, POLL_JOBS_MS, fmtDate, fmtAgo, fmtIn, errMessage,
  jobTone, jobKindLabel, fmtSchedule,
} from '/js/modules/tentanas/format.js';
import { openScheduleEditor, scheduleFieldsHtml, wireScheduleFields, readScheduleFields, normalizeSchedule } from '/js/modules/tentanas/schedule-editor.js';
import { openSnapshotScheduleEditor } from '/js/modules/tentanas/snapshots.js';
import '/js/components/tf-table.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-window.js';
import '/js/components/tf-toggle.js';
import '/js/components/tf-stat-card.js';

const POLL_SCHEDULES_MS = 30000;
const JOBS_LIMIT = 100;
const SCRUB_ONLY = ['weekly', 'monthly'];

export async function drawTasks(screen, body) {
  const admin = screen.isAdmin;
  body.innerHTML = `
    <div class="stack">
      <div class="section-card">
        <div class="section-card-head"><div class="title">${sprite('play')} ${escapeHtml(T('jobs.running'))}</div><span class="hint" id="nas-jobs-hint"></span></div>
        <div id="nas-jobs-running"></div>
      </div>
      <div class="prot-grid" id="nas-prot"></div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('clock')} ${escapeHtml(T('schedules.title'))}</div>
          <div class="actions">${admin ? `<tf-button variant="secondary" size="sm" icon="shield" data-act="smart">${escapeHtml(T('schedules.smart_edit'))}</tf-button>` : ''}</div>
        </div>
        <tf-table id="nas-sched-table" empty-message="${escapeAttr(T('schedules.none'))}">
          <tf-column key="kind" label="${escapeAttr(T('schedules.col_kind'))}" renderer="html" nowrap></tf-column>
          <tf-column key="subject" label="${escapeAttr(T('schedules.col_subject'))}" renderer="html" fill></tf-column>
          <tf-column key="schedule" label="${escapeAttr(T('schedules.col_schedule'))}" renderer="html" nowrap></tf-column>
          <tf-column key="enabled" label="${escapeAttr(T('schedules.col_enabled'))}" renderer="chip" nowrap></tf-column>
          <tf-column key="last" label="${escapeAttr(T('schedules.col_last'))}" renderer="html" nowrap hide-below="900"></tf-column>
          <tf-column key="next" label="${escapeAttr(T('schedules.col_next'))}" renderer="text" nowrap hide-below="1100"></tf-column>
        </tf-table>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('list')} ${escapeHtml(T('jobs.history'))}</div>
          <div class="actions"><tf-filter-chips id="nas-jobs-filters"></tf-filter-chips></div>
        </div>
        <tf-table id="nas-jobs-table" empty-message="${escapeAttr(T('jobs.none'))}">
          <tf-column key="kind" label="${escapeAttr(T('jobs.col_kind'))}" renderer="text" fill></tf-column>
          <tf-column key="subject" label="${escapeAttr(T('jobs.col_subject'))}" renderer="text"></tf-column>
          <tf-column key="status" label="${escapeAttr(T('jobs.col_status'))}" renderer="chip"></tf-column>
          <tf-column key="startedBy" label="${escapeAttr(T('jobs.col_by'))}" renderer="text" hide-below="900"></tf-column>
          <tf-column key="startedAt" label="${escapeAttr(T('jobs.col_started'))}" renderer="text" nowrap></tf-column>
          <tf-column key="finishedAt" label="${escapeAttr(T('jobs.col_finished'))}" renderer="text" nowrap hide-below="1000"></tf-column>
        </tf-table>
      </div>
    </div>`;

  const state = { jobs: [], done: [], filter: 'all', schedules: null };

  const jobsTable = body.querySelector('#nas-jobs-table');
  jobsTable.rowActions = (row) => {
    const b = document.createElement('tf-button');
    b.setAttribute('size', 'sm');
    b.setAttribute('variant', 'ghost');
    b.setAttribute('icon', 'file-text');
    b.textContent = T('jobs.log');
    b.addEventListener('click', (e) => { e.stopPropagation(); screen.openJobLog(row._job.jobId); });
    return b;
  };
  jobsTable.addEventListener('row-click', (e) => screen.openJobLog(e.detail.row._job.jobId));
  const filters = body.querySelector('#nas-jobs-filters');
  filters.filters = ['all', 'errors', 'scrub', 'snapshot'].map((id) => ({ id, label: T('jobs.filter_' + id), active: id === state.filter }));
  filters.addEventListener('change', (e) => { state.filter = e.detail.id; paintHistory(); });

  const paintHistory = () => {
    const rows = state.done.filter((j) => {
      if (state.filter === 'errors') return j.status === 'failed' || j.status === 'blocked';
      if (state.filter === 'scrub') return /scrub|resilver|replace/.test(String(j.kind));
      if (state.filter === 'snapshot') return /snapshot|rollback/.test(String(j.kind));
      return true;
    });
    jobsTable.rows = rows.map((j) => ({
      _job: j,
      kind: jobKindLabel(j.kind),
      subject: j.subject,
      status: { status: jobTone(j.status), label: T('jobs.status_' + j.status), dot: true },
      startedBy: j.startedBy,
      startedAt: fmtDate(j.startedAt),
      finishedAt: j.finishedAt ? fmtDate(j.finishedAt) : '—',
    }));
  };

  const refreshJobs = async () => {
    if (screen.disposed || !body.isConnected) return;
    try {
      const res = await screen.nas('tentaNasJobsListRequest', { limit: JOBS_LIMIT });
      if (screen.disposed || !body.isConnected) return;
      state.jobs = res.jobs || [];
      const running = state.jobs.filter((j) => j.status === 'running' || j.status === 'queued');
      state.done = state.jobs.filter((j) => !running.includes(j));
      const runEl = body.querySelector('#nas-jobs-running');
      runEl.innerHTML = running.length ? running.map((j) => screen.jobRowHtml(j)).join('') : `<div class="muted">${escapeHtml(T('jobs.none_running'))}</div>`;
      screen.wireJobRows(runEl, refreshJobs);
      body.querySelector('#nas-jobs-hint').textContent = running.length ? T('jobs.running_count', { n: running.length }) : '';
      paintHistory();
    } catch (e) {
      if (screen.disposed || !body.isConnected) return;
      toast(T('jobs.failed', { error: errMessage(e) }), 'error');
    }
  };
  // Polling is a separate loop so that the immediate refreshes after a cancel
  // or an edit never spawn a second timer chain.
  const pollJobs = async () => { await refreshJobs(); if (!screen.disposed && body.isConnected) screen.later(pollJobs, POLL_JOBS_MS); };

  const refreshSchedules = async () => {
    if (screen.disposed || !body.isConnected) return;
    try {
      state.schedules = await screen.nas('tentaNasSchedulesListRequest', {});
      if (screen.disposed || !body.isConnected) return;
      paintSchedules();
      paintProtection();
    } catch (e) {
      if (screen.disposed || !body.isConnected) return;
      toast(errMessage(e), 'error');
    }
  };
  const pollSchedules = async () => { await refreshSchedules(); if (!screen.disposed && body.isConnected) screen.later(pollSchedules, POLL_SCHEDULES_MS); };

  const paintProtection = () => {
    const rows = state.schedules?.rows || [];
    const smart = state.schedules?.smart || {};
    const scrub = rows.filter((r) => r.kind === 'scrub');
    const snaps = rows.filter((r) => r.kind === 'snapshot');
    const tiles = [
      {
        icon: 'shield', label: T('schedules.prot_scrub'),
        value: scrub.length ? `${scrub.filter((r) => r.enabled).length}/${scrub.length}` : '—',
        suffix: scrub.length ? T('schedules.prot_pools') : T('schedules.prot_none'),
        accent: scrub.length && scrub.every((r) => r.enabled) ? 'success' : scrub.length ? 'warning' : '',
      },
      {
        icon: 'clock', label: T('schedules.prot_snapshots'),
        value: snaps.length ? `${snaps.filter((r) => r.enabled).length}/${snaps.length}` : '—',
        suffix: snaps.length ? T('schedules.prot_datasets') : T('schedules.prot_none'),
        accent: snaps.length && snaps.every((r) => r.enabled) ? 'success' : snaps.length ? 'warning' : '',
      },
      {
        icon: 'cylinder', label: T('schedules.prot_smart'),
        value: smart.enabled ? T('schedule.on') : T('schedule.off'),
        suffix: smart.enabled && smart.nextShortAt ? T('schedules.next_in', { t: fmtIn(smart.nextShortAt) }) : '',
        accent: smart.enabled ? 'success' : 'warning',
      },
      {
        icon: 'alert', label: T('schedules.prot_failures'),
        value: String(rows.filter((r) => r.lastResult === 'failed').length),
        suffix: T('schedules.prot_failures_sub'),
        accent: rows.some((r) => r.lastResult === 'failed') ? 'danger' : 'success',
      },
    ];
    body.querySelector('#nas-prot').innerHTML = tiles.map((t) => `<tf-stat-card icon="${t.icon}" label="${escapeAttr(t.label)}" value="${escapeAttr(t.value)}" suffix="${escapeAttr(t.suffix)}" accent="${t.accent}"></tf-stat-card>`).join('');
  };

  const paintSchedules = () => {
    const table = body.querySelector('#nas-sched-table');
    const rows = state.schedules?.rows || [];
    table.rowActions = admin ? (row) => {
      const b = document.createElement('tf-button');
      b.setAttribute('size', 'sm');
      b.setAttribute('variant', 'ghost');
      b.setAttribute('icon', 'edit');
      b.setAttribute('title', I18n.t('common.edit'));
      b.addEventListener('click', (e) => { e.stopPropagation(); editSchedule(row._row); });
      return b;
    } : null;
    table.rows = rows.map((r) => ({
      _row: r,
      kind: `<span class="tf-table__cell-row">${sprite(KIND_ICON[r.kind] || 'clock')} ${escapeHtml(T('schedules.kind_' + r.kind))}</span>`,
      subject: `<span class="tf-table__cell--mono">${escapeHtml(r.subject)}</span>`,
      schedule: `<span class="sched-pill">${escapeHtml(fmtSchedule(r.schedule))}</span>`,
      enabled: { status: r.enabled ? 'ok' : 'warn', label: r.enabled ? T('schedule.on') : T('schedule.off'), dot: true },
      last: r.lastRunAt
        ? `<span>${escapeHtml(fmtAgo(r.lastRunAt))}</span> <tf-chip size="sm" status="${lastTone(r.lastResult)}" label="${escapeAttr(T('schedules.result_' + (r.lastResult || 'unknown')))}"></tf-chip>`
        : `<span class="tf-table__cell-sub">${escapeHtml(T('never'))}</span>`,
      next: r.enabled && r.nextRunAt ? fmtIn(r.nextRunAt) : '—',
    }));
  };

  const editSchedule = (r) => {
    if (r.kind === 'scrub') {
      openScheduleEditor({
        title: T('pool.scrub_schedule_title', { name: r.subject }),
        icon: 'shield',
        schedule: r.schedule,
        enabled: r.enabled,
        allowed: SCRUB_ONLY,
        note: T('pool.scrub_schedule_note'),
        onSave: async ({ enabled, schedule }) => {
          await screen.nas('tentaNasScrubScheduleSetRequest', { name: r.subject, enabled, schedule });
          refreshSchedules();
        },
      });
      return;
    }
    if (r.kind === 'snapshot') {
      // The overview row carries only the cadence; the retention counts and
      // the schedule id live in the snapshot-schedule list.
      screen.nas('tentaNasSnapshotSchedulesListRequest', {}).then((res) => {
        const full = (res.schedules || []).find((s) => s.dataset === r.subject);
        if (!full) { toast(T('schedules.snapshot_missing', { dataset: r.subject }), 'warning'); refreshSchedules(); return; }
        openSnapshotScheduleEditor(screen, { schedule: full, datasets: [{ name: full.dataset }], onDone: refreshSchedules });
      }).catch((e) => toast(errMessage(e), 'error'));
      return;
    }
    openSmartScheduleEditor(screen, state.schedules?.smart || {}, refreshSchedules);
  };

  body.querySelector('[data-act="smart"]')?.addEventListener('click', () => openSmartScheduleEditor(screen, state.schedules?.smart || {}, refreshSchedules));

  await Promise.all([pollJobs(), pollSchedules()]);
}

const KIND_ICON = { scrub: 'shield', snapshot: 'clock', smart_short: 'cylinder', smart_long: 'cylinder' };

function lastTone(result) {
  return result === 'ok' || result === 'succeeded' ? 'ok' : result === 'failed' ? 'err' : result === 'skipped' ? 'warn' : 'info';
}

// SMART schedule: one enable switch, two cadences (short test, long test).
export function openSmartScheduleEditor(screen, smart, onDone) {
  const shortS = normalizeSchedule(smart.short || { every: 'daily', hour: 3, minute: 0 });
  const longS = normalizeSchedule(smart.long || { every: 'monthly', day: 1, hour: 4, minute: 0 });
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('schedules.smart_title'));
  win.setAttribute('icon', 'shield');
  win.setAttribute('buttons', 'close');
  win.setAttribute('draggable', '');
  win.setAttribute('width', '620');
  win.setAttribute('min-width', '480');
  win.setAttribute('initial-x', 'center');
  win.setAttribute('initial-y', 'center');
  win.innerHTML = `
    <div slot="body" class="stack">
      <div class="explain-box">${escapeHtml(T('schedules.smart_explain'))}</div>
      <div class="toggle-card">
        <div class="tc-text"><span>${escapeHtml(T('schedule.enabled'))}</span><span class="tc-sub">${escapeHtml(T('schedules.smart_enabled_sub'))}</span></div>
        <tf-toggle id="nas-smart-enabled" ${smart.enabled ? 'checked' : ''}></tf-toggle>
      </div>
      <div class="wizard-section-title">${escapeHtml(T('schedules.smart_short'))}</div>
      ${scheduleFieldsHtml('nas-smart-short', shortS, { allowed: ['daily', 'weekly'] })}
      <div class="wizard-section-title">${escapeHtml(T('schedules.smart_long'))}</div>
      ${scheduleFieldsHtml('nas-smart-long', longS, { allowed: ['weekly', 'monthly'] })}
      <div class="num-err" id="nas-smart-error" hidden></div>
    </div>
    <div slot="footer">
      <tf-button variant="ghost" data-action="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>
      <tf-button variant="primary" icon="save" data-action="confirm">${escapeHtml(T('schedule.save'))}</tf-button>
    </div>`;
  document.body.appendChild(win);
  wireScheduleFields(win, 'nas-smart-short', shortS);
  wireScheduleFields(win, 'nas-smart-long', longS);
  let busy = false;
  win.addEventListener('action', async (e) => {
    if (e.detail?.action === 'cancel') { win.close(true); return; }
    if (e.detail?.action !== 'confirm') return;
    e.preventDefault();
    if (busy) return;
    busy = true;
    try {
      await screen.nas('tentaNasSmartScheduleSetRequest', {
        enabled: Boolean(win.querySelector('#nas-smart-enabled').checked),
        short: readScheduleFields(win, 'nas-smart-short'),
        long: readScheduleFields(win, 'nas-smart-long'),
      });
      toast(T('schedule.saved'), 'success');
      win.close(true);
      if (onDone) onDone();
    } catch (err) {
      busy = false;
      const errEl = win.querySelector('#nas-smart-error');
      errEl.textContent = errMessage(err);
      errEl.hidden = false;
    }
  });
  return win;
}
