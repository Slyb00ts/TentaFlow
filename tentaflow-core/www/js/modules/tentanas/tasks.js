// ===== File: modules/tentanas/tasks.js — the Tasks tab (n15): running jobs, the four-eyes queue, the protection status strip, every schedule (scrub / snapshot / SMART) and the job history =====
//
// The tab polls three lists: jobs every POLL_JOBS_MS (so a running scrub or
// resilver moves visibly), schedules every POLL_SCHEDULES_MS (the next-run
// column only changes when a schedule fires) and the operations waiting for a
// second admin (§5.10) on the same slower cadence. Editing a schedule reuses
// the same field set as the pool detail, so an admin sees identical forms
// wherever a cadence is set.

import { escapeHtml, escapeAttr, toast } from '/js/utils.js';
import { I18n } from '/js/i18n.js';
import {
  T, sprite, POLL_JOBS_MS, ADMIN_TIMEOUT_MS, fmtDate, fmtAgo, fmtIn, fmtDuration, parseServerTs, errMessage,
  jobTone, jobKindLabel, fmtSchedule,
} from '/js/modules/tentanas/format.js';
import { openScheduleEditor, scheduleFieldsHtml, wireScheduleFields, readScheduleFields, normalizeSchedule } from '/js/modules/tentanas/schedule-editor.js';
import { openSnapshotScheduleEditor, keepSummary } from '/js/modules/tentanas/snapshots.js';
import { followResponse } from '/js/modules/tentanas/dialogs.js';
import { approvalsCardHtml, wireApprovals } from '/js/modules/tentanas/approvals.js';
import '/js/components/tf-table.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import '/js/components/tf-window.js';
import '/js/components/tf-toggle.js';

const POLL_SCHEDULES_MS = 30000;
const JOBS_LIMIT = 100;
const SCRUB_ONLY = ['weekly', 'monthly'];

export async function drawTasks(screen, body) {
  const admin = screen.isAdmin;
  body.innerHTML = `
    <div class="stack">
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('line-chart')} ${escapeHtml(T('jobs.running_now'))} <tf-chip size="sm" status="accent" id="nas-jobs-count" label="0"></tf-chip></div>
          <span class="hint">${escapeHtml(T('jobs.running_hint', { s: Math.round(POLL_JOBS_MS / 1000) }))}</span>
        </div>
        <div id="nas-jobs-running"></div>
      </div>
      ${approvalsCardHtml(admin)}
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('shield')} ${escapeHtml(T('schedules.prot_title'))}</div>
          <span class="hint">${escapeHtml(T('schedules.prot_hint'))}</span>
        </div>
        <div class="prot-grid" id="nas-prot"></div>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('calendar')} ${escapeHtml(T('schedules.title'))} <tf-chip size="sm" id="nas-sched-count" label="0"></tf-chip></div>
          <div class="actions">${admin ? `<tf-button variant="secondary" size="sm" icon="plus" data-act="new">${escapeHtml(T('schedules.new'))}</tf-button>` : ''}</div>
        </div>
        <div id="nas-sched-list"></div>
      </div>
      <div class="section-card">
        <div class="section-card-head">
          <div class="title">${sprite('history')} ${escapeHtml(T('jobs.history'))}</div>
          <div class="actions"><tf-filter-chips id="nas-jobs-filters"></tf-filter-chips></div>
        </div>
        <tf-table id="nas-jobs-table" actions-label="${escapeAttr(I18n.t('common.actions'))}" empty-message="${escapeAttr(T('jobs.none'))}">
          <tf-column key="task" label="${escapeAttr(T('jobs.col_task'))}" renderer="html" fill></tf-column>
          <tf-column key="node" label="${escapeAttr(T('jobs.col_node'))}" renderer="html" nowrap hide-below="900"></tf-column>
          <tf-column key="startedAt" label="${escapeAttr(T('jobs.col_started'))}" renderer="html" nowrap></tf-column>
          <tf-column key="duration" label="${escapeAttr(T('jobs.col_duration'))}" renderer="html" nowrap hide-below="1000"></tf-column>
          <tf-column key="result" label="${escapeAttr(T('jobs.col_result'))}" renderer="html"></tf-column>
        </tf-table>
      </div>
    </div>`;

  const state = { jobs: [], done: [], filter: 'all', schedules: null, snapshotSchedules: [] };
  // A parked red-path operation is a task of this node like any other, so the
  // list sits with the jobs and shares their refresh cadence.
  const approvals = wireApprovals(screen, body, { onExecuted: () => refreshJobs() });
  const nodeName = screen.currentNode()?.nodeName || '';

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
  filters.filters = ['all', 'errors', 'scrub'].map((id) => ({ id, label: T('jobs.filter_' + id), active: id === state.filter }));
  filters.addEventListener('change', (e) => { state.filter = e.detail.id; paintHistory(); });

  const paintHistory = () => {
    const rows = state.done.filter((j) => {
      if (state.filter === 'errors') return j.status === 'failed' || j.status === 'blocked';
      if (state.filter === 'scrub') return /scrub|resilver|replace/.test(String(j.kind));
      return true;
    });
    jobsTable.rows = rows.map((j) => ({
      _job: j,
      task: `<span class="tf-table__cell-title">${escapeHtml(jobKindLabel(j.kind))}</span><div class="tf-table__cell-sub tf-table__cell-sub--mono">${escapeHtml(j.subject)}</div>`,
      node: `<span class="tf-table__cell--mono">${escapeHtml(nodeName)}</span>`,
      startedAt: `<span class="tf-table__cell--mono">${escapeHtml(fmtDate(j.startedAt))}</span>`,
      duration: `<span class="tf-table__cell--mono">${escapeHtml(jobDuration(j))}</span>`,
      result: `<tf-chip size="sm" dot status="${jobTone(j.status)}" label="${escapeAttr(T('jobs.status_' + j.status))}"></tf-chip>${j.error ? `<div class="tf-table__cell-sub">${escapeHtml(j.error)}</div>` : ''}`,
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
      body.querySelector('#nas-jobs-count').setAttribute('label', String(running.length));
      paintHistory();
    } catch (e) {
      if (screen.disposed || !body.isConnected) return;
      toast(T('jobs.failed', { error: errMessage(e) }), 'error');
    }
  };
  // Polling is a separate loop so that the immediate refreshes after a cancel
  // or an edit never spawn a second timer chain.
  const pollJobs = async () => { await refreshJobs(); if (!screen.disposed && body.isConnected) screen.later(pollJobs, POLL_JOBS_MS); };

  const pollApprovals = async () => {
    await approvals.refresh();
    if (!screen.disposed && body.isConnected) screen.later(pollApprovals, POLL_SCHEDULES_MS);
  };

  const refreshSchedules = async () => {
    if (screen.disposed || !body.isConnected) return;
    try {
      const [all, snaps] = await Promise.all([
        screen.nas('tentaNasSchedulesListRequest', {}),
        screen.nas('tentaNasSnapshotSchedulesListRequest', {}),
      ]);
      if (screen.disposed || !body.isConnected) return;
      state.schedules = all;
      state.snapshotSchedules = snaps.schedules || [];
      paintSchedules();
      paintProtection();
    } catch (e) {
      if (screen.disposed || !body.isConnected) return;
      toast(errMessage(e), 'error');
    }
  };
  const pollSchedules = async () => { await refreshSchedules(); if (!screen.disposed && body.isConnected) screen.later(pollSchedules, POLL_SCHEDULES_MS); };

  // Two columns of labelled rows: what protects the data (snapshots) on the
  // left, what checks it (scrub, SMART) on the right.
  const paintProtection = () => {
    const rows = state.schedules?.rows || [];
    const smart = state.schedules?.smart || {};
    const scrub = rows.filter((r) => r.kind === 'scrub');
    const snaps = rows.filter((r) => r.kind === 'snapshot');
    const chip = (status, label) => `<tf-chip size="sm" dot status="${status}" label="${escapeAttr(label)}"></tf-chip>`;
    // A protected schedule gets a second chip: the snapshots it takes cannot
    // be deleted from this app at all, which is worth seeing next to "last run".
    const protectDays = (dataset) => Number(state.snapshotSchedules.find((s) => s.dataset === dataset)?.protectDays) || 0;
    const left = snaps.length ? snaps.map((r) => `
      <div class="sr"><span class="k">${sprite('save')} ${escapeHtml(T('schedules.prot_snapshots_of', { dataset: r.subject }))}</span><span class="v">${
        !r.enabled ? chip('warn', T('schedule.off'))
          : r.lastResult === 'failed' ? chip('err', T('schedules.prot_last_failed', { t: fmtAgo(r.lastRunAt) }))
            : r.lastRunAt ? chip('ok', T('schedules.prot_last', { t: fmtAgo(r.lastRunAt) })) : chip('info', T('schedules.prot_pending', { t: fmtIn(r.nextRunAt) }))}${
        protectDays(r.subject) ? ` ${chip('ok', T('schedules.prot_protected', { n: protectDays(r.subject) }))}` : ''}</span></div>`).join('')
      : `<div class="sr"><span class="k">${sprite('save')} ${escapeHtml(T('schedules.prot_snapshots'))}</span><span class="v">${chip('warn', T('schedules.prot_none'))}</span></div>`;
    const right = [
      ...(scrub.length ? scrub.map((r) => `
        <div class="sr"><span class="k">${sprite('refresh')} ${escapeHtml(T('schedules.prot_scrub_of', { pool: r.subject }))}</span><span class="v">${
          !r.enabled ? chip('warn', T('schedule.off'))
            : r.lastResult === 'failed' ? chip('err', T('schedules.prot_last_failed', { t: fmtAgo(r.lastRunAt) }))
              : escapeHtml(r.nextRunAt ? T('schedules.prot_next', { t: fmtIn(r.nextRunAt), when: fmtSchedule(r.schedule) }) : '—')}</span></div>`)
        : [`<div class="sr"><span class="k">${sprite('refresh')} ${escapeHtml(T('schedules.prot_scrub'))}</span><span class="v">${chip('warn', T('schedules.prot_none'))}</span></div>`]),
      `<div class="sr"><span class="k">${sprite('cylinder')} ${escapeHtml(T('schedules.prot_smart'))}</span><span class="v">${
        !smart.enabled ? chip('warn', T('schedule.off'))
          : smart.lastShortAt ? chip('ok', T('schedules.prot_smart_last', { t: fmtAgo(smart.lastShortAt) }))
            : chip('info', T('schedules.prot_pending', { t: fmtIn(smart.nextShortAt) }))}</span></div>`,
    ].join('');
    body.querySelector('#nas-prot').innerHTML = `<div class="stat-rows">${left}</div><div class="stat-rows">${right}</div>`;
  };

  const paintSchedules = () => {
    const rows = state.schedules?.rows || [];
    const smart = state.schedules?.smart || {};
    // The two SMART rows of the wire shape are one schedule for the reader:
    // both cadences sit in one row, like the SMART editor.
    const items = rows.filter((r) => r.kind === 'scrub' || r.kind === 'snapshot').map((r) => scheduleItem(r));
    const smartRows = rows.filter((r) => r.kind === 'smart_short' || r.kind === 'smart_long');
    if (smartRows.length) items.push(smartItem(smart, smartRows));
    body.querySelector('#nas-sched-count').setAttribute('label', String(items.length));
    const list = body.querySelector('#nas-sched-list');
    if (!items.length) { list.innerHTML = `<div class="muted">${escapeHtml(T('schedules.none'))}</div>`; return; }
    list.innerHTML = items.map((it, i) => `
      <div class="job-row" data-idx="${i}">
        <div class="job-ico">${sprite(it.icon)}</div>
        <div class="job-main">
          <div class="job-name">${escapeHtml(it.name)}</div>
          <div class="job-sub">${it.pills.map((p) => `<span class="sched-pill">${sprite('clock')} ${escapeHtml(p)}</span>`).join(' ')} <span>${escapeHtml(it.sub)}</span></div>
        </div>
        <div class="job-actions">
          <tf-toggle data-act="toggle" ${it.enabled ? 'checked' : ''} ${admin ? '' : 'disabled'} title="${escapeAttr(it.enabled ? T('schedule.on') : T('schedule.off'))}"></tf-toggle>
          ${admin ? `
          <tf-button size="sm" variant="ghost" icon="play" data-act="run" title="${escapeAttr(T('schedules.run_now'))}"></tf-button>
          <tf-button size="sm" variant="ghost" icon="edit" data-act="edit" title="${escapeAttr(I18n.t('common.edit'))}"></tf-button>` : ''}
        </div>
      </div>`).join('');
    list.querySelectorAll('.job-row').forEach((rowEl) => {
      const it = items[Number(rowEl.dataset.idx)];
      rowEl.querySelector('[data-act="toggle"]').addEventListener('change', (e) => setEnabled(it, Boolean(e.target.checked)));
      rowEl.querySelector('[data-act="run"]')?.addEventListener('click', () => runNow(it));
      rowEl.querySelector('[data-act="edit"]')?.addEventListener('click', () => editSchedule(it));
    });
  };

  const scheduleItem = (r) => {
    if (r.kind === 'scrub') {
      return {
        kind: 'scrub', row: r, icon: 'refresh', enabled: r.enabled,
        name: T('schedules.scrub_name', { pool: r.subject }),
        pills: [fmtSchedule(r.schedule)],
        sub: r.lastRunAt ? T('schedules.scrub_sub', { date: fmtDate(r.lastRunAt), result: T('schedules.result_' + (r.lastResult || 'unknown')) }) : T('schedules.never_ran'),
      };
    }
    const full = state.snapshotSchedules.find((s) => s.dataset === r.subject) || null;
    const sub = full ? T('schedules.snapshot_sub', { keep: keepSummary(full), n: full.snapshotCount || 0 }) : (r.lastRunAt ? T('schedules.last_run', { t: fmtAgo(r.lastRunAt) }) : T('schedules.never_ran'));
    return {
      kind: 'snapshot', row: r, full, icon: 'save', enabled: r.enabled,
      name: T('schedules.snapshot_name', { dataset: r.subject }),
      pills: [fmtSchedule(r.schedule)],
      sub: full?.protectDays ? `${sub} · ${T('schedules.snapshot_protected', { n: full.protectDays })}` : sub,
    };
  };
  const smartItem = (smart, rows) => {
    const short = rows.find((r) => r.kind === 'smart_short');
    const long = rows.find((r) => r.kind === 'smart_long');
    return {
      kind: 'smart', smart, icon: 'cylinder', enabled: Boolean(smart.enabled),
      name: T('schedules.smart_name'),
      pills: [
        T('schedules.smart_pill_short', { when: fmtSchedule(smart.short || short?.schedule) }),
        T('schedules.smart_pill_long', { when: fmtSchedule(smart.long || long?.schedule) }),
      ],
      sub: T('schedules.smart_sub', {
        short: smart.lastShortAt ? `${fmtAgo(smart.lastShortAt)} · ${T('schedules.result_' + (short?.lastResult || 'unknown'))}` : T('never'),
        long: smart.lastLongAt ? `${fmtDate(smart.lastLongAt)} · ${T('schedules.result_' + (long?.lastResult || 'unknown'))}` : T('never'),
      }),
    };
  };

  // The toggle resends the schedule as it is, with only `enabled` flipped.
  const setEnabled = async (it, enabled) => {
    try {
      if (it.kind === 'scrub') await screen.nas('tentaNasScrubScheduleSetRequest', { name: it.row.subject, enabled, schedule: it.row.schedule });
      else if (it.kind === 'snapshot') {
        if (!it.full) { toast(T('schedules.snapshot_missing', { dataset: it.row.subject }), 'warning'); return; }
        await screen.nas('tentaNasSnapshotScheduleSetRequest', { ...snapshotSchedulePayload(it.full), enabled });
      } else await screen.nas('tentaNasSmartScheduleSetRequest', { enabled, short: normalizeSchedule(it.smart.short), long: normalizeSchedule(it.smart.long) });
      toast(enabled ? T('schedules.enabled_done', { name: it.name }) : T('schedules.disabled_done', { name: it.name }), 'success');
    } catch (e) {
      toast(errMessage(e), 'error');
    }
    refreshSchedules();
  };

  const runNow = async (it) => {
    const title = T('schedules.run_now_title', { name: it.name });
    if (it.kind === 'scrub') {
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasPoolScrubRequest', { name: it.row.subject, action: 'start', sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), title);
      followResponse(screen, res, refreshJobs, T('schedules.run_started', { name: it.name }));
      return;
    }
    if (it.kind === 'snapshot') {
      const shortName = 'manual-' + new Date().toISOString().slice(0, 16).replace('T', '-').replace(':', '');
      // A protected schedule protects what its "run now" takes too — the
      // promise the schedule makes does not depend on who started the run.
      const res = await screen.withSudo((sudoPassword) => screen.nas('tentaNasSnapshotCreateRequest', { dataset: it.row.subject, shortName, recursive: Boolean(it.full?.recursive), protectDays: Number(it.full?.protectDays) || 0, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), title);
      followResponse(screen, res, refreshJobs, T('schedules.run_started', { name: it.name }));
      return;
    }
    // "All disks" means one short test per disk that reports SMART; a single
    // sudo prompt covers the whole batch.
    let disks;
    try {
      disks = (await screen.nas('tentaNasDisksListRequest', {})).disks || [];
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    const targets = disks.filter((d) => d.smartAvailable);
    if (!targets.length) { toast(T('schedules.smart_no_disks'), 'warning'); return; }
    const res = await screen.withSudo(async (sudoPassword) => {
      for (const d of targets) await screen.nas('tentaNasDiskSmartTestRequest', { diskId: d.diskId, kind: 'short', sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS });
      return { count: targets.length };
    }, title);
    if (res === null) return;
    toast(T('schedules.smart_started', { n: targets.length }), 'success');
    refreshJobs();
  };

  const editSchedule = (it) => {
    if (it.kind === 'scrub') {
      openScheduleEditor({
        title: T('pool.scrub_schedule_title', { name: it.row.subject }),
        icon: 'refresh',
        schedule: it.row.schedule,
        enabled: it.row.enabled,
        allowed: SCRUB_ONLY,
        note: T('pool.scrub_schedule_note'),
        onSave: async ({ enabled, schedule }) => {
          await screen.nas('tentaNasScrubScheduleSetRequest', { name: it.row.subject, enabled, schedule });
          refreshSchedules();
        },
      });
      return;
    }
    if (it.kind === 'snapshot') {
      if (!it.full) { toast(T('schedules.snapshot_missing', { dataset: it.row.subject }), 'warning'); refreshSchedules(); return; }
      openSnapshotScheduleEditor(screen, { schedule: it.full, datasets: [{ name: it.full.dataset }], onDone: refreshSchedules });
      return;
    }
    openSmartScheduleEditor(screen, it.smart, refreshSchedules);
  };

  // "Nowy harmonogram": scrub and SMART schedules exist per pool / per node
  // already, so a new one is always a snapshot schedule for some dataset.
  body.querySelector('[data-act="new"]')?.addEventListener('click', async () => {
    let datasets;
    try {
      const pools = (await screen.nas('tentaNasPoolsListRequest', {})).pools || [];
      const lists = await Promise.all(pools.map((p) => screen.nas('tentaNasDatasetsListRequest', { pool: p.name })));
      datasets = lists.flatMap((l) => l.datasets || []);
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    if (!datasets.length) { toast(T('schedules.no_datasets'), 'warning'); return; }
    openSnapshotScheduleEditor(screen, { datasets, onDone: refreshSchedules });
  });

  await Promise.all([pollJobs(), pollSchedules(), pollApprovals()]);
}

// Every field of the schedule, because the toggle resends the WHOLE schedule:
// one left out here is one silently reset on the node.
const snapshotSchedulePayload = (s) => ({
  scheduleId: s.scheduleId, dataset: s.dataset, enabled: Boolean(s.enabled), recursive: Boolean(s.recursive), schedule: normalizeSchedule(s.schedule),
  keepFrequent: Number(s.keepFrequent) || 0, keepHourly: Number(s.keepHourly) || 0, keepDaily: Number(s.keepDaily) || 0, keepWeekly: Number(s.keepWeekly) || 0, keepMonthly: Number(s.keepMonthly) || 0,
  protectDays: Number(s.protectDays) || 0,
});

function jobDuration(j) {
  const a = parseServerTs(j.startedAt);
  const b = parseServerTs(j.finishedAt);
  if (!a || !b) return '—';
  return fmtDuration(Math.max(0, (b.getTime() - a.getTime()) / 1000));
}

// SMART schedule: one enable switch, two cadences (short test, long test).
export function openSmartScheduleEditor(screen, smart, onDone) {
  const shortS = normalizeSchedule(smart.short || { every: 'daily', hour: 3, minute: 0 });
  const longS = normalizeSchedule(smart.long || { every: 'monthly', day: 1, hour: 4, minute: 0 });
  const win = document.createElement('tf-window');
  win.className = 'nas-modal';
  win.setAttribute('title', T('schedules.smart_title'));
  win.setAttribute('icon', 'cylinder');
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
      <h2 class="wizard-section-title">${escapeHtml(T('schedules.smart_short'))}</h2>
      ${scheduleFieldsHtml('nas-smart-short', shortS, { allowed: ['daily', 'weekly'] })}
      <h2 class="wizard-section-title">${escapeHtml(T('schedules.smart_long'))}</h2>
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
