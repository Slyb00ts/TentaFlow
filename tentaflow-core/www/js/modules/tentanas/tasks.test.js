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

const { drawTasks, openSmartScheduleEditor, jobSubject } = await import('./tasks.js');

const jobs = [
  { jobId: 'j1', kind: 'pool_scrub', subject: 'tank', status: 'running', startedBy: 'admin', startedAt: '2026-09-02 08:00:00', finishedAt: null, progressPct: 40 },
  { jobId: 'j2', kind: 'snapshot_destroy', subject: 'tank/home', status: 'succeeded', startedBy: 'scheduler', startedAt: '2026-09-01 02:00:00', finishedAt: '2026-09-01 02:00:05' },
  { jobId: 'j3', kind: 'pool_replace', subject: 'tank', status: 'failed', startedBy: 'admin', startedAt: '2026-08-30 10:00:00', finishedAt: '2026-08-30 10:05:00', error: 'disk too small' },
];

const hourly = { every: '1h', hour: 0, minute: 0, weekday: 0, day: 1 };
const schedules = {
  rows: [
    { kind: 'scrub', subject: 'tank', enabled: true, schedule: { every: 'weekly', hour: 2, minute: 0, weekday: 0, day: 1 }, lastRunAt: '2026-08-30 02:00:00', lastResult: 'ok', nextRunAt: '2026-09-06 02:00:00' },
    // §5.10: the recurring `zpool trim`, next to the scrub of the same pool.
    { kind: 'trim', subject: 'fast', enabled: true, schedule: { every: 'monthly', hour: 3, minute: 30, weekday: 0, day: 1 }, lastRunAt: null, lastResult: '', nextRunAt: '2026-10-01 03:30:00' },
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

// The four-eyes list is polled next to the jobs; an empty queue with the
// switch off is the state every schedule test wants.
const noApprovals = { approvals: [], settings: { enabled: false, ttlHours: 24, adminCount: 1, byDefault: true } };
// So is the access log (§5.10): nothing audited, nothing collected, so the
// card hides itself and the schedule tests are not about it.
const noAccessLog = {
  events: [], total: 0, shares: [], users: [], operations: [],
  audit: { auditedShares: [], auditedExports: [], unauditedSmbDirect: [], retentionDays: 30, collectorState: 'ok', detail: '', collectedAt: null, eventCount: 0 },
  forward: { enabled: false, syslogTarget: '', webhookUrl: '', includeAccess: false, pending: 0, lastSentAt: null, lastError: '' },
};
const fixtures = (extra = {}) => ({ tentaNasJobsListRequest: { jobs }, tentaNasSchedulesListRequest: schedules, tentaNasSnapshotSchedulesListRequest: snapshotSchedules, tentaNasApprovalsListRequest: noApprovals, tentaNasAccessLogRequest: noAccessLog, ...extra });
const scheduleRows = (body) => [...body.querySelectorAll('#nas-sched-list .job-row')];
// Every toast shown, as `{ kind, text }`. utils.js keeps its container in a
// module variable, and a `screen.dispose()` in an earlier test empties the
// body — so the container can be detached and a DOM query would find
// nothing. Recording the append itself sees every toast wherever it lands.
const toasts = [];
{
  const append = window.Node.prototype.appendChild;
  window.Node.prototype.appendChild = function (child) {
    const kind = /(?:^|\s)toast-(\w+)/.exec(child?.className || '')?.[1];
    if (kind && kind !== 'container') toasts.push({ kind, text: child.textContent });
    return append.call(this, child);
  };
}
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
  assert.deepEqual(screen.calls.map((c) => c.kind).sort(), ['tentaNasAccessLogRequest', 'tentaNasApprovalsListRequest', 'tentaNasJobsListRequest', 'tentaNasSchedulesListRequest', 'tentaNasSnapshotSchedulesListRequest']);
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
  assert.equal(rows.length, 4, 'scrub, trim, snapshot and the folded SMART pair');
  assert.equal(body.querySelector('#nas-sched-count').getAttribute('label'), '4');
  assert.deepEqual(rows.map((r) => r.querySelector('.job-name').textContent), ['Scrub puli tank', 'TRIM puli fast', 'Snapshoty tank/home', 'Testy SMART — wszystkie dyski']);
  assert.match(rows[0].querySelector('.job-sub').textContent, /ostatni: .* · OK/);
  assert.match(rows[1].querySelector('.job-sub').textContent, /jeszcze nie uruchomiony/);
  assert.match(rows[2].querySelector('.job-sub').textContent, /retencja GFS: .* · 12 snapshotów/);
  assert.deepEqual([...rows[3].querySelectorAll('.sched-pill')].map((p) => p.textContent.trim()), ['short: codziennie o 03:00', 'long: co miesiąc, 1. dnia o 04:00'], 'both SMART cadences as pills');
  assert.deepEqual(rows.map((r) => r.querySelector('[data-act="toggle"]').checked), [true, true, false, true]);
  assert.equal(rows.filter((r) => r.querySelector('[data-act="run"]') && r.querySelector('[data-act="edit"]')).length, 4, 'admin gets run + edit per row');

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
  assert.equal(rows.length, 4);
  assert.ok(rows.every((r) => r.querySelector('[data-act="toggle"]').hasAttribute('disabled')));
  assert.equal(body.querySelectorAll('#nas-sched-list [data-act="run"], #nas-sched-list [data-act="edit"]').length, 0);
  assert.equal(body.querySelector('[data-act="new"]'), null);
  screen.dispose();
});

// The owner's rule: a machine id is never shown as a name. The node sends an
// EMPTY `nodeName` when its hostname is unknown (it used to send its 64-hex
// id); the "node" column of the job history must fall back to the Polish
// "Węzeł bez nazwy" label — through `nodeLabel` — and keep the id available
// only as a `title=` tooltip, never as visible text.
test('the job history node column falls back to "Node bez nazwy" for an unnamed node, with the id in a tooltip', async () => {
  const screen = fakeScreen(fixtures());
  screen.currentNode = () => ({ nodeId: 'node-64hex-abcdef', nodeName: '', isLocal: true });
  const body = mount();
  await drawTasks(screen, body);
  await flush();

  const history = body.querySelector('#nas-jobs-table');
  const wrap = document.createElement('div');
  wrap.innerHTML = history.rows[0].node;
  assert.doesNotMatch(wrap.textContent, /node-64hex-abcdef/, 'the id is not visible text');
  assert.match(wrap.textContent, /Node bez nazwy/);
  assert.equal(wrap.querySelector('span').getAttribute('title'), 'node-64hex-abcdef');
  screen.dispose();
});

// Same rule for who started a job: `jobAuthor` translates the system authors
// and turns an unresolvable UUID into "nieznane konto" with the id in a
// tooltip. The running-jobs strip used to print `j.startedBy` raw, so an
// account the server could not resolve leaked its UUID into the open here —
// even though the finished-jobs history and the job-log modal already went
// through `jobAuthor`.
test('a running job started by an unresolved account shows "nieznane konto" with the id in a tooltip', async () => {
  const uuid = '3fa85f64-5717-4562-b3fc-2c963f66afa6';
  const running = [{ jobId: 'j9', kind: 'pool_scrub', subject: 'tank', status: 'running', startedBy: uuid, startedAt: '2026-09-02 08:00:00', finishedAt: null, log: [] }];
  const screen = fakeScreen(fixtures({ tentaNasJobsListRequest: { jobs: running } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();

  const sub = body.querySelector('#nas-jobs-running .job-sub');
  assert.doesNotMatch(sub.textContent, new RegExp(uuid), 'the id is not visible text');
  assert.match(sub.textContent, /nieznane konto/);
  assert.equal(sub.getAttribute('title'), uuid);
  screen.dispose();
});

// A SMART job is spawned on the disk id; the node names it. A name the node
// only remembers (the disk has left its inventory) is marked as last-known,
// and an id that still reached the row is a tooltip, never text — in the
// running list and in the history alike.
test('a job subject is marked when last-known and never shows a disk id', async () => {
  const wwn = 'wwn-0x5000cca27dc7a4c6';
  const list = [
    { jobId: 'r1', kind: 'smart_test', subject: 'sdq', subjectLastKnown: true, status: 'running', startedBy: 'admin', startedAt: '2026-09-02 08:00:00', finishedAt: null, log: [] },
    { jobId: 'r2', kind: 'smart_test', subject: wwn, status: 'running', startedBy: 'admin', startedAt: '2026-09-02 08:00:00', finishedAt: null, log: [] },
    { jobId: 'h1', kind: 'smart_test', subject: 'sdq', subjectLastKnown: true, status: 'succeeded', startedBy: 'admin', startedAt: '2026-09-01 08:00:00', finishedAt: '2026-09-01 08:02:00' },
    { jobId: 'h2', kind: 'smart_test', subject: wwn, status: 'succeeded', startedBy: 'admin', startedAt: '2026-09-01 08:00:00', finishedAt: '2026-09-01 08:02:00' },
    { jobId: 'h3', kind: 'smart_test', subject: 'sdd', status: 'succeeded', startedBy: 'admin', startedAt: '2026-09-01 08:00:00', finishedAt: '2026-09-01 08:02:00' },
  ];
  const screen = fakeScreen(fixtures({ tentaNasJobsListRequest: { jobs: list } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();

  const running = body.querySelector('#nas-jobs-running');
  const name = (id) => running.querySelector(`.job-row[data-job="${id}"] .job-name .mono`);
  assert.equal(name('r1').textContent, 'ostatnio widziany jako sdq');
  assert.equal(name('r2').textContent, '', 'the id is not visible text');
  assert.equal(name('r2').getAttribute('title'), wwn, 'it is the tooltip');
  assert.doesNotMatch(running.textContent, /wwn-/);

  const history = body.querySelector('#nas-jobs-table').rows;
  const task = (id) => history.find((r) => r._job.jobId === id).task;
  assert.match(task('h1'), />ostatnio widziany jako sdq</);
  assert.match(task('h2'), new RegExp(`title="${wwn}"></div>`), 'the id only as the tooltip');
  assert.match(task('h3'), />sdd</, 'a live name is shown as it is');
  assert.doesNotMatch(task('h3'), /ostatnio/);
  screen.dispose();
});

// Only a SMART test's subject can BE a disk id (`disk_id`); every other
// job kind's subject is a pool/dataset/share name, and a real one can take
// the exact same shape as a disk id — a pool named `usb-backup`, a share
// `dev-backups`, a target `pci-store`, a dataset `2024`, `sn-archive`,
// `mmc-media`. Routing every kind through the disk rule (the old single
// `isMachineId`) hid all of those as if they were ids; only `smart_test`
// may use it.
test('a pool/share/target/dataset name is never hidden as an id, even in a disk-id shape', () => {
  const names = ['dev-backups', 'usb-backup', 'pci-store', '2024', 'sn-archive', 'mmc-media'];
  for (const kind of ['pool_scrub', 'snapshot_destroy', 'pool_replace', 'elastic_mover', 'share_create']) {
    for (const name of names) {
      const subject = jobSubject({ kind, subject: name });
      assert.equal(subject.text, name, `${kind} subject "${name}" must stay visible`);
      assert.equal(subject.title, '', `${kind} subject "${name}" is not an id, so no tooltip`);
    }
  }
});

// The same by-id shape IS a disk id when the job is a SMART test — hidden,
// with the id moved to the tooltip — but stays a visible name for every
// other kind, because that kind's subject is never a disk id.
test('a wwn- shaped subject is hidden for a SMART test, but shown for every other job kind', () => {
  const wwn = 'wwn-0x5000c500a1b2c3d4';
  const smart = jobSubject({ kind: 'smart_test', subject: wwn });
  assert.equal(smart.text, '', 'hidden for a SMART test');
  assert.equal(smart.title, wwn);

  const scrub = jobSubject({ kind: 'pool_scrub', subject: wwn });
  assert.equal(scrub.text, wwn, 'shown for a pool subject of the same shape');
  assert.equal(scrub.title, '');
});

// Opaque ids (UUID, a long-enough digit-run GUID, a 64-hex id) are never a
// name a human chose, so every job kind hides them — this is the part of
// the old behaviour that must survive the split.
test('an opaque id is hidden for every job kind, not only smart_test', () => {
  const uuid = '3fa85f64-5717-4562-b3fc-2c963f66afa6';
  for (const kind of ['pool_scrub', 'snapshot_destroy', 'smart_test']) {
    const subject = jobSubject({ kind, subject: uuid });
    assert.equal(subject.text, '', `${kind} subject must hide the opaque id`);
    assert.equal(subject.title, uuid);
  }
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
  assert.deepEqual(filters.filters.map((f) => f.id), ['all', 'errors', 'scrub', 'mover'], 'n15 offers four history filters');
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'scrub' } }));
  await flush();
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j3'], 'replace counts as a scrub-family job');
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'all' } }));
  await flush();
  assert.equal(history.rows.length, 2);
  screen.dispose();
});

test('the mover filter finds the cache drains and nothing else', async () => {
  const moverJobs = [
    ...jobs,
    { jobId: 'j4', kind: 'elastic_mover', subject: 'media', status: 'succeeded', startedBy: 'scheduler', startedAt: '2026-09-02 01:00:00', finishedAt: '2026-09-02 01:04:00' },
  ];
  const screen = fakeScreen(fixtures({ tentaNasJobsListRequest: { jobs: moverJobs } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const filters = body.querySelector('#nas-jobs-filters');
  const history = body.querySelector('#nas-jobs-table');
  assert.equal(filters.filters.find((f) => f.id === 'mover').label, 'Przenoszenie z cache');
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'mover' } }));
  await flush();
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j4']);
  // The row keeps the name the node logs, but no longer claims every run was
  // started by hand: most of them are automatic now.
  assert.match(history.rows[0].task, /Mover · przenoszenie z cache/);
  assert.doesNotMatch(history.rows[0].task, /Uruchom teraz/);
  // The scrub family must not swallow the mover, nor the mover the scrubs.
  filters.dispatchEvent(new window.CustomEvent('change', { detail: { id: 'scrub' } }));
  await flush();
  assert.deepEqual(history.rows.map((r) => r._job.jobId), ['j3']);
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

  flipToggle(scheduleRows(body)[2], true);
  await flush();
  await flush();
  assert.deepEqual(screen.calls.find((c) => c.kind === 'tentaNasSnapshotScheduleSetRequest').payload, {
    scheduleId: 'ss-1', dataset: 'tank/home', enabled: true, recursive: true, schedule: hourly,
    keepFrequent: 0, keepHourly: 24, keepDaily: 7, keepWeekly: 4, keepMonthly: 6, protectDays: 30,
  }, 'the full snapshot schedule goes back with the retention AND the protection intact');

  flipToggle(scheduleRows(body)[3], false);
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

  click(rows[3].querySelector('[data-act="run"]'));
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
  const row = scheduleRows(body)[2];
  assert.match(row.querySelector('.job-sub').textContent, /ochrona 30 dni/);
  assert.match(row.querySelector('.job-sub').textContent, /retencja GFS/, 'retention is still there');
  screen.dispose();
});

test('"Uruchom teraz" on a protected schedule protects the snapshot it takes', async () => {
  const screen = fakeScreen(fixtures({ tentaNasSnapshotCreateRequest: { ok: true } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  click(scheduleRows(body)[2].querySelector('[data-act="run"]'));
  await flush();
  await flush();
  const sent = screen.calls.find((c) => c.kind === 'tentaNasSnapshotCreateRequest');
  assert.equal(sent.payload.protectDays, 30);
  assert.equal(sent.payload.dataset, 'tank/home');
  assert.equal(sent.payload.recursive, true);
  screen.dispose();
});

// The three Elastic cadences (§5.3, E2-10) are rows of the same list. Their own
// fixture, because the shared one's row order is what the tests above index by.
const elasticSchedules = {
  rows: [
    { kind: 'elastic_mover', subject: 'media', enabled: true, schedule: hourly, lastRunAt: null, lastResult: '', nextRunAt: '2026-09-06 02:00:00' },
    { kind: 'elastic_sync', subject: 'media', enabled: false, schedule: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 }, lastRunAt: null, lastResult: '', nextRunAt: null },
  ],
  smart: schedules.smart,
};

test('n15 lists the Elastic cadences and the mover toggle never overwrites its rules', async () => {
  const screen = fakeScreen(fixtures({
    tentaNasSchedulesListRequest: elasticSchedules,
    tentaNasElasticMoverScheduleSetRequest: { ok: true },
  }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const rows = scheduleRows(body);
  // Moving is automatic; a mover row exists only as a saved WINDOW, and the
  // row says what its switch does to the files.
  assert.match(rows[0].textContent, /Okno przenoszenia z cache: media/);
  assert.doesNotMatch(rows[0].textContent, /mover/i);
  assert.match(rows[0].textContent, /poza oknem pliki czekają na cache/);
  assert.match(rows[1].textContent, /SnapRAID sync media/);

  flipToggle(rows[0], false);
  await flush();
  await flush();
  const sent = screen.calls.find((c) => c.kind === 'tentaNasElasticMoverScheduleSetRequest').payload;
  assert.deepEqual(sent, { name: 'media', enabled: false, schedule: hourly });
  // The rules are ABSENT, not zero. A `0` would be stored as a decision to
  // move everything and never trigger — settings the admin never made.
  assert.equal('minAgeSecs' in sent, false);
  assert.equal('cacheMinFreePct' in sent, false);
  assert.equal('coupledSync' in sent, false);
  screen.dispose();
});

test('editing the mover row opens on the array real rules, not on defaults', async () => {
  const screen = fakeScreen(fixtures({
    tentaNasSchedulesListRequest: elasticSchedules,
    tentaNasElasticArrayGetRequest: {
      array: {
        name: 'media',
        mover: { enabled: true, schedule: hourly, minAgeSecs: 1800, cacheMinFreePct: 30, coupledSync: false, configured: true },
      },
    },
    tentaNasElasticMoverScheduleSetRequest: { ok: true },
  }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  click(scheduleRows(body)[0].querySelector('[data-act="edit"]'));
  await flush();
  await flush();
  // The cadence row carries no rules, so the dialog fetches the array first —
  // otherwise saving would write defaults over the admin's settings.
  assert.ok(screen.calls.some((c) => c.kind === 'tentaNasElasticArrayGetRequest'), 'array fetched');
  const win = document.querySelector('tf-window.nas-mover-schedule');
  assert.ok(win, 'mover editor opened');
  confirmWindow(win);
  await flush();
  await flush();
  const sent = screen.calls.find((c) => c.kind === 'tentaNasElasticMoverScheduleSetRequest').payload;
  assert.equal(sent.minAgeSecs, 1800);
  assert.equal(sent.cacheMinFreePct, 30);
  assert.equal(sent.coupledSync, false);
  win.remove();
  screen.dispose();
});

// MAJOR 13 (critic n11-n19, n15): a running job's progress and elapsed-time
// text tick on almost every 3 s poll, and the old `patchHtml` rebuilt the
// WHOLE joined string whenever any of that moved — tearing down every row's
// Cancel button, not just the row that actually changed.
test('a running job whose progress moves keeps its own row and Cancel button; only the bar and text repaint', async () => {
  const runningV1 = [{ jobId: 'j1', kind: 'pool_scrub', subject: 'tank', status: 'running', startedBy: 'admin', startedAt: '2026-09-02 08:00:00', finishedAt: null, progressPct: 10, log: [] }];
  const runningV2 = [{ jobId: 'j1', kind: 'pool_scrub', subject: 'tank', status: 'running', startedBy: 'admin', startedAt: '2026-09-02 08:00:00', finishedAt: null, progressPct: 55, log: ['halfway'] }];
  let poll = 0;
  const screen = fakeScreen(fixtures({ tentaNasJobsListRequest: () => ({ jobs: poll++ === 0 ? runningV1 : runningV2 }) }));
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = mount();
  await drawTasks(screen, body);
  await flush();

  const row1 = body.querySelector('#nas-jobs-running .job-row');
  const cancelBtn1 = row1.querySelector('[data-act="cancel"]');
  const bar1 = row1.querySelector('tf-progress-bar');
  assert.ok(cancelBtn1, 'a scrub job can be cancelled');
  assert.equal(bar1.getAttribute('value'), '10');

  for (const fn of [...scheduled]) await fn();
  await flush();

  const row2 = body.querySelector('#nas-jobs-running .job-row');
  const bar2 = row2.querySelector('tf-progress-bar');
  // Compared as booleans, never as raw DOM nodes: on a MISMATCH, assert's
  // failure-message inspector walks a live DOM element's circular
  // parent/document/style graph and can hang for a very long time.
  assert.equal(row2 === row1, true, 'the row survives the poll even though its progress changed');
  assert.equal(row2.querySelector('[data-act="cancel"]') === cancelBtn1, true, 'the Cancel button is the same node across polls');
  assert.equal(bar2 === bar1, true, 'the progress bar is the same node');
  assert.equal(bar2.getAttribute('value'), '55', 'and its value is repainted');
  assert.match(row2.querySelector('.job-sub').textContent, /halfway/, 'the log tail is repainted too');
  screen.dispose();
});

// MAJOR 13 second half: the schedule list rebuilt wholesale on the 30 s poll,
// so an unrelated row's toggle was destroyed whenever ANY row's "last run"
// text changed underneath it.
test('a schedule whose last run changes keeps its own row; an unrelated row\'s toggle survives too', async () => {
  const schedulesV1 = schedules;
  const schedulesV2 = { ...schedules, rows: schedules.rows.map((r) => (r.kind === 'scrub' ? { ...r, lastRunAt: '2026-09-06 02:00:00', lastResult: 'ok' } : r)) };
  let poll = 0;
  const screen = fakeScreen(fixtures({ tentaNasSchedulesListRequest: () => (poll++ === 0 ? schedulesV1 : schedulesV2) }));
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = mount();
  await drawTasks(screen, body);
  await flush();

  const rows1 = scheduleRows(body);
  const scrubToggle1 = rows1[0].querySelector('[data-act="toggle"]');
  const trimToggle1 = rows1[1].querySelector('[data-act="toggle"]'); // unrelated row, its data never changes
  assert.match(rows1[0].querySelector('.job-sub').textContent, /30\.08\.2026/);

  for (const fn of [...scheduled]) await fn();
  await flush();

  const rows2 = scheduleRows(body);
  // Compared as booleans, never as raw DOM nodes (see the jobs test above).
  assert.equal(rows2[0] === rows1[0], true, 'the changed row keeps its own node');
  assert.equal(rows2[0].querySelector('[data-act="toggle"]') === scrubToggle1, true, 'its toggle is the same node');
  assert.match(rows2[0].querySelector('.job-sub').textContent, /6\.09\.2026/, 'the sub text is repainted with the new run');
  assert.equal(rows2[1] === rows1[1], true, 'a sibling row the poll did not touch is untouched');
  assert.equal(rows2[1].querySelector('[data-act="toggle"]') === trimToggle1, true, 'and its toggle is still the very node it always was');
  screen.dispose();
});

// MAJOR 14 first half / rule 2 (never fabricate): the SMART protection row
// used to say "{t} · OK" the moment ANY short test had ever run, even though
// the wire never actually carries a pass/fail for it.
test('the SMART protection row never claims OK when the server sent no real result', async () => {
  const noResultSchedules = {
    rows: [
      { kind: 'smart_short', subject: '*', enabled: true, schedule: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 }, lastRunAt: '2026-09-02 03:00:00', lastResult: '', nextRunAt: '2026-09-03 03:00:00' },
      { kind: 'smart_long', subject: '*', enabled: true, schedule: { every: 'monthly', hour: 4, minute: 0, weekday: 0, day: 1 }, lastRunAt: null, lastResult: '', nextRunAt: null },
    ],
    smart: { enabled: true, short: { every: 'daily', hour: 3, minute: 0, weekday: 0, day: 1 }, long: { every: 'monthly', hour: 4, minute: 0, weekday: 0, day: 1 }, lastShortAt: '2026-09-02 03:00:00', lastLongAt: null, nextShortAt: '2026-09-03 03:00:00', nextLongAt: null },
  };
  const screen = fakeScreen(fixtures({ tentaNasSchedulesListRequest: noResultSchedules, tentaNasSnapshotSchedulesListRequest: { schedules: [] } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const smartLine = [...body.querySelectorAll('#nas-prot .sr')].find((r) => /SMART short/.test(r.textContent));
  assert.ok(smartLine, 'the SMART protection row is painted');
  assert.doesNotMatch(smartLine.textContent, /OK/, 'an empty server result never renders as OK');
  assert.match(smartLine.textContent, /ostatni/, 'it says only that it ran, not what it found');
  screen.dispose();
});

test('the SMART protection row shows the real failure when the wire ever carries one', async () => {
  // Defensive: today the core always sends an empty result (above), but the
  // row must not lie the other way either if that ever changes.
  const screen = fakeScreen(fixtures()); // shared `schedules` fixture has smart_short lastResult: 'failed'
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const smartLine = [...body.querySelectorAll('#nas-prot .sr')].find((r) => /SMART short/.test(r.textContent));
  assert.match(smartLine.textContent, /błąd/, 'a real failed result is shown, not painted over as OK');
  assert.doesNotMatch(smartLine.textContent, /\bOK\b/);
  screen.dispose();
});

// A6 (critic real-functionality): a refused disk used to abort the whole
// "SMART all disks" batch — the `for` loop's bare `await` threw on the first
// rejection and every later disk was silently never tested.
test('"Uruchom teraz" on SMART all-disks continues past a refused disk in the middle and reports it', async () => {
  const screen = fakeScreen(fixtures({
    tentaNasDisksListRequest: {
      disks: [
        { diskId: 'sda', name: 'sda', smartAvailable: true },
        { diskId: 'sdb', name: 'sdb', smartAvailable: true },
        { diskId: 'sdc', name: 'sdc', smartAvailable: true },
      ],
    },
    tentaNasDiskSmartTestRequest: (payload) => {
      if (payload.diskId === 'sdb') return Promise.reject(new Error('dysk zajęty'));
      return Promise.resolve({ ok: true });
    },
  }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  click(scheduleRows(body)[3].querySelector('[data-act="run"]'));
  await flush();
  await flush();
  await flush();

  const tests = screen.calls.filter((c) => c.kind === 'tentaNasDiskSmartTestRequest').map((c) => c.payload.diskId);
  assert.deepEqual(tests, ['sda', 'sdb', 'sdc'], 'sdc is still tried after sdb refuses — the batch does not stop');
  // The refusal is reported in a translated sentence around the node's
  // per-disk reasons — not the bare "sdb: …" list.
  const warned = toasts.filter((t) => t.kind === 'warning').map((t) => t.text).join('\n');
  assert.match(warned, /Test SMART nie ruszył na 1 dysku: sdb: dysk zajęty/);
  screen.dispose();
});

// M1: a privilege/credential error (a rejected sudo password, an unarmed
// channel, a helper/core version mismatch — anything `broker_error` maps to
// `ProtocolErrorCode::NotAvailable`) is the OPPOSITE of a per-disk refusal:
// it will fail identically for every remaining disk, so it must stop the
// whole batch at once rather than replay the same password against sudo
// once per disk (pam_faillock can then lock the account the core runs as).
test('"Uruchom teraz" on SMART all-disks stops at once on a privilege/credential error and sends exactly one request', async () => {
  const screen = fakeScreen(fixtures({
    tentaNasDisksListRequest: {
      disks: [
        { diskId: 'sda', name: 'sda', smartAvailable: true },
        { diskId: 'sdb', name: 'sdb', smartAvailable: true },
        { diskId: 'sdc', name: 'sdc', smartAvailable: true },
      ],
    },
    tentaNasDiskSmartTestRequest: () => Promise.reject(Object.assign(new Error('Kanał uprawnień systemowych nie jest dostępny (sudo rejected the password)'), { code: 'NotAvailable' })),
  }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  click(scheduleRows(body)[3].querySelector('[data-act="run"]'));
  await flush();
  await flush();
  await flush();

  const tests = screen.calls.filter((c) => c.kind === 'tentaNasDiskSmartTestRequest');
  assert.equal(tests.length, 1, 'sda fails with a credential error — sdb and sdc are never sent');
  assert.equal(tests[0].payload.diskId, 'sda');
  screen.dispose();
});

// The jobs list polls every 3 s and the schedules every 30 s. Rebuilding
// either on an unchanged answer throws away the row the admin is reaching for.
test('an unchanged poll leaves the running jobs, the schedules and the strip alone', async () => {
  const screen = fakeScreen(fixtures());
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const running = [...body.querySelectorAll('#nas-jobs-running .job-row')];
  const rows = scheduleRows(body);
  const strip = body.querySelector('#nas-prot .sr');
  assert.ok(running.length, 'a job is running');
  assert.ok(rows.length, 'schedules are listed');
  assert.ok(strip, 'the protection strip is painted');

  for (const fn of [...scheduled]) await fn();
  await flush();
  [...body.querySelectorAll('#nas-jobs-running .job-row')].forEach((el, i) => assert.equal(el === running[i], true, `running job ${i} survives the poll`));
  scheduleRows(body).forEach((el, i) => assert.equal(el === rows[i], true, `schedule row ${i} survives the poll`));
  assert.equal(body.querySelector('#nas-prot .sr') === strip, true, 'the protection strip survives the poll');
  screen.dispose();
});
