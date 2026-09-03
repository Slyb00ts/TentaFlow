// =============================================================================
// File: modules/tentanas/tasks.test.js
// Description: The Tasks tab against a fake screen: running jobs and history
// split from JobsListResponse with the history filters, the protection
// status rows and the schedule list from SchedulesListResponse, the row
// toggle resending each schedule kind with `enabled` flipped, "Uruchom
// teraz" starting a scrub or a SMART batch through sudo, a scrub row edit
// sending ScrubScheduleSetRequest, the SMART editor sending both cadences,
// and the §5.10 snapshot protection showing in the strip, in the schedule row
// and in what "Uruchom teraz" sends. Runs under happy-dom.
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

const hourly = { every: '1h', hour: 0, minute: 0, weekday: 0, day: 1 };
const schedules = {
  rows: [
    { kind: 'scrub', subject: 'tank', enabled: true, schedule: { every: 'weekly', hour: 2, minute: 0, weekday: 0, day: 1 }, lastRunAt: '2026-08-30 02:00:00', lastResult: 'ok', nextRunAt: '2026-09-06 02:00:00' },
    { kind: 'snapshot', subject: 'tank/home', enabled: false, schedule: hourly, lastRunAt: null, lastResult: '', nextRunAt: null },
    { kind: 'smart_short', subject: '*', enabled: true, schedule: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 }, lastRunAt: '2026-09-02 03:00:00', lastResult: 'failed', nextRunAt: '2026-09-03 03:00:00' },
    { kind: 'smart_long', subject: '*', enabled: true, schedule: { every: 'monthly', hour: 4, minute: 0, weekday: 0, day: 1 }, lastRunAt: null, lastResult: '', nextRunAt: null },
  ],
  smart: { enabled: true, short: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 }, long: { every: 'monthly', hour: 4, minute: 0, weekday: 0, day: 1 }, lastShortAt: '2026-09-02 03:00:00', lastLongAt: null, nextShortAt: '2026-09-03 03:00:00', nextLongAt: null },
};
const snapshotSchedules = {
  schedules: [{ scheduleId: 'ss-1', dataset: 'tank/home', enabled: false, recursive: true, schedule: hourly, keepFrequent: 0, keepHourly: 24, keepDaily: 7, keepWeekly: 4, keepMonthly: 6, protectDays: 30, lastRunAt: null, nextRunAt: null, snapshotCount: 12 }],
};

function mount() {
  const body = document.createElement('div');
  document.body.appendChild(body);
  return body;
}

const fixtures = (extra = {}) => ({ tentaNasJobsListRequest: { jobs }, tentaNasSchedulesListRequest: schedules, tentaNasSnapshotSchedulesListRequest: snapshotSchedules, ...extra });
const scheduleRows = (body) => [...body.querySelectorAll('#nas-sched-list .job-row')];
const flipToggle = (row, checked) => {
  const t = row.querySelector('[data-act="toggle"]');
  t.checked = checked;
  t.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked } }));
};

test('splits running jobs from history and paints the protection rows and the schedule list', async () => {
  const screen = fakeScreen(fixtures());
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  assert.deepEqual(screen.calls.map((c) => c.kind).sort(), ['tentaNasJobsListRequest', 'tentaNasSchedulesListRequest', 'tentaNasSnapshotSchedulesListRequest']);
  assert.equal(screen.calls.find((c) => c.kind === 'tentaNasJobsListRequest').payload.limit, 100);

  assert.equal(body.querySelectorAll('#nas-jobs-running .job-row').length, 1);
  assert.equal(body.querySelector('#nas-jobs-count').getAttribute('label'), '1');
  assert.match(body.querySelector('.section-card .hint').textContent, /odświeżany co 3 s/);
  const history = body.querySelector('#nas-jobs-table');
  assert.equal(history.rows.length, 2, 'finished jobs only');
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j2', 'j3']);
  assert.match(history.rows[1].result, /disk too small/);
  assert.match(history.rows[0].node, /orion/);

  const rows = scheduleRows(body);
  assert.equal(rows.length, 3, 'scrub, snapshot and the folded SMART pair');
  assert.equal(body.querySelector('#nas-sched-count').getAttribute('label'), '3');
  assert.deepEqual(rows.map((r) => r.querySelector('.job-name').textContent), ['Scrub puli tank', 'Snapshoty tank/home', 'Testy SMART — wszystkie dyski']);
  assert.match(rows[0].querySelector('.job-sub').textContent, /ostatni: .* · OK/);
  assert.match(rows[1].querySelector('.job-sub').textContent, /retencja GFS: .* · 12 snapshotów/);
  assert.deepEqual([...rows[2].querySelectorAll('.sched-pill')].map((p) => p.textContent.trim()), ['short: codziennie o 03:00', 'long: co miesiąc, 1. dnia o 04:00'], 'both SMART cadences as pills');
  assert.deepEqual(rows.map((r) => r.querySelector('[data-act="toggle"]').checked), [true, false, true]);
  assert.equal(rows.filter((r) => r.querySelector('[data-act="run"]') && r.querySelector('[data-act="edit"]')).length, 3, 'admin gets run + edit per row');

  const prot = [...body.querySelectorAll('#nas-prot .sr')].map((r) => r.textContent.replace(/\s+/g, ' ').trim());
  assert.equal(prot.length, 3);
  assert.match(prot[0], /^Snapshoty tank\/home/);
  assert.equal(body.querySelector('#nas-prot .sr tf-chip').getAttribute('label'), 'wyłączony', 'the disabled snapshot schedule shows as off');
  assert.match(prot[1], /^Scrub tank/);
  assert.match(prot[2], /^SMART short \(wszystkie\)/);
  screen.dispose();
});

test('a viewer sees the toggles disabled and no run/edit actions', async () => {
  const screen = fakeScreen(fixtures(), { admin: false });
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const rows = scheduleRows(body);
  assert.equal(rows.length, 3);
  assert.ok(rows.every((r) => r.querySelector('[data-act="toggle"]').hasAttribute('disabled')));
  assert.equal(body.querySelectorAll('#nas-sched-list [data-act="run"], #nas-sched-list [data-act="edit"]').length, 0);
  assert.equal(body.querySelector('[data-act="new"]'), null);
  screen.dispose();
});

test('the history filters narrow the finished jobs', async () => {
  const screen = fakeScreen(fixtures());
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const filters = body.querySelector('#nas-jobs-filters');
  const history = body.querySelector('#nas-jobs-table');
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'errors' } }));
  await flush();
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j3']);
  assert.deepEqual(filters.filters.map((f) => f.id), ['all', 'errors', 'scrub'], 'n15 offers three history filters');
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'scrub' } }));
  await flush();
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j3'], 'replace counts as a scrub-family job');
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'all' } }));
  await flush();
  assert.equal(history.rows.length, 2);
  screen.dispose();
});

test('the row toggle resends each schedule kind with only enabled flipped and refreshes', async () => {
  let lists = 0;
  const screen = fakeScreen(fixtures({
    tentaNasSchedulesListRequest: () => { lists += 1; return schedules; },
    tentaNasScrubScheduleSetRequest: { ok: true },
    tentaNasSnapshotScheduleSetRequest: { ok: true },
    tentaNasSmartScheduleSetRequest: { ok: true },
  }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  assert.equal(lists, 1);
  const rows = scheduleRows(body);

  flipToggle(rows[0], false);
  await flush();
  await flush();
  assert.deepEqual(screen.calls.find((c) => c.kind === 'tentaNasScrubScheduleSetRequest').payload, {
    name: 'tank', enabled: false, schedule: { every: 'weekly', hour: 2, minute: 0, weekday: 0, day: 1 },
  });
  assert.equal(lists, 2, 'schedules refreshed after the save');

  flipToggle(scheduleRows(body)[1], true);
  await flush();
  await flush();
  assert.deepEqual(screen.calls.find((c) => c.kind === 'tentaNasSnapshotScheduleSetRequest').payload, {
    scheduleId: 'ss-1', dataset: 'tank/home', enabled: true, recursive: true, schedule: hourly,
    keepFrequent: 0, keepHourly: 24, keepDaily: 7, keepWeekly: 4, keepMonthly: 6, protectDays: 30,
  }, 'the full snapshot schedule goes back with the retention AND the protection intact');

  flipToggle(scheduleRows(body)[2], false);
  await flush();
  await flush();
  assert.deepEqual(screen.calls.find((c) => c.kind === 'tentaNasSmartScheduleSetRequest').payload, {
    enabled: false,
    short: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 },
    long: { every: 'monthly', hour: 4, minute: 0, weekday: 0, day: 1 },
  });
  assert.equal(lists, 4);
  screen.dispose();
});

test('"Uruchom teraz" starts a scrub through sudo and a SMART short test on every SMART-capable disk', async () => {
  const screen = fakeScreen(fixtures({
    tentaNasPoolScrubRequest: { job: { jobId: 'job-s', kind: 'pool_scrub', status: 'running' } },
    tentaNasDisksListRequest: { disks: [{ diskId: 'sda', name: 'sda', smartAvailable: true }, { diskId: 'sdb', name: 'sdb', smartAvailable: false }, { diskId: 'nvme0n1', name: 'nvme0n1', smartAvailable: true }], telemetry: null },
    tentaNasDiskSmartTestRequest: { ok: true },
  }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const rows = scheduleRows(body);
  assert.equal(rows[0].querySelector('[data-act="run"]').getAttribute('title'), 'Uruchom teraz');

  click(rows[0].querySelector('[data-act="run"]'));
  await flush();
  await flush();
  const scrub = screen.calls.find((c) => c.kind === 'tentaNasPoolScrubRequest');
  assert.deepEqual(scrub.payload, { name: 'tank', action: 'start', sudoPassword: 'hunter2' });
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['job-s'], 'the scrub job opens its log');

  click(rows[2].querySelector('[data-act="run"]'));
  await flush();
  await flush();
  await flush();
  const tests = screen.calls.filter((c) => c.kind === 'tentaNasDiskSmartTestRequest').map((c) => c.payload);
  assert.deepEqual(tests, [
    { diskId: 'sda', kind: 'short', sudoPassword: 'hunter2' },
    { diskId: 'nvme0n1', kind: 'short', sudoPassword: 'hunter2' },
  ], 'one short test per disk that reports SMART');
  screen.dispose();
});

test('editing the scrub row saves through ScrubScheduleSetRequest and refreshes', async () => {
  let lists = 0;
  const screen = fakeScreen(fixtures({
    tentaNasJobsListRequest: { jobs: [] },
    tentaNasSchedulesListRequest: () => { lists += 1; return schedules; },
    tentaNasScrubScheduleSetRequest: { ok: true },
  }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  assert.equal(lists, 1);
  click(scheduleRows(body)[0].querySelector('[data-act="edit"]'));
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
  assert.equal(win.querySelector('[data-sched="nas-smart-long"] [data-sched-part="weekday"]').hidden, false);
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

test('a protected snapshot schedule says so in the protection strip and in its row (n15)', async () => {
  const screen = fakeScreen(fixtures());
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const strip = body.querySelector('#nas-prot').textContent;
  assert.match(strip, /Snapshoty tank\/home/);
  assert.match(strip, /ochrona 30 dni/, 'the protection period sits next to the snapshot schedule');
  const row = scheduleRows(body)[1];
  assert.match(row.querySelector('.job-sub').textContent, /ochrona 30 dni/);
  assert.match(row.querySelector('.job-sub').textContent, /retencja GFS/, 'retention is still there');
  screen.dispose();
});

test('"Uruchom teraz" on a protected schedule protects the snapshot it takes', async () => {
  const screen = fakeScreen(fixtures({ tentaNasSnapshotCreateRequest: { ok: true } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  click(scheduleRows(body)[1].querySelector('[data-act="run"]'));
  await flush();
  await flush();
  const sent = screen.calls.find((c) => c.kind === 'tentaNasSnapshotCreateRequest');
  assert.equal(sent.payload.protectDays, 30);
  assert.equal(sent.payload.dataset, 'tank/home');
  assert.equal(sent.payload.recursive, true);
  screen.dispose();
});
