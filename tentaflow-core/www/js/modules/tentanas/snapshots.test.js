// =============================================================================
// File: modules/tentanas/snapshots.test.js
// Description: Snapshot dialogs against a fake screen: `newerThan` picks the
// snapshots a rollback would destroy, the rollback dialog lists them and
// sends SnapshotRollbackRequest with `destroyNewer` only after the short
// name was retyped and confirmed, its secondary action swaps to the clone
// dialog, and the read-only browser walks SnapshotBrowseRequest with the
// path breadcrumb. Runs under happy-dom.
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
  const lost = win.querySelector('.snap-lost');
  assert.ok(lost, 'loss list shown');
  assert.match(lost.textContent, /manual-before-upgrade/);
  assert.match(lost.textContent, /daily-2026-09-01/);
  assert.match(win.querySelector('.wizard-warning').textContent, /łącznie z 2 nowszymi snapshotami/);
  assert.match(win.querySelector('.explain-box').textContent, /Potrzebujesz tylko kilku plików\?/);
  assert.equal(confirmButton(win).textContent.trim(), 'Rollback (usuń 2 nowsze)', 'the button names the count');
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
