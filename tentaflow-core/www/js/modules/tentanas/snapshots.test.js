// =============================================================================
// File: modules/tentanas/snapshots.test.js
// Description: Snapshot rollback against a fake screen: `newerThan` picks the
// snapshots a rollback would destroy, the dialog lists them, and the
// SnapshotRollbackRequest carries `destroyNewer` only after the name was
// retyped and confirmed (true when newer snapshots exist, false otherwise).
// Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, typeInto, confirmWindow } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openRollbackDialog, newerThan } = await import('./snapshots.js');

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

  const lost = win.querySelector('.snap-lost');
  assert.ok(lost, 'loss list shown');
  assert.match(lost.textContent, /manual-before-upgrade/);
  assert.match(lost.textContent, /daily-2026-09-01/);
  assert.match(confirmButton(win).textContent, /2/, 'the button names the count');

  assert.ok(confirmButton(win).hasAttribute('disabled'));
  confirmWindow(win);
  await flush();
  assert.equal(screen.calls.length, 0, 'nothing sent while locked');

  typeInto(win.querySelector('#nas-retype'), 'tank/home@daily-2026-08-3');
  assert.ok(confirmButton(win).hasAttribute('disabled'), 'partial name keeps it locked');
  confirmWindow(win);
  await flush();
  assert.equal(screen.calls.length, 0);

  typeInto(win.querySelector('#nas-retype'), target.name);
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
  typeInto(win.querySelector('#nas-retype'), target.name);
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
  typeInto(win.querySelector('#nas-retype'), target.name);
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(screen.calls.length, 0);
  assert.ok(document.querySelector('tf-window'), 'still open');
  assert.ok(!confirmButton(win).hasAttribute('disabled'));
  win.remove();
  screen.dispose();
});
