// =============================================================================
// File: modules/tentanas/snapshots.test.js
// Description: Snapshot dialogs against a fake screen: `newerThan` picks the
// snapshots a rollback would destroy, the rollback dialog lists them and
// sends SnapshotRollbackRequest with `destroyNewer` only after the short
// name was retyped and confirmed, its secondary action swaps to the clone
// dialog, and the read-only browser walks SnapshotBrowseRequest with the
// path breadcrumb. The §5.10 half: the lock column, the delete dialog that
// promises a RECORDED destroy rather than a deletion, the manual-snapshot
// protection option behind its own confirmation, and no unprotect action
// anywhere. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, typeInto, confirmWindow, click, window, windowTitle } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openRollbackDialog, openSnapshotBrowser, newerThan } = await import('./snapshots.js');

const snap = (short, createdAt, overrides = {}) => ({
  name: `tank/home@${short}`, shortName: short, dataset: 'tank/home', createdAt, usedBytes: 1024 * 1024, referencedBytes: 3e9, origin: 'auto', holds: 0, clones: [],
  ...overrides,
});
const all = [
  snap('daily-2026-08-30', '2026-08-30 02:00:00'),
  snap('daily-2026-08-31', '2026-08-31 02:00:00'),
  snap('manual-before-upgrade', '2026-08-31 09:15:00', { origin: 'manual' }),
  snap('daily-2026-09-01', '2026-09-01 02:00:00'),
  { name: 'tank/media@daily-2026-09-01', shortName: 'daily-2026-09-01', dataset: 'tank/media', createdAt: '2026-09-01 02:00:00', usedBytes: 0, origin: 'auto' },
];

const confirmButton = (win) => win.querySelector('tf-button[data-action="confirm"]');

test('newerThan returns the later snapshots of the same dataset, oldest first', () => {
  const target = all[1];
  assert.deepEqual(newerThan(all, target).map((s) => s.shortName), ['manual-before-upgrade', 'daily-2026-09-01']);
  assert.deepEqual(newerThan(all, all[3]), [], 'the newest snapshot has nothing after it');
});

test('rollback with newer snapshots lists them and sends destroyNewer only after retype and confirm', async () => {
  const target = all[1];
  const newer = newerThan(all, target);
  const screen = fakeScreen({ tentaNasSnapshotRollbackRequest: { job: { jobId: 'job-3', kind: 'snapshot_rollback', status: 'running' } } });
  const win = openRollbackDialog(screen, { snapshot: target, newer, onDone: () => {} });
  await flush();

  assert.equal(windowTitle(win), 'Rollback do daily-2026-08-31');
  assert.ok(win.querySelector('.confirm-type .field > label'), 'the retype row is a labelled field (n10)');
  const lost = win.querySelector('.snap-lost');
  assert.ok(lost, 'loss list shown');
  assert.match(lost.textContent, /manual-before-upgrade/);
  assert.match(lost.textContent, /daily-2026-09-01/);
  assert.match(win.querySelector('.wizard-warning').textContent, /łącznie z 2 nowszymi snapshotami/);
  assert.match(win.querySelector('.explain-box').textContent, /Potrzebujesz tylko kilku plików\?/);
  assert.equal(confirmButton(win).textContent.trim(), 'Rollback', 'n10 keeps the plain label even with newer snapshots');
  assert.equal(win.querySelector('[data-act="secondary"]').textContent.trim(), 'Zrób Clone zamiast');

  assert.ok(confirmButton(win).hasAttribute('disabled'));
  confirmWindow(win);
  await flush();
  assert.equal(screen.calls.length, 0, 'nothing sent while locked');

  typeInto(win.querySelector('#nas-retype'), 'daily-2026-08-3');
  assert.ok(confirmButton(win).hasAttribute('disabled'), 'partial name keeps it locked');
  confirmWindow(win);
  await flush();
  assert.equal(screen.calls.length, 0);
  typeInto(win.querySelector('#nas-retype'), target.name);
  assert.ok(confirmButton(win).hasAttribute('disabled'), 'the full dataset@name is not what the label asks for');

  typeInto(win.querySelector('#nas-retype'), target.shortName);
  assert.ok(!confirmButton(win).hasAttribute('disabled'));
  assert.equal(screen.calls.length, 0, 'typing alone sends nothing');
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(screen.calls.length, 1);
  assert.equal(screen.calls[0].kind, 'tentaNasSnapshotRollbackRequest');
  assert.deepEqual(screen.calls[0].payload, { name: target.name, confirmName: target.name, destroyNewer: true, sudoPassword: 'hunter2' });
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['job-3']);
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(document.querySelector('tf-window'), null, 'dialog closed');
  screen.dispose();
});

test('rollback to the newest snapshot sends destroyNewer=false', async () => {
  const target = all[3];
  let done = 0;
  const screen = fakeScreen({ tentaNasSnapshotRollbackRequest: { ok: true } });
  const win = openRollbackDialog(screen, { snapshot: target, newer: newerThan(all, target), onDone: () => { done += 1; } });
  await flush();
  assert.equal(win.querySelector('.snap-lost'), null, 'no loss list');
  assert.match(win.querySelector('.wizard-warning').textContent, /nowszych snapshotów nie ma/);
  assert.equal(confirmButton(win).textContent.trim(), 'Rollback');
  typeInto(win.querySelector('#nas-retype'), target.shortName);
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(screen.calls.length, 1);
  assert.equal(screen.calls[0].payload.destroyNewer, false);
  assert.equal(screen.calls[0].payload.confirmName, target.name);
  assert.equal(done, 1, 'a direct answer runs onDone right away');
  assert.equal(screen.jobLogs.length, 0);
  win.remove();
  screen.dispose();
});

test('a cancelled sudo prompt sends nothing and keeps the dialog armed', async () => {
  const target = all[1];
  const screen = fakeScreen({ tentaNasSnapshotRollbackRequest: {} }, { sudo: null });
  const win = openRollbackDialog(screen, { snapshot: target, newer: newerThan(all, target), onDone: () => {} });
  await flush();
  typeInto(win.querySelector('#nas-retype'), target.shortName);
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(screen.calls.length, 0);
  assert.ok(document.querySelector('tf-window'), 'still open');
  assert.ok(!confirmButton(win).hasAttribute('disabled'));
  win.remove();
  screen.dispose();
});

test('"Zrób Clone zamiast" closes the rollback dialog and opens the clone dialog with the pool prefix', async () => {
  const target = all[1];
  const screen = fakeScreen({ tentaNasSnapshotCloneRequest: { ok: true } });
  let done = 0;
  const win = openRollbackDialog(screen, { snapshot: target, newer: newerThan(all, target), onDone: () => { done += 1; } });
  await flush();
  click(win.querySelector('[data-act="secondary"]'));
  await new Promise((r) => setTimeout(r, 300));
  assert.ok(!win.isConnected, 'rollback dialog closed');
  const clone = document.querySelector('tf-window');
  assert.ok(clone, 'clone dialog opened');
  assert.equal(windowTitle(clone), 'Clone');
  assert.equal(clone.getAttribute('subtitle'), target.name);
  const input = clone.querySelector('#nas-cl-target');
  assert.equal(input.getAttribute('prefix'), 'tank/');
  assert.equal(input.getAttribute('value'), 'home-clone');
  typeInto(input, 'home-restore');
  confirmWindow(clone);
  await flush();
  await flush();
  assert.deepEqual(screen.calls, [{ kind: 'tentaNasSnapshotCloneRequest', payload: { name: target.name, target: 'tank/home-restore', sudoPassword: 'hunter2' } }]);
  assert.equal(done, 1);
  assert.equal(screen.calls.some((c) => c.kind === 'tentaNasSnapshotRollbackRequest'), false, 'no rollback was sent');
  clone.remove();
  screen.dispose();
});

test('the browser walks the snapshot through SnapshotBrowseRequest with a path breadcrumb', async () => {
  const target = all[1];
  const tree = {
    '': [{ name: 'docs', path: 'docs', dataset: null, sharedAs: [] }, { name: 'photos', path: 'photos', dataset: null, sharedAs: ['zdjecia'] }],
    docs: [{ name: '2026', path: 'docs/2026', dataset: null, sharedAs: [] }],
    'docs/2026': [],
  };
  const screen = fakeScreen({ tentaNasSnapshotBrowseRequest: ({ path }) => ({ path, entries: tree[path] }) });
  const win = openSnapshotBrowser(screen, { snapshot: target });
  await flush();
  await flush();
  assert.equal(windowTitle(win), 'Przeglądaj snapshot daily-2026-08-31');
  assert.equal(win.getAttribute('subtitle'), 'tank/home');
  assert.deepEqual(screen.calls[0], { kind: 'tentaNasSnapshotBrowseRequest', payload: { snapshot: target.name, path: '' } });
  assert.match(win.querySelector('.explain-box').textContent, /tylko do odczytu/);
  const table = win.querySelector('#nas-sbr-table');
  assert.deepEqual(table.rows.map((r) => r._entry.name), ['docs', 'photos']);
  assert.deepEqual([...win.querySelectorAll('#nas-sbr-crumbs .tf-breadcrumb-item')].map((c) => c.textContent), ['daily-2026-08-31']);
  assert.equal(win.querySelector('#nas-sbr-path').textContent, 'tank/home/.zfs/snapshot/daily-2026-08-31');

  table.dispatchEvent(new window.CustomEvent('row-click', { detail: { row: table.rows[0] } }));
  await flush();
  await flush();
  assert.deepEqual(screen.calls.at(-1).payload, { snapshot: target.name, path: 'docs' });
  assert.deepEqual(table.rows.map((r) => r._entry.name), ['2026']);
  assert.deepEqual([...win.querySelectorAll('#nas-sbr-crumbs .tf-breadcrumb-item')].map((c) => c.textContent), ['daily-2026-08-31', 'docs']);
  assert.equal(win.querySelector('#nas-sbr-path').textContent, 'tank/home/.zfs/snapshot/daily-2026-08-31/docs');

  table.dispatchEvent(new window.CustomEvent('row-click', { detail: { row: table.rows[0] } }));
  await flush();
  await flush();
  assert.equal(table.rows.length, 0, 'an empty directory shows no rows');
  assert.deepEqual([...win.querySelectorAll('#nas-sbr-crumbs .tf-breadcrumb-item')].map((c) => c.textContent), ['daily-2026-08-31', 'docs', '2026']);

  click(win.querySelector('#nas-sbr-crumbs a'));
  await flush();
  await flush();
  assert.deepEqual(screen.calls.at(-1).payload, { snapshot: target.name, path: '' }, 'the root crumb returns to the snapshot root');
  assert.deepEqual(table.rows.map((r) => r._entry.name), ['docs', 'photos']);
  win.remove();
  screen.dispose();
});

// ---------------------------------------------------------------------------
// Protected snapshots (plan-02 §5.10)
// ---------------------------------------------------------------------------

const { drawSnapshots, openSnapshotNowDialog, isProtected, protectionShortfall } = await import('./snapshots.js');

const latestWindow = () => [...document.querySelectorAll('tf-window')].at(-1);
// TfWindow.open resolves only once the window really left the DOM, and the
// close is animated — so everything behind a confirmation is waited for
// rather than slept on, which is what a loaded test run needs.
const waitFor = async (read, timeoutMs = 3000) => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const value = read();
    if (value !== null && value !== undefined) return value;
    await new Promise((r) => setTimeout(r, 10));
  }
  return read();
};

const listFixtures = (snapshots) => ({
  tentaNasSnapshotsListRequest: { snapshots, total: snapshots.length, totalUsedBytes: 0 },
  tentaNasSnapshotSchedulesListRequest: { schedules: [] },
  tentaNasSharesListRequest: { shares: [] },
});

test('the snapshot list marks a protected snapshot with the lock column and never offers to unprotect it', async () => {
  const rows = [
    snap('auto-20260901-1445-frequent', '2026-09-01 14:45:00'),
    snap('przed-migracja', '2026-08-31 09:15:00', { origin: 'manual', holds: 1, protectedUntil: '2026-10-01 09:15:00' }),
    snap('kwartal', '2026-08-01 00:00:00', { origin: 'manual', holds: 1, destroyPending: true }),
  ];
  const screen = fakeScreen(listFixtures(rows));
  const host = document.createElement('div');
  document.body.appendChild(host);
  await drawSnapshots(screen, host, { pool: 'tank', datasets: [{ name: 'tank/home', snapshotCount: 3 }] });
  await flush();

  const table = host.querySelector('#nas-snap-table');
  assert.deepEqual(
    [...host.querySelectorAll('#nas-snap-table tf-column')].map((c) => c.getAttribute('key')),
    ['name', 'created', 'used', 'origin', 'protection'],
    'the lock column is part of the n10 table',
  );
  assert.equal(table.rows.length, 3);
  assert.match(table.rows[0].protection, /—/, 'an unprotected snapshot shows no lock');
  assert.doesNotMatch(table.rows[0].protection, /i-lock/);
  assert.match(table.rows[1].protection, /#i-lock/, 'the protected one carries the lock icon');
  assert.match(table.rows[1].protection, /chroniony do/);
  assert.match(table.rows[1].protection, /zatwierdzenia drugiej osoby/, 'the row says why nothing can unprotect it');
  assert.match(table.rows[2].protection, /Usunięcie zapisane/, 'a deferred destroy is legible in the row');

  const actions = table.rowActions(table.rows[1]);
  const acts = [...actions.querySelectorAll('[data-act]')].map((b) => b.getAttribute('data-act'));
  assert.deepEqual(acts, ['browse', 'clone', 'rollback', 'delete'], 'no unprotect button — the app cannot do it');
  host.remove();
  screen.dispose();
});

test('deleting a protected snapshot warns that the destroy is only recorded', async () => {
  document.body.innerHTML = '';
  const target = snap('przed-migracja', '2026-08-31 09:15:00', { origin: 'manual', holds: 1 });
  const screen = fakeScreen({
    ...listFixtures([target]),
    tentaNasSnapshotDestroyRequest: { job: { jobId: 'job-9', kind: 'snapshot_destroy', status: 'running' } },
  });
  const host = document.createElement('div');
  document.body.appendChild(host);
  await drawSnapshots(screen, host, { pool: 'tank', datasets: [{ name: 'tank/home', snapshotCount: 1 }] });
  await flush();
  const table = host.querySelector('#nas-snap-table');
  click(table.rowActions(table.rows[0]).querySelector('[data-act="delete"]'));
  await flush();
  await flush();
  const confirm = latestWindow();
  assert.match(confirm.querySelector('[slot="body"]').textContent, /Usunięcie zostanie tylko ZAPISANE/);
  confirmWindow(confirm);
  const sent = await waitFor(() => screen.calls.find((c) => c.kind === 'tentaNasSnapshotDestroyRequest'));
  assert.deepEqual(sent.payload, { names: [target.name], sudoPassword: 'hunter2' });
  screen.dispose();
});

test('the manual snapshot dialog sends protectDays only after the lock is confirmed', async () => {
  document.body.innerHTML = '';
  let created = null;
  const screen = fakeScreen({ tentaNasSnapshotCreateRequest: (p) => { created = p; return { snapshots: [], total: 0, totalUsedBytes: 0 }; } });
  const win = openSnapshotNowDialog(screen, { dataset: 'tank/home', onDone: () => {} });
  await flush();
  const days = win.querySelector('#nas-sn-protect-days');
  assert.ok(days.hasAttribute('disabled'), 'the period is off until protection is switched on');
  assert.equal(days.getAttribute('value'), '30');

  const protect = win.querySelector('#nas-sn-protect');
  protect.checked = true;
  protect.dispatchEvent(new window.CustomEvent('change', { bubbles: true }));
  assert.ok(!days.hasAttribute('disabled'));
  typeInto(days, '90');
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(created, null, 'nothing is created before the one-way door is acknowledged');
  const confirm = latestWindow();
  assert.notEqual(confirm, win, 'the protection is confirmed in its own dialog');
  assert.match(confirm.querySelector('[slot="body"]').textContent, /NIE potrafi zdjąć tej ochrony/);
  confirmWindow(confirm);
  await waitFor(() => created);
  assert.equal(created.protectDays, 90);
  assert.equal(created.dataset, 'tank/home');
  assert.equal(created.sudoPassword, 'hunter2');
  screen.dispose();
});

test('a snapshot without protection sends protectDays 0 and asks nothing extra', async () => {
  document.body.innerHTML = '';
  let created = null;
  const screen = fakeScreen({ tentaNasSnapshotCreateRequest: (p) => { created = p; return { snapshots: [], total: 0, totalUsedBytes: 0 }; } });
  const win = openSnapshotNowDialog(screen, { dataset: 'tank/home', onDone: () => {} });
  await flush();
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(created.protectDays, 0);
  assert.equal(document.querySelectorAll('tf-window').length, 1, 'no confirmation for an unprotected snapshot');
  screen.dispose();
});

test('isProtected reads the hold, and protectionShortfall names the first tier that is too short', () => {
  assert.equal(isProtected({ holds: 0 }), false);
  assert.equal(isProtected({ holds: 2 }), true);
  const base = { schedule: { every: '15m' }, keepFrequent: 96, keepHourly: 0, keepDaily: 30, keepWeekly: 0, keepMonthly: 12 };
  assert.equal(protectionShortfall({ ...base, protectDays: 0 }), null);
  assert.deepEqual(protectionShortfall({ ...base, protectDays: 30 }), { tier: 'frequent', days: 1 });
  assert.equal(protectionShortfall({ ...base, keepFrequent: 0, protectDays: 30 }), null);
  assert.deepEqual(protectionShortfall({ ...base, keepFrequent: 0, keepDaily: 7, protectDays: 30 }), { tier: 'daily', days: 7 });
});
