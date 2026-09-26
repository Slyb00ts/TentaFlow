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

import { fakeScreen, flush, click, confirmWindow, window, I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawTasks, openSmartScheduleEditor, jobSubject, jobRowSkeleton, paintJobRow, scheduleOutcome } = await import('./tasks.js');
const { jobCanCancel } = await import('./format.js');

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
// "Węzeł bez nazwy" label — through `nodeLabel` — and the id is neither
// visible text nor a `title=` tooltip (no ids anywhere in the GUI).
test('the job history node column falls back to "Node bez nazwy" for an unnamed node, with no id even as a tooltip', async () => {
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
  assert.equal(wrap.querySelector('span').getAttribute('title'), null, 'nor the tooltip');
  screen.dispose();
});

// Same rule for who started a job: `jobAuthor` translates the system authors
// and turns an unresolvable UUID into "nieznane konto", with the id nowhere. The running-jobs strip used to print `j.startedBy` raw, so an
// account the server could not resolve leaked its UUID into the open here —
// even though the finished-jobs history and the job-log modal already went
// through `jobAuthor`.
test('a running job started by an unresolved account shows "nieznane konto" and never its id', async () => {
  const uuid = '3fa85f64-5717-4562-b3fc-2c963f66afa6';
  const running = [{ jobId: 'j9', kind: 'pool_scrub', subject: 'tank', status: 'running', startedBy: uuid, startedAt: '2026-09-02 08:00:00', finishedAt: null, log: [] }];
  const screen = fakeScreen(fixtures({ tentaNasJobsListRequest: { jobs: running } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();

  const sub = body.querySelector('#nas-jobs-running .job-sub');
  assert.doesNotMatch(sub.outerHTML, new RegExp(uuid), 'the id is neither text nor a tooltip');
  assert.match(sub.textContent, /nieznane konto/);
  screen.dispose();
});

// A SMART job is spawned on the disk id; the node names it. A name the node
// only remembers (the disk has left its inventory) is marked as last-known,
// and an id that still reached the row reads "nieznany dysk" — never the id,
// not even as a tooltip — in the running list and in the history alike.
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
  const name = (id) => running.querySelector(`.job-row[data-job="${id}"] .job-name > span`);
  assert.equal(name('r1').textContent, 'ostatnio widziany jako sdq');
  assert.equal(name('r2').textContent, 'nieznany dysk', 'the id is not visible text');
  assert.doesNotMatch(running.innerHTML, /wwn-/, 'nor a tooltip');

  const history = body.querySelector('#nas-jobs-table').rows;
  const task = (id) => history.find((r) => r._job.jobId === id).task;
  assert.match(task('h1'), />ostatnio widziany jako sdq</);
  assert.match(task('h2'), />nieznany dysk</);
  assert.doesNotMatch(task('h2'), /wwn-/, 'the id is nowhere in the cell');
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
      assert.deepEqual(subject, { text: name, words: false }, `${kind} subject "${name}" must stay visible`);
    }
  }
});

// The same by-id shape IS a disk id when the job is a SMART test — hidden,
// the subject reading "nieznany dysk" — but stays a visible name for every
// other kind, because that kind's subject is never a disk id.
test('a wwn- shaped subject is hidden for a SMART test, but shown for every other job kind', () => {
  const wwn = 'wwn-0x5000c500a1b2c3d4';
  assert.deepEqual(jobSubject({ kind: 'smart_test', subject: wwn }), { text: 'nieznany dysk', words: true }, 'hidden for a SMART test');
  assert.deepEqual(jobSubject({ kind: 'pool_scrub', subject: wwn }), { text: wwn, words: false }, 'shown for a pool subject of the same shape');
});

// Critic wave 5, MINOR 10: "nieznany dysk" / "ostatnio sdk" are words, not
// a device name — the row sets them in the normal face, a real name in mono.
test('a worded job subject is not set in monospace; a real name is', () => {
  const row = (j) => { const el = document.createElement('div'); el.innerHTML = jobRowSkeleton(j); return el.querySelector('.job-name span'); };
  const unknown = row({ jobId: 'j1', kind: 'smart_test', subject: 'wwn-0x5000c500a1b2c3d4', status: 'running' });
  assert.equal(unknown.textContent, 'nieznany dysk');
  assert.equal(unknown.classList.contains('mono'), false);
  const remembered = row({ jobId: 'j2', kind: 'smart_test', subject: 'sdk', subjectLastKnown: true, status: 'running' });
  assert.equal(remembered.classList.contains('mono'), false);
  const named = row({ jobId: 'j3', kind: 'pool_scrub', subject: 'tank', status: 'running' });
  assert.equal(named.classList.contains('mono'), true);
});

// Opaque ids (UUID, a long-enough digit-run GUID, a 64-hex id) are never a
// name a human chose, so every job kind hides them — this is the part of
// the old behaviour that must survive the split.
test('an opaque id is hidden for every job kind, not only smart_test', () => {
  const uuid = '3fa85f64-5717-4562-b3fc-2c963f66afa6';
  for (const kind of ['pool_scrub', 'snapshot_destroy', 'smart_test']) {
    const subject = jobSubject({ kind, subject: uuid });
    assert.deepEqual(subject, { text: kind === 'smart_test' ? 'nieznany dysk' : '', words: true }, `${kind} subject must hide the opaque id, tooltip included`);
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
    tentaNasDiskSmartTestBatchRequest: { job: { jobId: 'job-b', kind: 'smart_test_batch', status: 'running', disks: [] } },
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
  const batches = screen.calls.filter((c) => c.kind === 'tentaNasDiskSmartTestBatchRequest').map((c) => c.payload);
  assert.deepEqual(batches, [
    { diskIds: ['sda', 'nvme0n1'], kind: 'short', sudoPassword: 'hunter2' },
  ], 'ONE request for every disk that reports SMART, with one password');
  assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasDiskSmartTestRequest').length, 0, 'no request per disk');
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['job-s', 'job-b'], 'the one SMART job opens its log');
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

// Wave 9b: "SMART all disks" is ONE job on the node with a line per disk.
// The per-disk rules — go on past a refused disk, stop at the first
// privilege error without replaying the password — are the node's now
// (`jobs::smart_self_test_batch`, tested there); the screen shows each line
// by name with its own worded state, and a refused credential ends the one
// request with the one error.
test('"Uruchom teraz" on SMART all-disks shows the job\'s disk lines, each by name with its worded state', async () => {
  const batch = {
    jobId: 'job-b', kind: 'smart_test_batch', subject: 'short|sda, sdb, sdc', status: 'running', progressPct: 30,
    startedBy: 'admin', startedAt: '2026-09-26 10:00:00', log: [],
    disks: [
      { name: 'sda', lastKnown: false, state: 'running', progressPct: 60, reasons: [] },
      { name: 'sdb', lastKnown: false, state: 'refused', progressPct: null, reasons: [{ code: 'self_test_running', params: {} }] },
      { name: 'sdq', lastKnown: true, state: 'pending', progressPct: null, reasons: [] },
    ],
  };
  const screen = fakeScreen(fixtures({ tentaNasJobsListRequest: { jobs: [batch] } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const lines = [...body.querySelectorAll('#nas-jobs-running .job-disk')];
  assert.equal(lines.length, 3, 'one line per disk');
  assert.match(body.querySelector('#nas-jobs-running .job-name').textContent, /SMART short\s+sda, sdb, sdc/, 'a chosen set is named by its disks');
  assert.equal(lines[0].querySelector('[data-role="name"]').textContent, 'sda');
  assert.equal(lines[0].querySelector('[data-role="state"]').getAttribute('label'), 'w toku 60%');
  assert.equal(lines[1].querySelector('[data-role="detail"]').textContent, 'na tym dysku trwa już test SMART');
  assert.equal(lines[1].querySelector('[data-role="state"]').getAttribute('status'), 'warn');
  assert.equal(lines[2].querySelector('[data-role="name"]').textContent, 'ostatnio widziany jako sdq', 'a remembered name is marked');
  assert.doesNotMatch(body.querySelector('#nas-jobs-running').textContent, /wwn-|sn-|tentanas\./, 'no id and no raw key');
  screen.dispose();
});

test('a batch line that moves is patched in place: the other lines keep their elements', async () => {
  const line = (name, state, pct = null) => ({ name, lastKnown: false, state, progressPct: pct, reasons: [] });
  let jobs = [{ jobId: 'job-b', kind: 'smart_test_batch', subject: 'sda, sdb', status: 'running', progressPct: 10, startedBy: 'admin', startedAt: '2026-09-26 10:00:00', log: [], disks: [line('sda', 'running', 10), line('sdb', 'pending')] }];
  const screen = fakeScreen(fixtures({ tentaNasJobsListRequest: () => ({ jobs }) }));
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const before = [...body.querySelectorAll('#nas-jobs-running .job-disk')];
  jobs = [{ ...jobs[0], progressPct: 40, disks: [line('sda', 'running', 40), line('sdb', 'pending')] }];
  for (const fn of scheduled.splice(0)) await fn();
  await flush();
  const after = [...body.querySelectorAll('#nas-jobs-running .job-disk')];
  assert.equal(after[0], before[0], 'the moving line is the same element');
  assert.equal(after[1], before[1], 'the unchanged line is the same element');
  assert.equal(after[0].querySelector('[data-role="state"]').getAttribute('label'), 'w toku 40%');
  screen.dispose();
});

test('a finished batch reads "N/M OK" in the history and names the disks that did not pass', async () => {
  const done = {
    jobId: 'job-h', kind: 'smart_test_batch', subject: 'short|all', status: 'failed', startedBy: 'scheduler',
    startedAt: '2026-09-26 10:00:00', finishedAt: '2026-09-26 10:04:00', log: [], error: 'the self-test did not pass on 1 of 2 disks',
    disks: [
      { name: 'sda', lastKnown: false, state: 'passed', progressPct: null, reasons: [] },
      { name: 'sdb', lastKnown: false, state: 'failed', progressPct: null, reasons: [{ code: 'test_failed', params: { detail: 'Completed: read failure' } }] },
    ],
  };
  const screen = fakeScreen(fixtures({ tentaNasJobsListRequest: { jobs: [done] } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const row = body.querySelector('#nas-jobs-table').rows.find((r) => r._job.jobId === 'job-h');
  assert.match(row.result, /label="1\/2 OK"/);
  // n15: "SMART short — wszystkie · 18/18 OK" — the test kind in the label,
  // "all disks" as the subject, never the list of every kernel name.
  assert.match(row.task, /SMART short/);
  assert.match(row.task, /— wszystkie dyski/);
  assert.match(row.result, /sdb: nie przeszedł — dysk nie przeszedł testu: Completed: read failure/);
  assert.doesNotMatch(row.result, /did not pass on/, 'the node\'s English summary is not the text');
  screen.dispose();
});

test('a refused credential ends the one SMART request with its one error and no job', async () => {
  const screen = fakeScreen(fixtures({
    tentaNasDisksListRequest: {
      disks: [
        { diskId: 'sda', name: 'sda', smartAvailable: true },
        { diskId: 'sdb', name: 'sdb', smartAvailable: true },
      ],
    },
    tentaNasDiskSmartTestBatchRequest: () => Promise.reject(Object.assign(new Error('Kanał uprawnień systemowych nie jest dostępny (sudo rejected the password)'), { code: 'NotAvailable' })),
  }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  click(scheduleRows(body)[3].querySelector('[data-act="run"]'));
  await flush();
  await flush();
  await flush();
  const sent = screen.calls.filter((c) => c.kind === 'tentaNasDiskSmartTestBatchRequest');
  assert.equal(sent.length, 1, 'exactly one request, never one per disk');
  assert.deepEqual(sent[0].payload.diskIds, ['sda', 'sdb']);
  assert.deepEqual(screen.jobLogs, [], 'no job log opens for a refused request');
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

// The backlog after the 2026-09-21 critic pass: tasks.js used to carry its
// own `runningJobSkeleton`/`paintRunningJob` because tentanas.js was
// off-limits, while n02 kept a second copy (`jobRowHtml`/`wireJobRows`).
// `jobRowSkeleton`/`paintJobRow` are now exported from here and imported by
// tentanas.js — this proves the tab's own running-jobs row IS that shared
// implementation, not a look-alike copy, by building the very same job
// through it directly and comparing the result to what the tab painted.
test('n15\'s running-jobs row is the shared job-row implementation n02 also imports from here', async () => {
  const screen = fakeScreen(fixtures());
  const body = mount();
  // Its own detached host, appended to the document like every other row
  // host in this file: the tf-chip/tf-progress-bar custom elements only
  // upgrade and render their insides once connected, so a comparison against
  // an unattached fragment would fault on their text, not on the row.
  const host = mount();
  try {
    await drawTasks(screen, body);
    await flush();
    const row = body.querySelector('#nas-jobs-running .job-row');
    const runningJob = jobs.find((j) => j.status === 'running');
    assert.ok(row && runningJob, 'a running job is on screen');

    host.innerHTML = jobRowSkeleton(runningJob);
    const built = host.firstElementChild;
    paintJobRow(built, runningJob);

    assert.equal(built.querySelector('.job-name').textContent, row.querySelector('.job-name').textContent, 'same name/subject text');
    assert.equal(!!built.querySelector('[data-act="cancel"]'), !!row.querySelector('[data-act="cancel"]'), 'same cancellability');
    assert.equal(built.querySelector('[data-role="status"]').getAttribute('status'), row.querySelector('[data-role="status"]').getAttribute('status'), 'same status tone');
    assert.equal(built.querySelector('[data-role="status"]').getAttribute('label'), row.querySelector('[data-role="status"]').getAttribute('label'), 'same status label');
    assert.equal(built.querySelector('.job-sub').textContent, row.querySelector('.job-sub').textContent, 'same author/started-at text');
    assert.equal(built.querySelector('tf-progress-bar').getAttribute('value'), row.querySelector('tf-progress-bar').getAttribute('value'), 'same progress value');
  } finally {
    screen.dispose();
  }
});

// MINOR 8 (critic-round2-wave1-2026-09-22.md): `paintJobRow` used to flip the
// icon's `running` class with `ico.classList.toggle('running', running)` on
// EVERY poll, running or not. In happy-dom (unlike a spec-compliant
// `DOMTokenList.toggle`, which is a no-op when the token already matches),
// that unconditionally rewrites the `class` attribute and fires a mutation
// record even when nothing changed — verified directly against happy-dom's
// own `classList.toggle`, which does mutate on a redundant call. `setClass`
// (dom-patch.js) checks `classList.contains` first and skips the call
// entirely when the state already matches, so an unchanged poll should
// mutate nothing on the icon.
test('an unchanged poll leaves the running-job icon untouched (MINOR 8)', async () => {
  const runningJob = jobs.find((j) => j.status === 'running');
  const host = mount();
  host.innerHTML = jobRowSkeleton(runningJob);
  const row = host.firstElementChild;
  paintJobRow(row, runningJob); // first paint: sets the 'running' class.
  const ico = row.querySelector('[data-role="ico"]');

  let mutations = 0;
  const obs = new window.MutationObserver((recs) => { mutations += recs.length; });
  obs.observe(ico, { attributes: true, attributeFilter: ['class'] });
  paintJobRow(row, runningJob); // second paint, same status: nothing to change.
  await flush();
  obs.disconnect();

  assert.equal(mutations, 0, 'the icon class attribute was not touched by an unchanged poll');
  assert.ok(ico.classList.contains('running'), 'the icon is still marked running');
});

// ===== B2 (critic-mockups-n11-n19-2026-09-21.md): the scheduler stores its
// own sentence, `started job <uuid>` / `failed to start: <error>`
// (scheduler.rs). The row used to print `tentanas.schedules.result_started
// job <uuid>`; it now shows the named job's real, translated status and never
// the uuid or a key.
const UUID_A = '0192f1c2-7a3b-7c11-9d2e-3f4a5b6c7d8e';
const UUID_B = '0192f1c2-7a3b-7c11-9d2e-3f4a5b6c7d8f';
const RAW_KEY = /tentanas\.|schedules\.|jobs\.|started job|[0-9a-f]{8}-[0-9a-f]{4}-/;

test('a scheduled scrub row shows its job\'s real outcome, never the uuid or a raw key (B2)', async () => {
  const listed = [{ jobId: UUID_A, kind: 'pool_scrub', subject: 'tank', status: 'failed', startedBy: 'scheduler', startedAt: '2026-08-30 02:00:00', finishedAt: '2026-08-30 03:00:00', error: 'checksum errors' }];
  const sched = { ...schedules, rows: [
    { ...schedules.rows[0], lastResult: `started job ${UUID_A}` },
    // The trim's job is older than the jobs list: fetched once by id.
    { ...schedules.rows[1], lastRunAt: '2026-08-01 03:30:00', lastResult: `started job ${UUID_B}` },
  ] };
  const gets = [];
  const screen = fakeScreen(fixtures({
    tentaNasJobsListRequest: { jobs: listed },
    tentaNasSchedulesListRequest: sched,
    tentaNasJobGetRequest: (p) => { gets.push(p.jobId); return { job: { jobId: p.jobId, kind: 'pool_trim', subject: 'fast', status: 'succeeded' } }; },
  }));
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  try {
    const body = mount();
    await drawTasks(screen, body);
    await flush(); await flush();
    const [scrubRow, trimRow] = scheduleRows(body);
    const scrubSub = scrubRow.querySelector('[data-role="sub"]').textContent;
    const trimSub = trimRow.querySelector('[data-role="sub"]').textContent;
    assert.match(scrubSub, /ostatni: .* · błąd/);
    assert.match(trimSub, /ostatni: .* · OK/);
    assert.doesNotMatch(scrubSub + trimSub, RAW_KEY);
    assert.deepEqual(gets, [UUID_B], 'only the job missing from the list is fetched, once');
    // The failed run finally reaches the protection strip.
    assert.match(body.querySelector('#nas-prot').textContent, /błąd · ostatni/);

    for (const fn of [...scheduled]) await fn();
    await flush();
    assert.deepEqual(gets, [UUID_B], 'a poll never asks for the same job again');
  } finally {
    screen.dispose();
  }
});

test('a job that cannot be resolved shows only when it ran; a refused start reads as failed with the reason in a tooltip (B2)', async () => {
  const sched = { ...schedules, rows: [
    { ...schedules.rows[0], lastResult: `started job ${UUID_A}` },
    { ...schedules.rows[1], lastRunAt: '2026-08-01 03:30:00', lastResult: 'failed to start: pool is busy' },
  ] };
  const screen = fakeScreen(fixtures({
    tentaNasJobsListRequest: { jobs: [] },
    tentaNasSchedulesListRequest: sched,
    tentaNasJobGetRequest: () => { throw new Error('job not found'); },
  }));
  screen.later = () => {};
  try {
    const body = mount();
    await drawTasks(screen, body);
    await flush(); await flush();
    const [scrubRow, trimRow] = scheduleRows(body);
    const scrubSub = scrubRow.querySelector('[data-role="sub"]');
    const trimSub = trimRow.querySelector('[data-role="sub"]');
    assert.match(scrubSub.textContent, /^ostatni: \S/);
    assert.doesNotMatch(scrubSub.textContent, /·/, 'no invented outcome for a job nobody can read');
    assert.match(trimSub.textContent, /· błąd$/);
    assert.equal(trimSub.getAttribute('title'), 'pool is busy');
    assert.doesNotMatch(scrubSub.textContent + trimSub.textContent, RAW_KEY);
  } finally {
    screen.dispose();
  }
});

test('scheduleOutcome maps every stored shape and never returns a raw key', () => {
  const statusOf = (id) => ({ a: 'running', b: 'cancelled', c: 'mystery' })[id] ?? null;
  assert.equal(scheduleOutcome('ok').label, 'OK');
  assert.equal(scheduleOutcome('failed').failed, true);
  assert.equal(scheduleOutcome('started job a', statusOf).label, 'w toku');
  assert.equal(scheduleOutcome('started job b', statusOf).label, 'przerwane');
  assert.equal(scheduleOutcome('started job c', statusOf), null, 'an unknown status is not rendered as a key');
  assert.equal(scheduleOutcome('started job z', statusOf), null);
  assert.equal(scheduleOutcome(''), null);
  assert.equal(scheduleOutcome('the node wrote something new'), null, 'free text is not classified');
});

// n15 (critic-round2-wave2 MINOR 9): the scheduler stores `pominięto: <why>`
// when it declines a Sync slot because parity errors await a repair
// (`elastic::scheduled_sync_blocker`). That is neither a run nor a failure,
// and it used to render as no outcome at all — the row read like a normal run.
test('a skipped scheduled Sync reads as skipped, with the reason only in the tooltip', async () => {
  const why = 'scrub zgłosił 3 błędów, których nic jeszcze nie naprawiło — uruchom naprawę z parity';
  const outcome = scheduleOutcome(`pominięto: ${why}`);
  assert.deepEqual(outcome, { label: 'pominięto', failed: false, skipped: true, title: why });
  // Every stored shape carries the same fields.
  for (const raw of ['ok', 'failed', 'skipped', 'failed to start: busy']) {
    assert.deepEqual(Object.keys(scheduleOutcome(raw)).sort(), ['failed', 'label', 'skipped', 'title'], raw);
  }
  assert.equal(scheduleOutcome('skipped').skipped, true);
  assert.equal(scheduleOutcome('failed to start: busy').skipped, false);

  const sched = { ...elasticSchedules, rows: [
    elasticSchedules.rows[0],
    { ...elasticSchedules.rows[1], enabled: true, lastRunAt: '2026-09-02 03:00:00', lastResult: `pominięto: ${why}` },
  ] };
  const screen = fakeScreen(fixtures({ tentaNasSchedulesListRequest: sched }));
  screen.later = () => {};
  try {
    const body = mount();
    await drawTasks(screen, body);
    await flush(); await flush();
    const sub = scheduleRows(body)[1].querySelector('[data-role="sub"]');
    assert.match(sub.textContent, /^ostatni: .* · pominięto$/);
    assert.equal(sub.getAttribute('title'), why, 'the node\'s sentence is the tooltip');
    assert.doesNotMatch(sub.textContent, /scrub zgłosił/, 'and never the row text');
  } finally {
    screen.dispose();
  }
});

// n15 (critic-round2-wave2 MINOR 12): the status of a job a schedule row
// names used to be read ONCE per mount, so a job still running at that moment
// read "w toku" until the tab was left. An unfinished status is re-read on the
// schedules cadence; a finished one is never asked for again.
test('a named job still running is re-read on the schedules poll, a finished one is cached', async () => {
  const sched = { ...schedules, rows: [
    schedules.rows[0],
    { ...schedules.rows[1], lastRunAt: '2026-08-01 03:30:00', lastResult: `started job ${UUID_B}` },
  ] };
  let status = 'running';
  const gets = [];
  const screen = fakeScreen(fixtures({
    tentaNasJobsListRequest: { jobs: [] },
    tentaNasSchedulesListRequest: sched,
    tentaNasJobGetRequest: (p) => { gets.push(p.jobId); return { job: { jobId: p.jobId, kind: 'pool_trim', subject: 'fast', status } }; },
  }));
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const runPolls = async () => {
    const due = scheduled.splice(0);
    for (const fn of due) await fn();
    await flush(); await flush();
  };
  try {
    const body = mount();
    await drawTasks(screen, body);
    await flush(); await flush();
    const trimSub = () => scheduleRows(body)[1].querySelector('[data-role="sub"]').textContent;
    assert.match(trimSub(), / · w toku$/);
    assert.equal(gets.length, 1);

    status = 'succeeded';
    await runPolls();
    assert.match(trimSub(), / · OK$/, 'the finished outcome replaces "running"');
    const afterFinish = gets.length;
    assert.ok(afterFinish >= 2, 'the running job was asked for again');

    await runPolls();
    await runPolls();
    assert.equal(gets.length, afterFinish, 'a finished status is never asked for again');
    assert.match(trimSub(), / · OK$/);
  } finally {
    screen.dispose();
  }
});

// Wave 6 B: a node of this build sends the outcome structured, with the
// started job's status read on the node. The row says it without asking for
// any job by id, follows the status the next poll brings in place (the row
// and its toggle are the same nodes), and the refusal's sentence — ids
// scrubbed — is only the tooltip.
test('a structured outcome is shown from the row itself, with no job looked up by id', async () => {
  const structured = (status) => ({ ...schedules, rows: [
    { ...schedules.rows[0], lastRunAt: '2026-08-30 02:00:00', lastResult: `started job ${UUID_B}`, lastOutcome: 'started', lastJobStatus: status },
    { ...schedules.rows[1], lastRunAt: '2026-09-01 03:30:00', lastResult: 'failed to start: x', lastOutcome: 'start_failed',
      lastDetail: 'Na tym dysku trwa już autotest SMART; odmowa drugiego (0191f2c0-0000-7000-8000-000000000001)' },
  ] });
  let current = structured('running');
  const gets = [];
  const screen = fakeScreen(fixtures({
    tentaNasJobsListRequest: { jobs: [] },
    tentaNasSchedulesListRequest: () => current,
    tentaNasJobGetRequest: (p) => { gets.push(p.jobId); return { job: { jobId: p.jobId, status: 'failed' } }; },
  }));
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  try {
    const body = mount();
    await drawTasks(screen, body);
    await flush(); await flush();
    const sub = (i) => scheduleRows(body)[i].querySelector('[data-role="sub"]');
    assert.match(sub(0).textContent, / · w toku$/);
    const row = scheduleRows(body)[0];
    const toggle = row.querySelector('[data-act="toggle"]');

    current = structured('succeeded');
    const due = scheduled.splice(0);
    for (const fn of due) await fn();
    await flush(); await flush();
    assert.match(sub(0).textContent, / · OK$/, 'the status the poll brought');
    assert.equal(scheduleRows(body)[0], row, 'the row is patched, not rebuilt');
    assert.equal(row.querySelector('[data-act="toggle"]'), toggle);

    assert.match(sub(1).textContent, / · błąd$/);
    const title = sub(1).getAttribute('title');
    assert.match(title, /^Na tym dysku trwa już autotest SMART/);
    assert.doesNotMatch(title, /0191f2c0/, 'an id in the node\'s sentence is scrubbed');
    assert.deepEqual(gets, [], 'no job is asked for by id');
    assert.doesNotMatch(body.textContent, new RegExp(UUID_B));
  } finally {
    screen.dispose();
  }
});

// A2/A3/A5 (critic-real-functionality-2026-09-21.md): Cancel only where it
// really stops the work.
test('Cancel is offered only for a pool scrub, never for kinds whose cancel stops nothing', () => {
  assert.equal(jobCanCancel({ kind: 'pool_scrub' }), true);
  for (const kind of ['elastic_fix', 'elastic_add_disk', 'elastic_destroy', 'elastic_replace_disk', 'elastic_sync', 'elastic_mover',
    'smart_test', 'pool_trim', 'pool_create', 'pool_replace', 'dataset_destroy', 'disk_wipe', 'packages_install', 'config_import', 'some_future_kind']) {
    assert.equal(jobCanCancel({ kind }), false, kind);
  }
  const host = mount();
  host.innerHTML = ['smart_test', 'elastic_fix', 'pool_trim', 'pool_scrub'].map((kind) => jobRowSkeleton({ jobId: kind, kind, subject: 'tank', status: 'running' })).join('');
  assert.deepEqual([...host.querySelectorAll('[data-act="cancel"]')].map((b) => b.closest('.job-row').dataset.job), ['pool_scrub']);
  assert.equal(host.querySelectorAll('[data-act="log"]').length, 4, 'the log stays reachable for every job');
  host.remove();
});

test('every hide-below of the history table is a breakpoint tf-table honours', async () => {
  const screen = fakeScreen(fixtures());
  screen.later = () => {};
  try {
    const body = mount();
    await drawTasks(screen, body);
    await flush();
    const table = body.querySelector('#nas-jobs-table');
    const wanted = [...table.querySelectorAll('tf-column[hide-below]')].map((c) => c.getAttribute('hide-below'));
    const got = [...table.shadowRoot.querySelectorAll('thead th')].flatMap((th) => [...th.classList]
      .filter((c) => c.startsWith('tf-table__col--hide-below-')).map((c) => c.slice('tf-table__col--hide-below-'.length)));
    assert.deepEqual(got, wanted);
    assert.ok(wanted.includes('1024'));
  } finally {
    screen.dispose();
  }
});

// n15 (backlog 2026-09-21): the protection strip was one `patchHtml` over both
// columns, so a single "last run" tick rebuilt every row and every chip in it.
// Rows are keyed now, and a chip that stays a chip keeps its node while its
// status and label change in place.
test('a changed schedule patches only its own protection row, in place', async () => {
  let current = schedules;
  const screen = fakeScreen(fixtures({ tentaNasSchedulesListRequest: () => current }));
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  try {
    const body = mount();
    await drawTasks(screen, body);
    await flush();
    const rowOf = (key) => body.querySelector(`#nas-prot [data-prot="${key}"]`);
    const before = { snap: rowOf('snapshot:tank/home'), scrub: rowOf('scrub:tank'), smart: rowOf('smart') };
    assert.ok(before.snap && before.scrub && before.smart, 'all three rows are painted');
    const snapChip = before.snap.querySelector('tf-chip');
    const protectChip = before.snap.querySelectorAll('tf-chip')[1];
    const smartChip = before.smart.querySelector('tf-chip');
    assert.equal(snapChip.getAttribute('label'), 'wyłączony');
    assert.equal(snapChip.getAttribute('status'), 'warn');
    assert.ok(protectChip, 'the protected schedule carries a second chip');

    current = {
      ...schedules,
      rows: schedules.rows.map((r) => (r.kind === 'snapshot' ? { ...r, enabled: true, lastRunAt: '2026-09-02 02:00:00', lastResult: 'ok' } : r)),
    };
    for (const fn of scheduled.splice(0)) await fn();
    await flush();

    assert.ok(rowOf('snapshot:tank/home') === before.snap, 'the changed row is patched, not rebuilt');
    assert.ok(rowOf('scrub:tank') === before.scrub, 'the scrub row is untouched');
    assert.ok(rowOf('smart') === before.smart, 'the SMART row is untouched');
    assert.ok(before.snap.querySelector('tf-chip') === snapChip, 'the state chip keeps its node');
    assert.ok(before.snap.querySelectorAll('tf-chip')[1] === protectChip, 'the protection chip keeps its node');
    assert.ok(before.smart.querySelector('tf-chip') === smartChip);
    assert.equal(snapChip.getAttribute('status'), 'ok');
    assert.match(snapChip.getAttribute('label'), /^ostatni/);

    // Dropping the schedule takes its row with it and brings the "none" row.
    current = { ...schedules, rows: schedules.rows.filter((r) => r.kind !== 'snapshot') };
    for (const fn of scheduled.splice(0)) await fn();
    await flush();
    assert.equal(rowOf('snapshot:tank/home'), null);
    assert.ok(rowOf('snapshot:none'), 'the empty state row replaces it');
    assert.ok(rowOf('scrub:tank') === before.scrub, 'the other column is still untouched');
  } finally {
    screen.dispose();
  }
});

// Minor 8 of the release review: a scheduled Sync the node skipped over a
// parity fault records a CODE, and the row says why in the reader's
// language — the older stored sentence stays a tooltip.
test('a coded skip reason is the row text, translated, and never a code', async () => {
  const sched = { ...elasticSchedules, rows: [
    elasticSchedules.rows[0],
    { ...elasticSchedules.rows[1], enabled: true, lastRunAt: '2026-09-02 03:00:00', lastResult: 'pominięto: reason:elastic_parity_fault_unacknowledged' },
  ] };
  try {
    for (const [language, words] of [['pl', /pominięto: macierz ma nienaprawiony błąd scrub lub naprawy/], ['en', /the array has an unrepaired scrub or repair fault/]]) {
      await I18n.setLanguage(language);
      const screen = fakeScreen(fixtures({ tentaNasSchedulesListRequest: sched }));
      screen.later = () => {};
      const body = mount();
      await drawTasks(screen, body);
      await flush(); await flush();
      const sub = scheduleRows(body)[1].querySelector('[data-role="sub"]');
      assert.match(sub.textContent, words, language);
      assert.doesNotMatch(sub.textContent, /reason:|elastic_/, language);
      screen.dispose();
    }
  } finally { await I18n.setLanguage('pl'); }
  // A code this build has no words for stays a tooltip, never row text.
  const unknown = scheduleOutcome('pominięto: reason:elastic_something_new');
  assert.equal(unknown.skipped, true);
  assert.equal(unknown.reason, undefined);
});

// "Uruchom teraz" on a Sync the node refuses for want of the confirm (a
// parity fault, or a confirm that named an older fault): the refusal is
// worded by the toast, and the admin is taken to the array screen, where the
// confirm that names the cost lives. Any other refusal stays where it is.
test('"Uruchom teraz" on a Sync that needs the confirm opens the array screen', async () => {
  const sched = { ...elasticSchedules, rows: [elasticSchedules.rows[0], { ...elasticSchedules.rows[1], enabled: true }] };
  for (const [refusal, redirected] of [
    ['refusal:elastic_fault_unacknowledged', true],
    ['refusal:elastic_fault_changed', true],
    ['refusal:elastic_operation_pending', false],
  ]) {
    const screen = fakeScreen(fixtures({
      tentaNasSchedulesListRequest: sched,
      tentaNasElasticArraySyncRequest: () => { throw new Error(`protocol error NotAvailable: ${refusal}`); },
    }));
    screen.later = () => {};
    // As the real screen does: the error is toasted and the call answers null.
    screen.withSudo = async (fn) => { try { return await fn('hunter2'); } catch { return null; } };
    const opened = [];
    screen.openArray = (name) => opened.push(name);
    const body = mount();
    await drawTasks(screen, body);
    await flush();
    click(scheduleRows(body)[1].querySelector('[data-act="run"]'));
    await flush(); await flush();
    assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasElasticArraySyncRequest').length, 1, refusal);
    assert.deepEqual(opened, redirected ? ['media'] : [], refusal);
    assert.equal(screen.jobLogs.length, 0, refusal);
    screen.dispose();
  }
});

// Every code a multi-disk job line can carry, read from the Rust that writes
// it (tentanas/jobs.rs: the batch body and its verdicts; tentanas/db.rs: the
// busy disk the insert refuses), and every state it can be in: each has words
// in every locale, so a code added there without words here fails this test
// instead of showing a bare state on screen.
test('every disk-line code and state of a multi-disk job is worded in every locale', async () => {
  const { readFileSync } = await import('node:fs');
  const { join } = await import('node:path');
  const { WWW_ROOT } = await import('./_test-setup.js');
  const { diskLineReason } = await import('./tasks.js');
  const production = (file) => {
    const src = readFileSync(join(WWW_ROOT, '..', 'src', 'tentanas', file), 'utf8');
    const cut = src.indexOf('#[cfg(test)]\nmod tests');
    return cut >= 0 ? src.slice(0, cut) : src;
  };
  const jobs = readFileSync(join(WWW_ROOT, '..', 'src', 'tentanas', 'jobs.rs'), 'utf8');
  const from = jobs.indexOf('fn line_verdict');
  const batch = jobs.slice(from, jobs.indexOf('\n}\n', jobs.indexOf('pub async fn smart_self_test_batch', from)));
  const codes = new Set([...batch.matchAll(/(?:coded_reason|reason)\(\s*"(\w+)"/g)].map((m) => m[1]));
  for (const m of production('db.rs').matchAll(/"refused", vec!\[super::disks::coded_reason\("(\w+)"/g)) codes.add(m[1]);
  assert.ok(codes.size >= 11, `the scan found the codes (${[...codes].join(', ')})`);
  const states = ['pending', 'running', 'passed', 'failed', 'incomplete', 'refused', 'skipped', 'interrupted', 'cancelled'];
  try {
    for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
      await I18n.setLanguage(lang);
      for (const code of codes) {
        const words = diskLineReason({ reasons: [{ code, params: { detail: 'Completed: read failure', min: '30' } }] });
        assert.ok(words, `${code} has words in ${lang}`);
        assert.doesNotMatch(words, /tentanas\.|\{/, `${code} in ${lang}: ${words}`);
      }
      for (const state of states) {
        const words = I18n.t('tentanas.jobs.disk_state.' + state);
        assert.notEqual(words, 'tentanas.jobs.disk_state.' + state, `${state} in ${lang}`);
      }
    }
  } finally {
    await I18n.setLanguage('pl');
  }
});

// Wave 10 (n18d): the sharing stop is one job with a line per STEP —
// SMB/NFS shares, iSCSI/NVMe-oF targets, the disable — each worded, with
// its coded reason; a failure shows which step, and the steps after it read
// skipped.
test('a sharing stop shows its steps as worded lines, and the history names the step that failed', async () => {
  const step = (name, state, reasons = []) => ({ name, lastKnown: false, state, progressPct: null, reasons });
  const running = {
    jobId: 'job-s', kind: 'sharing_stop', subject: 'helios', status: 'running', startedBy: 'admin', startedAt: '2026-09-26 10:00:00', log: [],
    disks: [step('shares', 'done', [{ code: 'shares_stopped', params: { smb: '2', nfs: '1' } }]), step('targets', 'running'), step('disable', 'pending')],
  };
  const failed = {
    jobId: 'job-f', kind: 'sharing_stop', subject: 'helios', status: 'failed', startedBy: 'admin', startedAt: '2026-09-26 09:00:00',
    finishedAt: '2026-09-26 09:01:00', log: [], error: 'refusal:sharing_stop_failed_partial',
    disks: [step('shares', 'done'), step('targets', 'failed', [
      { code: 'targets_left', params: { count: '1' } },
      { code: 'rollback_partial', params: { shares: 'projekty', targets: '', other_shares: '0', other_targets: '1' } },
    ]), step('disable', 'skipped')],
  };
  const screen = fakeScreen(fixtures({ tentaNasJobsListRequest: { jobs: [running, failed] } }));
  const body = mount();
  await drawTasks(screen, body);
  await flush();
  const lines = [...body.querySelectorAll('#nas-jobs-running .job-disk')];
  assert.equal(lines.length, 3, 'one line per step');
  assert.match(body.querySelector('#nas-jobs-running .job-name').textContent, /Zatrzymanie udostępniania\s+helios/);
  assert.equal(lines[0].querySelector('[data-role="name"]').textContent, 'Udziały SMB/NFS');
  assert.equal(lines[0].querySelector('[data-role="state"]').getAttribute('label'), 'gotowe');
  assert.equal(lines[0].querySelector('[data-role="state"]').getAttribute('status'), 'ok');
  assert.equal(lines[0].querySelector('[data-role="detail"]').textContent, '2 SMB, 1 NFS zatrzymane');
  assert.equal(lines[1].querySelector('[data-role="name"]').textContent, 'Targety iSCSI/NVMe-oF');
  assert.equal(lines[2].querySelector('[data-role="name"]').textContent, 'Wyłączenie TentaNas');
  const row = body.querySelector('#nas-jobs-table').rows.find((r) => r._job.jobId === 'job-f');
  // Critic wave 10, M4: a partial rollback is said as such, and what did
  // not come back is named — the asking organisation's by name, another's
  // counted.
  assert.match(row.result, /nie wszystko udało się przywrócić/);
  assert.match(row.result, /Targety iSCSI\/NVMe-oF: nie przeszedł — 1 nadal w jądrze; nie przywrócono: projekty, 1 target innych organizacji · Wyłączenie TentaNas: pominięty/);
  assert.doesNotMatch(body.textContent + row.result, /tentanas\.|shares_stopped|targets_left|rollback_partial|refusal:|\{rows\}/, 'no raw key, code or placeholder');
  screen.dispose();
});
