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
  jobTone, jobKindLabel, jobCanCancel, fmtSchedule, nodeLabel, jobAuthor, runDiskBatch, refusedBatchNames,
} from '/js/modules/tentanas/format.js';
import { setAttr, setText, patchHtml, patchKeyedList } from '/js/modules/tentanas/dom-patch.js';
import { isOpaqueId, isDiskIdShape } from '/js/modules/tentanas/machine-id.js';
import { openScheduleEditor, scheduleFieldsHtml, wireScheduleFields, readScheduleFields, normalizeSchedule } from '/js/modules/tentanas/schedule-editor.js';
import { openSnapshotScheduleEditor, keepSummary } from '/js/modules/tentanas/snapshots.js';
import { openMoverScheduleEditor, openElasticScheduleEditor } from '/js/modules/tentanas/elastic-detail.js';
import { followResponse } from '/js/modules/tentanas/dialogs.js';
import { approvalsCardHtml, wireApprovals } from '/js/modules/tentanas/approvals.js';
import { accessLogCardHtml, wireAccessLog } from '/js/modules/tentanas/access-log.js';
import '/js/components/tf-table.js';
import '/js/components/tf-filter-chips.js';
import '/js/components/tf-chip.js';
import '/js/components/tf-button.js';
import { TfWindow } from '/js/components/tf-window.js';
import '/js/components/tf-toggle.js';

const POLL_SCHEDULES_MS = 30000;
const JOBS_LIMIT = 100;
const SCRUB_ONLY = ['weekly', 'monthly'];
// TRIM competes with real I/O, so the cadences stop at weekly for the same
// reason the scrub's do (§5.10).
const TRIM_ONLY = ['weekly', 'monthly'];

// A job's subject as every job renderer shows it — the running list and the
// history here, the overview card and the job-log header in tentanas.js.
//
// The subject is a NAME for every job kind but one: a SMART test is spawned
// on the `disk_id`, and the node swaps in the disk's name when it has one
// (`name_jobs` in dispatch/tentanas.rs). So:
// - an id that reached the row anyway (a disk the node never named) is not
//   text — the row shows the kind alone and the id is the tooltip;
// - a name the node only REMEMBERS (the disk has since left its inventory)
//   is marked as last-known, never shown as if it were the current device:
//   the kernel may have handed that name to another disk since.
//
// Only a SMART test's subject can BE a disk id, so only that kind uses the
// disk rule; every other kind's subject is a pool/dataset/share/target name
// that must not be hidden just because it starts with `dev-`/`usb-`/`pci-`/…
// or is all digits (a share named `dev-backups`, a pool `2024`).
//
// Returns `{ text, title }`; both are plain strings, escaped by the caller.
export function jobSubject(j) {
  const subject = String(j?.subject || '').trim();
  if (!subject) return { text: '', title: '' };
  const isId = j?.kind === 'smart_test' ? isDiskIdShape(subject) : isOpaqueId(subject);
  if (isId) return { text: '', title: subject };
  if (j.subjectLastKnown) return { text: T('jobs.subject_last_known', { name: subject }), title: '' };
  return { text: subject, title: '' };
}

function titleAttr(title) {
  return title ? ` title="${escapeAttr(title)}"` : '';
}

// The history table's task cell: the kind, and the subject under it.
function historyTaskHtml(j) {
  const subject = jobSubject(j);
  return `<span class="tf-table__cell-title">${escapeHtml(jobKindLabel(j.kind))}</span><div class="tf-table__cell-sub tf-table__cell-sub--mono"${titleAttr(subject.title)}>${escapeHtml(subject.text)}</div>`;
}

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
      ${accessLogCardHtml(admin)}
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
  // The access log is this node's own audit trail (§5.10): it belongs with the
  // other things the Tasks tab watches, and it moves on the slow cadence
  // because an audit log is reviewed, not watched live.
  const accessLog = wireAccessLog(screen, body);
  const node = screen.currentNode();

  const jobsTable = body.querySelector('#nas-jobs-table');
  jobsTable.rowActions = (row, idx, currentRow) => {
    const live = () => currentRow?.() ?? row;
    const b = document.createElement('tf-button');
    b.setAttribute('size', 'sm');
    b.setAttribute('variant', 'ghost');
    b.setAttribute('icon', 'file-text');
    b.textContent = T('jobs.log');
    b.addEventListener('click', (e) => { e.stopPropagation(); screen.openJobLog(live()._job.jobId); });
    return b;
  };
  jobsTable.addEventListener('row-click', (e) => screen.openJobLog(e.detail.row._job.jobId));
  const filters = body.querySelector('#nas-jobs-filters');
  filters.filters = ['all', 'errors', 'scrub', 'mover'].map((id) => ({ id, label: T('jobs.filter_' + id), active: id === state.filter }));
  filters.addEventListener('change', (e) => { state.filter = e.detail.id; paintHistory(); });

  // The running-jobs skeleton carries only what a job's IDENTITY decides
  // (icon slot, name, subject, whether it can be cancelled): fields that stay
  // put for the job's whole life. `progressPct`, the elapsed "started …" text
  // and the status chip tick on almost every 3 s poll, so they are left as
  // empty slots here and painted by `paintRunningJob` below — otherwise
  // keying this list by `jobId` would still rebuild the row on every poll
  // (a fresh string every time) and the Cancel button under the cursor would
  // never survive a single tick.
  const runningJobSkeleton = (j, subject = jobSubject(j)) => `
    <div class="job-row" data-job="${escapeAttr(j.jobId)}">
      <div class="job-ico" data-role="ico"></div>
      <div class="job-main">
        <div class="job-name">${escapeHtml(jobKindLabel(j.kind))} <span class="mono text-2"${titleAttr(subject.title)}>${escapeHtml(subject.text)}</span> <tf-chip data-role="status"></tf-chip></div>
        <div class="job-sub" data-role="sub"></div>
        ${j.progressPct != null ? `<tf-progress-bar data-role="progress" size="sm" tone="accent"></tf-progress-bar>` : ''}
      </div>
      <div class="job-actions">
        <tf-button size="sm" variant="ghost" icon="file-text" data-act="log" title="${escapeAttr(T('jobs.log'))}"></tf-button>
        ${jobCanCancel(j) ? `<tf-button size="sm" variant="ghost" icon="x" data-act="cancel">${escapeHtml(I18n.t('common.cancel'))}</tf-button>` : ''}
      </div>
    </div>`;

  // Only the icon's inner markup differs by state (spinning refresh vs a
  // static clock), so it goes through `patchHtml` too — the same "write only
  // when it changed" rule, just scoped to one child instead of the row.
  const paintRunningJob = (row, j) => {
    if (!row) return;
    const running = j.status === 'running';
    const ico = row.querySelector('[data-role="ico"]');
    ico.classList.toggle('running', running);
    patchHtml(ico, sprite(running ? 'refresh' : 'clock'));
    const chip = row.querySelector('[data-role="status"]');
    setAttr(chip, 'status', jobTone(j.status));
    setAttr(chip, 'label', T('jobs.status_' + j.status));
    const last = (j.log || []).slice(-1)[0] || '';
    const author = jobAuthor(j.startedBy);
    const sub = row.querySelector('[data-role="sub"]');
    setText(sub, T('jobs.started_by', { by: author.label, t: fmtAgo(j.startedAt) }) + (last ? ' · ' + last : ''));
    setAttr(sub, 'title', author.title || '');
    if (j.progressPct != null) setAttr(row.querySelector('[data-role="progress"]'), 'value', Number(j.progressPct));
  };

  const paintHistory = () => {
    const rows = state.done.filter((j) => {
      if (state.filter === 'errors') return j.status === 'failed' || j.status === 'blocked';
      if (state.filter === 'scrub') return /scrub|resilver|replace/.test(String(j.kind));
      // Cache drains are the runs an admin chases when the cache fills up —
      // automatic ones included — so they get a chip of their own, named for
      // what they do, instead of being found by reading the whole history.
      if (state.filter === 'mover') return /mover/.test(String(j.kind));
      return true;
    });
    jobsTable.rows = rows.map((j) => ({
      _job: j,
      task: historyTaskHtml(j),
      node: `<span class="tf-table__cell--mono" title="${escapeAttr(node?.nodeId || '')}">${escapeHtml(nodeLabel(node))}</span>`,
      startedAt: `<span class="tf-table__cell--mono">${escapeHtml(fmtDate(j.startedAt))}</span>`,
      duration: `<span class="tf-table__cell--mono">${escapeHtml(jobDuration(j))}</span>`,
      result: `<tf-chip size="sm" dot status="${jobTone(j.status)}" label="${escapeAttr(T('jobs.status_' + j.status))}"></tf-chip>${j.error ? `<div class="tf-table__cell-sub">${escapeHtml(j.error)}</div>` : ''}`,
    }));
  };

  // Delegated ONCE on the container rather than per row: `patchKeyedList`
  // keeps a row's Cancel/Log buttons as the very node a poll found them at,
  // so re-attaching a listener to them on every poll would either stack a
  // second handler on a survivor or silently do nothing for one that moved.
  // A listener on the (never rebuilt) container works for both cases and
  // every future row alike.
  const runEl = body.querySelector('#nas-jobs-running');
  const cancelJob = async (jobId) => {
    const ok = await TfWindow.confirm({ title: T('jobs.cancel'), message: T('jobs.cancel_confirm'), confirmLabel: T('jobs.cancel'), cancelLabel: I18n.t('common.cancel'), danger: true });
    if (!ok) return;
    try {
      await screen.nas('tentaNasJobCancelRequest', { jobId });
      refreshJobs();
    } catch (e) {
      toast(errMessage(e), 'error');
    }
  };
  runEl.addEventListener('click', (e) => {
    const row = e.target.closest('.job-row');
    if (!row) return;
    const jobId = row.dataset.job;
    if (e.target.closest('[data-act="log"]')) { screen.openJobLog(jobId); return; }
    if (e.target.closest('[data-act="cancel"]')) { e.stopPropagation(); cancelJob(jobId); }
  });

  const refreshJobs = async () => {
    if (screen.disposed || !body.isConnected) return;
    try {
      const res = await screen.nas('tentaNasJobsListRequest', { limit: JOBS_LIMIT });
      if (screen.disposed || !body.isConnected) return;
      state.jobs = res.jobs || [];
      const running = state.jobs.filter((j) => j.status === 'running' || j.status === 'queued');
      state.done = state.jobs.filter((j) => !running.includes(j));
      if (!running.length) {
        patchHtml(runEl, `<div class="muted">${escapeHtml(T('jobs.none_running'))}</div>`);
      } else {
        // Keyed by jobId: a job whose progress or elapsed-time text moves
        // keeps its own row (and every sibling's), unlike the old
        // `patchHtml` over the whole joined string, which rebuilt everyone's
        // Cancel button on every 3 s tick.
        patchKeyedList(runEl, running.map((j) => ({ key: j.jobId, html: runningJobSkeleton(j) })));
        running.forEach((j, i) => paintRunningJob(runEl.children[i], j));
      }
      setAttr(body.querySelector('#nas-jobs-count'), 'label', String(running.length));
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

  const pollAccessLog = async () => {
    await accessLog.refresh();
    if (!screen.disposed && body.isConnected) screen.later(pollAccessLog, POLL_SCHEDULES_MS);
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
      // The wire has NO real pass/fail for SMART here: `smart` (NasSmartSchedule)
      // carries only timestamps, and the core always sends `last_result:
      // String::new()` for the smart_short/smart_long rows too (§5.10). A
      // green "OK" the moment a timestamp exists would be invented — the last
      // run's actual result is honored when the row ever carries one, and
      // otherwise this says only THAT it ran, in a neutral tone, never OK.
      (() => {
        const shortRow = rows.find((r) => r.kind === 'smart_short');
        return `<div class="sr"><span class="k">${sprite('cylinder')} ${escapeHtml(T('schedules.prot_smart'))}</span><span class="v">${
          !smart.enabled ? chip('warn', T('schedule.off'))
            : !smart.lastShortAt ? chip('info', T('schedules.prot_pending', { t: fmtIn(smart.nextShortAt) }))
              : shortRow?.lastResult === 'failed' ? chip('err', T('schedules.prot_last_failed', { t: fmtAgo(smart.lastShortAt) }))
                : chip('info', T('schedules.prot_last', { t: fmtAgo(smart.lastShortAt) }))}</span></div>`;
      })(),
    ].join('');
    patchHtml(body.querySelector('#nas-prot'), `<div class="stat-rows">${left}</div><div class="stat-rows">${right}</div>`);
  };

  // One key per row: kind+subject already identifies a schedule uniquely on
  // the wire (one scrub per pool, one cadence per Elastic array-and-verb, one
  // schedule per dataset); the SMART pair folds to a single row that has no
  // `row` of its own.
  const scheduleKey = (it) => (it.kind === 'smart' ? 'smart' : `${it.kind}:${it.row.subject}`);

  // The skeleton is everything that only changes when an admin actually EDITS
  // the schedule (icon, name, pills, whether the row admin controls exist at
  // all): stable for as long as the row is on screen. `sub` — "last run …",
  // built with `fmtAgo`/`fmtIn` — ticks on its own as time passes, so it is
  // left as an empty slot and painted by hand below; baking it into the
  // compared string here would rebuild the row (and every later sibling, on
  // the old `patchHtml`) on a poll where nothing the admin can see actually
  // changed.
  const scheduleSkeleton = (it) => `
    <div class="job-row" data-key="${escapeAttr(scheduleKey(it))}">
      <div class="job-ico">${sprite(it.icon)}</div>
      <div class="job-main">
        <div class="job-name">${escapeHtml(it.name)}</div>
        <div class="job-sub">${it.pills.map((p) => `<span class="sched-pill">${sprite('clock')} ${escapeHtml(p)}</span>`).join(' ')} <span data-role="sub"></span></div>
      </div>
      <div class="job-actions">
        <tf-toggle data-act="toggle" ${admin ? '' : 'disabled'}></tf-toggle>
        ${admin ? `
        <tf-button size="sm" variant="ghost" icon="play" data-act="run" title="${escapeAttr(T('schedules.run_now'))}"></tf-button>
        <tf-button size="sm" variant="ghost" icon="edit" data-act="edit" title="${escapeAttr(I18n.t('common.edit'))}"></tf-button>` : ''}
      </div>
    </div>`;

  // Looked up by the delegated handlers below, always the CURRENT items —
  // rebuilt on every `paintSchedules()` call, so a click always acts on what
  // is actually on screen even when the row node itself was kept.
  const itemByKey = new Map();
  const paintSchedules = () => {
    const rows = state.schedules?.rows || [];
    const smart = state.schedules?.smart || {};
    // The two SMART rows of the wire shape are one schedule for the reader:
    // both cadences sit in one row, like the SMART editor.
    const items = rows.filter((r) => r.kind === 'scrub' || r.kind === 'trim' || r.kind === 'snapshot' || r.kind.startsWith('elastic_')).map((r) => scheduleItem(r));
    const smartRows = rows.filter((r) => r.kind === 'smart_short' || r.kind === 'smart_long');
    if (smartRows.length) items.push(smartItem(smart, smartRows));
    setAttr(body.querySelector('#nas-sched-count'), 'label', String(items.length));
    const list = body.querySelector('#nas-sched-list');
    itemByKey.clear();
    items.forEach((it) => itemByKey.set(scheduleKey(it), it));
    if (!items.length) { patchHtml(list, `<div class="muted">${escapeHtml(T('schedules.none'))}</div>`); return; }
    // Keyed by schedule: a row whose "last run" text moved keeps its own
    // toggle, and — unlike the old whole-list `patchHtml` — every sibling
    // keeps its toggle too, instead of the entire list being torn down
    // because ONE row's relative time crossed a minute.
    patchKeyedList(list, items.map((it) => ({ key: scheduleKey(it), html: scheduleSkeleton(it) })));
    items.forEach((it, i) => {
      const rowEl = list.children[i];
      setText(rowEl.querySelector('[data-role="sub"]'), it.sub);
      const toggle = rowEl.querySelector('[data-act="toggle"]');
      setAttr(toggle, 'checked', it.enabled);
      setAttr(toggle, 'title', it.enabled ? T('schedule.on') : T('schedule.off'));
    });
  };

  // Delegated once on the list container for the same reason as the jobs
  // list above: `patchKeyedList` keeps a row's toggle/run/edit buttons as the
  // exact node a poll found them at, so wiring them per row on every rebuild
  // would double up on a survivor.
  const scheduleList = body.querySelector('#nas-sched-list');
  scheduleList.addEventListener('change', (e) => {
    const toggle = e.target.closest('[data-act="toggle"]');
    const row = toggle?.closest('.job-row');
    const it = row && itemByKey.get(row.dataset.key);
    if (it) setEnabled(it, Boolean(e.target.checked));
  });
  scheduleList.addEventListener('click', (e) => {
    const row = e.target.closest('.job-row');
    const it = row && itemByKey.get(row.dataset.key);
    if (!it) return;
    if (e.target.closest('[data-act="run"]')) runNow(it);
    else if (e.target.closest('[data-act="edit"]')) editSchedule(it);
  });

  const scheduleItem = (r) => {
    // The three Elastic cadences (§5.3). `subject` is the ARRAY, and the kind
    // is prefixed so an array's scrub is never mistaken for a pool's.
    if (r.kind.startsWith('elastic_')) {
      const verb = r.kind.slice('elastic_'.length);
      return {
        kind: r.kind, row: r, verb, enabled: r.enabled,
        icon: verb === 'mover' ? 'transform' : verb === 'sync' ? 'refresh' : 'search',
        name: T('schedules.elastic_' + verb + '_name', { array: r.subject }),
        pills: [fmtSchedule(r.schedule)],
        // The stored result is the scheduler's own sentence ("started job …"),
        // not one of the `result_*` labels, so the row says WHEN it last ran
        // and leaves the outcome to the job the log links to.
        //
        // The mover row is not a cadence the array needs: moving is automatic,
        // and this row exists only because an admin saved a WINDOW that
        // restricts it. So it also says what its switch means for the files.
        sub: [
          r.lastRunAt ? T('schedules.last_run', { t: fmtAgo(r.lastRunAt) }) : T('schedules.never_ran'),
          ...(verb === 'mover' ? [T(r.enabled ? 'schedules.elastic_mover_window_on' : 'schedules.elastic_mover_window_off')] : []),
        ].join(' · '),
      };
    }
    // The scrub and the TRIM (§5.10) are the same row with a different verb:
    // one pool, one cadence, one last run.
    if (r.kind === 'scrub' || r.kind === 'trim') {
      return {
        kind: r.kind, row: r, icon: r.kind === 'trim' ? 'zap' : 'refresh', enabled: r.enabled,
        name: T('schedules.' + r.kind + '_name', { pool: r.subject }),
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
      else if (it.kind === 'trim') await screen.nas('tentaNasTrimScheduleSetRequest', { name: it.row.subject, enabled, schedule: it.row.schedule });
      else if (it.kind.startsWith('elastic_')) {
        const request = it.verb === 'mover' ? 'tentaNasElasticMoverScheduleSetRequest'
          : it.verb === 'sync' ? 'tentaNasElasticSyncScheduleSetRequest' : 'tentaNasElasticScrubScheduleSetRequest';
        // The mover's RULES are deliberately NOT sent. This request is about
        // the cadence; omitting them is what leaves the admin's age and
        // free-space settings untouched instead of overwriting them with zeros.
        await screen.nas(request, { name: it.row.subject, enabled, schedule: it.row.schedule });
      }
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
    if (it.kind.startsWith('elastic_')) {
      const request = it.verb === 'mover' ? 'tentaNasElasticArrayMoverRequest'
        : it.verb === 'sync' ? 'tentaNasElasticArraySyncRequest' : 'tentaNasElasticArrayScrubRequest';
      const res = await screen.withSudo((sudoPassword) => screen.nas(request, { name: it.row.subject, sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), title);
      followResponse(screen, res, refreshJobs, T('schedules.run_started', { name: it.name }));
      return;
    }
    if (it.kind === 'scrub' || it.kind === 'trim') {
      const request = it.kind === 'trim' ? 'tentaNasPoolTrimRequest' : 'tentaNasPoolScrubRequest';
      const res = await screen.withSudo((sudoPassword) => screen.nas(request, { name: it.row.subject, action: 'start', sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS }), title);
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
    // sudo prompt covers the whole batch. A disk that refuses per se (busy,
    // a test already runs, the disk rejects the command) must not stop the
    // batch: the old loop's bare `await` inside the `for` threw on the FIRST
    // rejection, which aborted the whole callback and left every later disk
    // silently untested (A6). A privilege/credential error is the opposite
    // case — it will fail identically for every remaining disk, so it must
    // stop the batch at once rather than replay the same rejected password
    // against sudo once per disk (see `runDiskBatch` / `isBatchHaltError` in
    // format.js, shared with the n03 bulk SMART action in tentanas.js).
    let disks;
    try {
      disks = (await screen.nas('tentaNasDisksListRequest', {})).disks || [];
    } catch (e) {
      toast(errMessage(e), 'error');
      return;
    }
    const targets = disks.filter((d) => d.smartAvailable);
    if (!targets.length) { toast(T('schedules.smart_no_disks'), 'warning'); return; }
    let outcome;
    try {
      outcome = await screen.withSudo((sudoPassword) => runDiskBatch(targets, (d) => screen.nas(
        'tentaNasDiskSmartTestRequest', { diskId: d.diskId, kind: 'short', sudoPassword }, { timeoutMs: ADMIN_TIMEOUT_MS },
      )), title);
    } catch (e) {
      // `withSudo` itself already catches and toasts every rejection from
      // its callback — this is a defensive backstop, not the normal path.
      toast(errMessage(e), 'error');
      return;
    }
    // `outcome` is null both when the prompt was cancelled and when
    // `runDiskBatch` halted the batch on a privilege/credential error —
    // `withSudo`'s own catch already toasted that one error.
    if (outcome === null) return;
    if (outcome.started.length) toast(T('schedules.smart_started', { n: outcome.started.length }), 'success');
    // The refusals are real per-disk data — each disk by name with the
    // node's own reason — so they are listed, never dropped; the sentence
    // around them is ours and translated.
    if (outcome.refused.length) toast(T('jobs.smart_batch_refused', { n: outcome.refused.length, disks: refusedBatchNames(outcome.refused) }), 'warning');
    refreshJobs();
  };

  const editSchedule = async (it) => {
    if (it.kind.startsWith('elastic_')) {
      if (it.verb !== 'mover') {
        openElasticScheduleEditor(screen, {
          name: it.row.subject, kind: it.verb, schedule: it.row.schedule, enabled: it.row.enabled,
        }, refreshSchedules);
        return;
      }
      // The mover dialog sets the RULES too, and the cadence row does not
      // carry them — so it opens on the array's real current settings rather
      // than on defaults that would overwrite them on save.
      try {
        const res = await screen.nas('tentaNasElasticArrayGetRequest', { name: it.row.subject });
        if (!res?.array) throw new Error(T('elastic.bad_response'));
        openMoverScheduleEditor(screen, res.array, refreshSchedules);
      } catch (e) {
        toast(errMessage(e), 'error');
      }
      return;
    }
    if (it.kind === 'scrub' || it.kind === 'trim') {
      const trim = it.kind === 'trim';
      openScheduleEditor({
        title: T(trim ? 'pool.trim_schedule_title' : 'pool.scrub_schedule_title', { name: it.row.subject }),
        icon: trim ? 'zap' : 'refresh',
        schedule: it.row.schedule,
        enabled: it.row.enabled,
        allowed: trim ? TRIM_ONLY : SCRUB_ONLY,
        note: T(trim ? 'pool.trim_schedule_note' : 'pool.scrub_schedule_note'),
        onSave: async ({ enabled, schedule }) => {
          await screen.nas(trim ? 'tentaNasTrimScheduleSetRequest' : 'tentaNasScrubScheduleSetRequest', { name: it.row.subject, enabled, schedule });
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

  await Promise.all([pollJobs(), pollSchedules(), pollApprovals(), pollAccessLog()]);
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
