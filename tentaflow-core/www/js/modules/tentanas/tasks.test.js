// =============================================================================
// File: modules/tentanas/tasks.test.js
// Description: The Tasks tab against a fake screen: running jobs and history
// split from JobsListResponse with the history filters, the schedule table
// and protection tiles from SchedulesListResponse, a scrub row edit sending
// ScrubScheduleSetRequest, and the SMART editor sending both cadences.
// Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, confirmWindow, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawTasks, openSmartScheduleEditor } = await import('./tasks.js');

const jobs = [
  { jobId: 'j1', kind: 'pool_scrub', subject: 'tank', status: 'running', startedBy: 'admin', startedAt: '2026-09-02 08:00:00', finishedAt: null, progressPct: 40 },
  { jobId: 'j2', kind: 'snapshot_destroy', subject: 'tank/home', status: 'succeeded', startedBy: 'scheduler', startedAt: '2026-09-01 02:00:00', finishedAt: '2026-09-01 02:00:05' },
  { jobId: 'j3', kind: 'pool_replace', subject: 'tank', status: 'failed', startedBy: 'admin', startedAt: '2026-08-30 10:00:00', finishedAt: '2026-08-30 10:05:00', error: 'disk too small' },
];

const schedules = {
  rows: [
    { kind: 'scrub', subject: 'tank', enabled: true, schedule: { every: 'weekly', hour: 2, minute: 0, weekday: 0, day: 1 }, lastRunAt: '2026-08-30 02:00:00', lastResult: 'ok', nextRunAt: '2026-09-06 02:00:00' },
    { kind: 'snapshot', subject: 'tank/home', enabled: false, schedule: { every: '1h', hour: 0, minute: 0, weekday: 0, day: 1 }, lastRunAt: null, lastResult: '', nextRunAt: null },
    { kind: 'smart_short', subject: '*', enabled: true, schedule: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 }, lastRunAt: '2026-09-02 03:00:00', lastResult: 'failed', nextRunAt: '2026-09-03 03:00:00' },
  ],
  smart: { enabled: true, short: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 }, long: { every: 'monthly', hour: 4, minute: 0, weekday: 0, day: 1 }, lastShortAt: '2026-09-02 03:00:00', lastLongAt: null, nextShortAt: '2026-09-03 03:00:00', nextLongAt: null },
};

function mount() {
  const body = document.createElement('div');
  document.body.appendChild(body);
  return body;
}

// tf-table renders into an open shadow root; the data rows stay on `rows`.
const tableRows = (table) => [...table.shadowRoot.querySelectorAll('tbody tr')];

test('splits running jobs from history and paints schedules with the protection tiles', async () => {
  const screen = fakeScreen({ tentaNasJobsListRequest: { jobs }, tentaNasSchedulesListRequest: schedules });
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  assert.deepEqual(screen.calls.map((c) => c.kind).sort(), ['tentaNasJobsListRequest', 'tentaNasSchedulesListRequest']);
  assert.equal(screen.calls.find((c) => c.kind === 'tentaNasJobsListRequest').payload.limit, 100);

  assert.equal(body.querySelectorAll('#nas-jobs-running .job-row').length, 1);
  assert.match(body.querySelector('#nas-jobs-hint').textContent, /1/);
  const history = body.querySelector('#nas-jobs-table');
  assert.equal(history.rows.length, 2, 'finished jobs only');
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j2', 'j3']);

  const sched = body.querySelector('#nas-sched-table');
  assert.equal(sched.rows.length, 3);
  assert.match(sched.rows[0].subject, /tank/);
  assert.match(sched.rows[0].schedule, /sched-pill/, 'cadence pill in the row');
  const rows = tableRows(sched);
  assert.equal(rows.length, 3);
  assert.equal(rows.filter((r) => r.querySelector('tf-button[icon="edit"]')).length, 3, 'admin gets an edit button per row');

  const tiles = [...body.querySelectorAll('#nas-prot tf-stat-card')];
  assert.equal(tiles.length, 4);
  assert.equal(tiles[0].getAttribute('value'), '1/1', 'scrub coverage');
  assert.equal(tiles[1].getAttribute('value'), '0/1', 'snapshot coverage');
  assert.equal(tiles[1].getAttribute('accent'), 'warning');
  assert.equal(tiles[3].getAttribute('value'), '1', 'one failed schedule');
  assert.equal(tiles[3].getAttribute('accent'), 'danger');
  screen.dispose();
});

test('the history filters narrow the finished jobs', async () => {
  const screen = fakeScreen({ tentaNasJobsListRequest: { jobs }, tentaNasSchedulesListRequest: schedules });
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const filters = body.querySelector('#nas-jobs-filters');
  const history = body.querySelector('#nas-jobs-table');
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'errors' } }));
  await flush();
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j3']);
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'snapshot' } }));
  await flush();
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j2']);
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'scrub' } }));
  await flush();
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j3'], 'replace counts as a scrub-family job');
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'all' } }));
  await flush();
  assert.equal(history.rows.length, 2);
  screen.dispose();
});

test('editing the scrub row saves through ScrubScheduleSetRequest and refreshes', async () => {
  let lists = 0;
  const screen = fakeScreen({
    tentaNasJobsListRequest: { jobs: [] },
    tentaNasSchedulesListRequest: () => { lists += 1; return schedules; },
    tentaNasScrubScheduleSetRequest: { ok: true },
  });
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  assert.equal(lists, 1);
  click(tableRows(body.querySelector('#nas-sched-table'))[0].querySelector('tf-button[icon="edit"]'));
  await flush();
  const win = document.querySelector('tf-window');
  assert.ok(win, 'editor opened');
  assert.deepEqual([...win.querySelectorAll('#nas-sched-every option')].map((o) => o.value), ['weekly', 'monthly'], 'scrub cadences only');
  win.querySelector('#nas-sched-enabled').checked = false;
  confirmWindow(win);
  await flush();
  await flush();
  const set = screen.calls.find((c) => c.kind === 'tentaNasScrubScheduleSetRequest');
  assert.ok(set, 'schedule saved');
  assert.deepEqual(set.payload, { name: 'tank', enabled: false, schedule: { every: 'weekly', hour: 2, minute: 0, weekday: 0, day: 1 } });
  assert.equal(lists, 2, 'schedules refreshed after the save');
  screen.dispose();
});

test('the SMART editor sends the switch and both cadences', async () => {
  let done = 0;
  const screen = fakeScreen({ tentaNasSmartScheduleSetRequest: { ok: true } });
  const win = openSmartScheduleEditor(screen, schedules.smart, () => { done += 1; });
  await flush();
  assert.equal(win.querySelector('#nas-smart-short-every').value, 'daily');
  assert.equal(win.querySelector('#nas-smart-long-every').value, 'monthly');
  assert.equal(win.querySelector('#nas-smart-long-day').value, '1');
  const longEvery = win.querySelector('#nas-smart-long-every');
  longEvery.value = 'weekly';
  longEvery.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'weekly' } }));
  assert.equal(win.querySelector('[data-sched="nas-smart-long"] .sched-weekday').hidden, false);
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(screen.calls.length, 1);
  assert.deepEqual(screen.calls[0].payload, {
    enabled: true,
    short: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 },
    long: { every: 'weekly', hour: 4, minute: 0, weekday: 0, day: 1 },
  });
  assert.equal(done, 1);
  screen.dispose();
});
