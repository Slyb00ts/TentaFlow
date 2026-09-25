// =============================================================================
// File: modules/tentanas/destroy-dialog.test.js
// Description: The destroy-pool dialog (retype gate) against a fake screen:
// the loss list names the datasets and freed disks, the danger button stays
// locked until the pool name is retyped exactly, a confirm while locked
// sends nothing, and the armed confirm sends PoolDestroyRequest with the
// confirm name and follows the job. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, typeInto, confirmWindow } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openPoolDestroyDialog } = await import('./pool-detail.js');

const TB = 1024 ** 4;
const pool = {
  name: 'tank', state: 'online', health: 'ok', layout: 'raidz1', dataDisks: 3, usedBytes: 3 * TB, datasetCount: 2, snapshotCount: 12,
  vdevs: [{ id: 'raidz1-0', role: 'data', kind: 'raidz1', state: 'online', disks: [{ name: 'sda' }, { name: 'sdb' }, { name: 'sdc' }] }],
};
const datasets = [
  { name: 'tank/media', usedBytes: 2 * TB },
  { name: 'tank/home', usedBytes: TB },
];

const confirmButton = (win) => win.querySelector('tf-button[data-action="confirm"]');
// The dialog also reads the share and target lists on open (what dies with
// the pool); only the destroy request is what these tests count.
const destroys = (screen) => screen.calls.filter((c) => c.kind === 'tentaNasPoolDestroyRequest');

test('lists what is lost and keeps the danger button locked until the exact name', async () => {
  const screen = fakeScreen({ tentaNasPoolDestroyRequest: { job: { jobId: 'job-9', kind: 'pool_destroy', status: 'running' } } });
  const win = openPoolDestroyDialog(screen, pool, datasets, () => {});
  await flush();
  const lost = [...win.querySelectorAll('.loss-list li')].map((li) => li.textContent);
  assert.equal(lost.length, 3, 'two datasets plus the snapshot count');
  assert.match(lost[0], /tank\/media — 2\.0 TiB/);
  assert.match(lost[2], /12 snapshotów/);
  assert.match(win.querySelector('.explain-box').textContent, /3 dyskach RAIDZ1 \(sda, sdb, sdc\)/);
  assert.match(win.querySelector('.wizard-warning.danger').textContent, /NIEODWRACALNA/);

  const btn = confirmButton(win);
  assert.ok(btn.hasAttribute('disabled'), 'locked before typing');
  const input = win.querySelector('#retype-input');
  typeInto(input, 'tan');
  assert.ok(btn.hasAttribute('disabled'), 'partial name keeps it locked');
  typeInto(input, 'Tank');
  assert.ok(btn.hasAttribute('disabled'), 'case matters');

  confirmWindow(win);
  await flush();
  assert.equal(destroys(screen).length, 0, 'nothing sent while locked');

  typeInto(input, 'tank');
  assert.ok(!btn.hasAttribute('disabled'), 'exact name unlocks');
  typeInto(input, ' tank ');
  assert.ok(!btn.hasAttribute('disabled'), 'surrounding whitespace is ignored');
  win.remove();
  screen.dispose();
});

test('the pool root is the subject of the modal, not a loss row, and the overflow counts children only', async () => {
  const screen = fakeScreen({ tentaNasPoolDestroyRequest: {} });
  const many = [
    { name: 'tank', usedBytes: 10 * TB },
    ...Array.from({ length: 9 }, (_, i) => ({ name: `tank/d${i}`, usedBytes: TB })),
  ];
  const win = openPoolDestroyDialog(screen, pool, many, () => {});
  await flush();
  const lost = [...win.querySelectorAll('.loss-list li')].map((li) => li.textContent);
  assert.ok(!lost.some((l) => /\btank\b —/.test(l)), 'the pool root is not listed as its own child');
  assert.equal(lost.length, 10, '8 children + the overflow row + the snapshot row');
  assert.match(lost[8], /i 1 więcej/, 'only the 9th child overflows');
  win.remove();
  screen.dispose();
});

test('an armed confirm sends PoolDestroyRequest with the confirm name and follows the job', async () => {
  const screen = fakeScreen({ tentaNasPoolDestroyRequest: { job: { jobId: 'job-9', kind: 'pool_destroy', status: 'running' } } });
  let done = 0;
  const win = openPoolDestroyDialog(screen, pool, datasets, () => { done += 1; });
  await flush();
  typeInto(win.querySelector('#retype-input'), 'tank');
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(destroys(screen).length, 1);
  assert.deepEqual(destroys(screen)[0].payload, { name: 'tank', confirmName: 'tank', sudoPassword: 'hunter2' });
  assert.equal(screen.jobLogs.length, 1, 'the job log opened');
  assert.equal(screen.jobLogs[0].jobId, 'job-9');
  assert.equal(done, 0, 'onDone waits for the job to finish');
  screen.jobLogs[0].onFinish();
  assert.equal(done, 1);
  await new Promise((r) => setTimeout(r, 300));
  assert.equal(document.querySelector('tf-window'), null, 'dialog closed');
  screen.dispose();
});

test('a cancelled sudo prompt keeps the dialog open and armed', async () => {
  const screen = fakeScreen({ tentaNasPoolDestroyRequest: {} }, { sudo: null });
  const win = openPoolDestroyDialog(screen, pool, datasets, () => {});
  await flush();
  typeInto(win.querySelector('#retype-input'), 'tank');
  confirmWindow(win);
  await flush();
  await flush();
  assert.equal(destroys(screen).length, 0);
  assert.ok(document.querySelector('tf-window'), 'still open');
  assert.ok(!confirmButton(win).hasAttribute('disabled'), 'retry possible');
  win.remove();
  screen.dispose();
});

test('a failed destroy shows the error under the retype field and unlocks again', async () => {
  const screen = fakeScreen({ tentaNasPoolDestroyRequest: () => { throw new Error('pool is busy'); } });
  const win = openPoolDestroyDialog(screen, pool, datasets, () => {});
  await flush();
  typeInto(win.querySelector('#retype-input'), 'tank');
  confirmWindow(win);
  await flush();
  await flush();
  const err = win.querySelector('#retype-error');
  assert.equal(err.hidden, false);
  assert.match(err.textContent, /pool is busy/);
  assert.ok(!confirmButton(win).hasAttribute('disabled'));
  win.remove();
  screen.dispose();
});

// n17a (M15): the shares and block targets whose data lives on the pool die
// with it, and the mockup lists each one. They come from this organisation's
// own lists; a share or target elsewhere is not named, and a list that cannot
// be read adds nothing.
test('the loss list names every share and block target that lives on the pool', async () => {
  const screen = fakeScreen({
    tentaNasSharesListRequest: {
      shares: [
        { name: 'projekty', protocol: 'smb', dataset: 'tank/media', sourcePath: '/tank/media' },
        { name: 'backups', protocol: 'nfs', dataset: null, sourcePath: '/tank/home/backups' },
        { name: 'inne', protocol: 'smb', dataset: 'tankless/x', sourcePath: '/tankless/x' },
      ],
    },
    tentaNasTargetsListRequest: {
      targets: [
        { name: 'vm-store', protocol: 'iscsi', luns: [{ source: 'tank/vm-store', sourceKind: 'zvol' }] },
        { name: 'elsewhere', protocol: 'nvmet', luns: [{ source: 'fast/vm', sourceKind: 'zvol' }] },
      ],
    },
  });
  const onPool = [
    { name: 'tank/media', usedBytes: 2 * TB, mountpoint: '/tank/media' },
    { name: 'tank/home', usedBytes: TB, mountpoint: '/tank/home' },
  ];
  const win = openPoolDestroyDialog(screen, pool, onPool, () => {});
  await flush();
  await flush();
  const deps = [...win.querySelectorAll('[data-role="dependents"] li')].map((li) => li.textContent);
  assert.deepEqual(deps, [
    'tank/media — share SMB „projekty” przestanie działać',
    '/tank/home/backups — share NFS „backups” przestanie działać',
    'tank/vm-store — target iSCSI „vm-store” przestanie działać',
  ]);
  win.remove();
  screen.dispose();
});
