// =============================================================================
// File: modules/tentanas/pool-wizard.test.js
// Description: The pool wizard step flow against a fake screen: the disk step
// stays locked until a disk is checked, moving on asks the node for a plan
// with exactly the checked disk ids, the layout step preselects the
// recommended option and the summary's retype gate guards the create
// request, which carries the chosen layout and options. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, typeInto, window, windowTitle } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openPoolWizard, poolNameValid } = await import('./pool-wizard.js');

const TB = 1024 ** 4;
const disk = (id, overrides = {}) => ({ diskId: id, name: id, kind: 'hdd', model: 'WD Red', serial: `WD-${id}`, sizeBytes: 4 * TB, health: 'ok', healthReason: '', ...overrides });
const freeDisks = [disk('sda'), disk('sdb'), disk('sdc'), disk('sdd', { health: 'critical', healthReason: 'SMART failed' })];

const plan = {
  options: [
    { layout: 'stripe', available: true, reason: '', usableBytes: 12 * TB, rawBytes: 12 * TB, faultTolerance: 0, recommended: false },
    { layout: 'mirror', available: true, reason: '', usableBytes: 4 * TB, rawBytes: 12 * TB, faultTolerance: 2, recommended: false },
    { layout: 'raidz1', available: true, reason: '', usableBytes: 8 * TB, rawBytes: 12 * TB, faultTolerance: 1, recommended: true },
    { layout: 'raidz2', available: false, reason: 'too_few_disks', usableBytes: 0, rawBytes: 12 * TB, faultTolerance: 2, recommended: false },
  ],
  warnings: [],
  smallestDiskBytes: 4 * TB,
};

const nextBtn = (win) => win.querySelector('[data-wizard-next]');
const backBtn = (win) => win.querySelector('[data-wizard-back]');

function checkDisk(win, id) {
  const cell = win.querySelector(`.disk-cell[data-disk="${id}"]`);
  const cb = cell.querySelector('tf-checkbox');
  cb.checked = true;
  cb.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked: true } }));
}

test('pool names follow the zpool rules', () => {
  assert.ok(poolNameValid('tank'));
  assert.ok(poolNameValid('data_2026'));
  assert.ok(!poolNameValid('1tank'), 'must start with a letter');
  assert.ok(!poolNameValid('my pool'), 'no spaces');
  assert.ok(!poolNameValid('mirror'), 'reserved vdev words are rejected');
  assert.ok(!poolNameValid('raidz1'));
  assert.ok(!poolNameValid('log'));
});

test('n07/n08: the window title names the pool kind, the h1 stays "Nowa pula"', async () => {
  const screen = fakeScreen({ tentaNasPoolPlanRequest: plan });
  const win = openPoolWizard(screen, { freeDisks });
  await flush();
  assert.equal(windowTitle(win), 'Nowa pula', 'the kind is not chosen yet');
  assert.match(win.querySelector('.install-header h1').textContent, /^Nowa pula/);
  click(nextBtn(win));
  await flush();
  assert.equal(windowTitle(win), 'Nowa pula — ZFS');
  assert.match(win.querySelector('.install-header h1').textContent, /^Nowa pula/);
  win.remove();
  screen.dispose();
});

test('the wizard plans with the checked disk ids and creates with the chosen layout', async () => {
  let done = null;
  const screen = fakeScreen({
    tentaNasPoolPlanRequest: plan,
    tentaNasPoolCreateRequest: { job: { jobId: 'job-7', kind: 'pool_create', status: 'running', progressPct: 0, log: [] } },
    tentaNasJobGetRequest: { job: { jobId: 'job-7', kind: 'pool_create', status: 'succeeded', progressPct: 100, log: ['zpool create ok'] } },
  });
  const win = openPoolWizard(screen, { freeDisks, onDone: (job) => { done = job; } });
  await flush();

  // Step 1: ZFS is preselected, so Next is live.
  assert.equal(win.querySelector('#nas-pw-kind').getAttribute('value'), 'zfs');
  assert.ok(!nextBtn(win).hasAttribute('disabled'));
  click(nextBtn(win));
  await flush();

  // Step 2: nothing checked yet, the critical disk is blocked.
  assert.equal(win.querySelectorAll('#nas-pw-disks .disk-cell').length, 4);
  assert.ok(win.querySelector('.disk-cell[data-disk="sdd"]').classList.contains('disabled'));
  assert.ok(nextBtn(win).hasAttribute('disabled'), 'locked without disks');
  checkDisk(win, 'sda');
  checkDisk(win, 'sdb');
  checkDisk(win, 'sdc');
  assert.ok(!nextBtn(win).hasAttribute('disabled'));
  assert.match(win.querySelector('#nas-pw-selected').textContent, /3/);
  assert.equal(screen.calls.length, 0, 'no request before leaving the disk step');
  click(nextBtn(win));
  await flush();
  await flush();

  // Step 3: the plan came from the node with exactly the checked ids.
  const planCall = screen.calls.find((c) => c.kind === 'tentaNasPoolPlanRequest');
  assert.ok(planCall, 'plan requested');
  assert.deepEqual(planCall.payload, { diskIds: ['sda', 'sdb', 'sdc'] });
  const group = win.querySelector('#nas-pw-layout');
  assert.ok(group, 'layout cards rendered');
  assert.equal(group.getAttribute('value'), 'raidz1', 'recommended layout preselected');
  const cards = [...group.querySelectorAll('tf-choice-card')];
  assert.deepEqual(cards.map((c) => c.getAttribute('value')), ['stripe', 'mirror', 'raidz1', 'raidz2']);
  assert.ok(cards[3].hasAttribute('disabled'), 'unavailable layout is disabled');
  assert.ok(nextBtn(win).hasAttribute('disabled'), 'a name is still needed');

  group.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'mirror' } }));
  typeInto(win.querySelector('#nas-pw-name'), 'bad name');
  assert.ok(nextBtn(win).hasAttribute('disabled'), 'invalid name keeps it locked');
  assert.ok(win.querySelector('#nas-pw-name').hasAttribute('error'));
  typeInto(win.querySelector('#nas-pw-name'), 'tank');
  assert.ok(!nextBtn(win).hasAttribute('disabled'));
  click(nextBtn(win));
  await flush();

  // Step 4: the summary reflects the picks and the retype gate guards Create.
  assert.match(win.querySelector('.stat-rows').textContent, /tank/);
  assert.equal(win.querySelectorAll('.loss-list li').length, 3);
  assert.ok(nextBtn(win).hasAttribute('disabled'), 'create locked before retype');
  typeInto(win.querySelector('#nas-pw-confirm'), 'tan');
  assert.ok(nextBtn(win).hasAttribute('disabled'), 'partial name keeps it locked');
  click(nextBtn(win));
  await flush();
  assert.ok(!screen.calls.some((c) => c.kind === 'tentaNasPoolCreateRequest'), 'no create while locked');
  typeInto(win.querySelector('#nas-pw-confirm'), 'tank');
  assert.ok(!nextBtn(win).hasAttribute('disabled'));
  click(nextBtn(win));
  await flush();
  await flush();

  const create = screen.calls.find((c) => c.kind === 'tentaNasPoolCreateRequest');
  assert.ok(create, 'create sent');
  assert.deepEqual(create.payload, {
    name: 'tank', layout: 'mirror', diskIds: ['sda', 'sdb', 'sdc'], compression: 'zstd', encryption: false, sudoPassword: 'hunter2',
  }, 'ashift and autotrim are left to the node defaults');

  // The job is followed inside the wizard until it finishes.
  await new Promise((r) => setTimeout(r, 20));
  assert.ok(screen.calls.some((c) => c.kind === 'tentaNasJobGetRequest' && c.payload.jobId === 'job-7'));
  assert.ok(win.querySelector('.result-box.ok'), 'success shown');
  assert.match(win.querySelector('.job-log').textContent, /zpool create ok/);
  assert.equal(done && done.jobId, 'job-7');
  win.remove();
  screen.dispose();
});

test('members and spares of existing pools show up disabled with the reason on the cell', async () => {
  const member = (name) => ({ diskId: name, name, sizeBytes: 8 * TB, state: 'online', health: 'ok', healthReason: '' });
  const pools = [{
    name: 'backup', layout: 'mirror',
    vdevs: [
      { name: 'mirror-0', kind: 'mirror', role: 'data', state: 'online', disks: [member('sde'), member('sdf')] },
      { name: 'spare', kind: 'spare', role: 'spare', state: 'online', disks: [member('sdg')] },
    ],
  }];
  const screen = fakeScreen({ tentaNasPoolPlanRequest: plan });
  const win = openPoolWizard(screen, { freeDisks, pools });
  await flush();
  click(nextBtn(win));
  await flush();

  const cells = [...win.querySelectorAll('#nas-pw-disks .disk-cell')];
  assert.deepEqual(cells.map((c) => c.dataset.disk), ['sda', 'sdb', 'sdc', 'sdd', 'sde', 'sdf', 'sdg'], 'free disks first, then the occupied ones');
  const occupied = win.querySelector('.disk-cell[data-disk="sde"]');
  assert.ok(occupied.classList.contains('disabled'));
  assert.ok(occupied.querySelector('tf-checkbox').hasAttribute('disabled'));
  // n07:240 — the tooltip says why, the sub line says what claims the disk.
  assert.equal(occupied.getAttribute('title'), 'w puli backup (mirror) — niedostępne');
  assert.equal(occupied.querySelector('.dc-sub').textContent, 'zajęte: pula backup');
  const spare = win.querySelector('.disk-cell[data-disk="sdg"]');
  assert.ok(spare.classList.contains('disabled'));
  assert.equal(spare.getAttribute('title'), 'hot-spare puli backup — niedostępny');
  assert.equal(spare.querySelector('.dc-sub').textContent, 'hot-spare puli backup');
  assert.equal(win.querySelector('.disk-cell[data-disk="sdd"]').getAttribute('title'), 'SMART failed', 'a critical free disk carries its SMART reason');

  checkDisk(win, 'sde');
  assert.ok(nextBtn(win).hasAttribute('disabled'), 'an occupied disk cannot be picked');
  assert.equal(win.querySelectorAll('.disk-cell.checked').length, 0);
  win.remove();
  screen.dispose();
});

test('changing the disk selection drops the cached plan and Back returns to the disks', async () => {
  let plans = 0;
  const screen = fakeScreen({ tentaNasPoolPlanRequest: () => { plans += 1; return plan; } });
  const win = openPoolWizard(screen, { freeDisks });
  await flush();
  click(nextBtn(win));
  await flush();
  checkDisk(win, 'sda');
  checkDisk(win, 'sdb');
  click(nextBtn(win));
  await flush();
  await flush();
  assert.equal(plans, 1);
  click(backBtn(win));
  await flush();
  assert.ok(win.querySelector('#nas-pw-disks'), 'back on the disk step');
  assert.equal(win.querySelectorAll('.disk-cell.checked').length, 2, 'selection kept');
  checkDisk(win, 'sdc');
  click(nextBtn(win));
  await flush();
  await flush();
  assert.equal(plans, 2, 'a new selection asks for a new plan');
  assert.deepEqual(screen.calls.at(-1).payload, { diskIds: ['sda', 'sdb', 'sdc'] });
  win.remove();
  screen.dispose();
});

test('a cancelled sudo prompt leaves the summary step armed and sends nothing', async () => {
  const screen = fakeScreen({ tentaNasPoolPlanRequest: plan }, { sudo: null });
  const win = openPoolWizard(screen, { freeDisks });
  await flush();
  click(nextBtn(win));
  await flush();
  checkDisk(win, 'sda');
  checkDisk(win, 'sdb');
  click(nextBtn(win));
  await flush();
  await flush();
  typeInto(win.querySelector('#nas-pw-name'), 'tank');
  click(nextBtn(win));
  await flush();
  typeInto(win.querySelector('#nas-pw-confirm'), 'tank');
  click(nextBtn(win));
  await flush();
  await flush();
  assert.ok(!screen.calls.some((c) => c.kind === 'tentaNasPoolCreateRequest'));
  assert.ok(win.querySelector('#nas-pw-confirm'), 'still on the summary');
  assert.ok(!nextBtn(win).hasAttribute('disabled'), 'retry possible');
  win.remove();
  screen.dispose();
});
