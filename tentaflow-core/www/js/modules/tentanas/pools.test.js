// =============================================================================
// File: modules/tentanas/pools.test.js
// Description: The pools tab against a fake screen: one card per pool from
// PoolsListResponse with the health/state/layout chips and the capacity
// split, the free-disk strip (spares carry the media badge from the disk
// inventory), the empty state when no pool exists, and the card click opening
// the pool. Runs under happy-dom with the `/js/` hook.
// =============================================================================

import { fakeScreen, flush, click } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawPools } = await import('./pools.js');

const TB = 1024 ** 4;

function pool(overrides) {
  return {
    name: 'tank', guid: '1', state: 'online', health: 'ok', healthReason: '', layout: 'raidz1', dataDisks: 4, faultTolerance: 1,
    sizeBytes: 16 * TB, usableBytes: 12 * TB, usedBytes: 3 * TB, compression: 'zstd', compressRatio: 1.42, encryption: 'off',
    datasetCount: 5, snapshotCount: 40, lastScrubAt: '2026-08-30 02:00:00', nextScrubAt: '2026-09-06 02:00:00',
    scrubSchedule: { every: 'weekly', hour: 2, minute: 0, weekday: 0, day: 1 }, scan: { kind: 'none', status: 'idle', progressPct: 0, errors: 0 },
    vdevs: [],
    ...overrides,
  };
}

const freeDisk = { diskId: 'sde', name: 'sde', kind: 'hdd', model: 'WD Red', serial: 'WD-5', sizeBytes: 4 * TB, health: 'ok', healthReason: '' };
const member = (name) => ({ diskId: name, name, sizeBytes: 4 * TB, state: 'online', health: 'ok', healthReason: '' });
// The shelf reads the media kind of a spare from the node's disk inventory.
const inventory = { disks: [freeDisk, { diskId: 'sdf', name: 'sdf', kind: 'nvme', sizeBytes: 4 * TB, health: 'ok' }] };
const tankVdevs = [
  { name: 'raidz1-0', kind: 'raidz1', role: 'data', state: 'online', disks: ['sda', 'sdb', 'sdc', 'sdd'].map(member) },
  { name: 'spare', kind: 'spare', role: 'spare', state: 'online', disks: [member('sdf')] },
];

function mount() {
  const body = document.createElement('div');
  document.body.appendChild(body);
  return body;
}

test('renders one card per pool with chips, capacity split and the free-disk strip', async () => {
  const screen = fakeScreen({
    tentaNasPoolsListRequest: {
      pools: [pool({ vdevs: tankVdevs }), pool({ name: 'backup', health: 'warning', healthReason: 'one disk reports pending sectors', state: 'degraded', layout: 'mirror', dataDisks: 2, scan: { kind: 'scrub', status: 'running', progressPct: 37, errors: 0 } })],
      freeDisks: [freeDisk],
    },
    tentaNasDisksListRequest: inventory,
  });
  const body = mount();
  await drawPools(screen, body);
  await flush();

  assert.deepEqual(screen.calls.map((c) => c.kind).sort(), ['tentaNasDisksListRequest', 'tentaNasPoolsListRequest']);
  const cards = [...body.querySelectorAll('.pool-card[data-pool]')];
  assert.deepEqual(cards.map((c) => c.dataset.pool), ['tank', 'backup']);

  const tank = cards[0];
  assert.match(tank.querySelector('.pc-cap .v').textContent, /3\.0 TiB \/ 12 TiB użyteczne \(25%\)/);
  const chips = [...tank.querySelectorAll('.pc-head tf-chip')].map((c) => c.getAttribute('label'));
  assert.ok(chips.includes('ZFS RAIDZ1'), `layout chip present: ${chips.join(' | ')}`);
  assert.match(tank.querySelector('.pc-desc').textContent, /^4×4\.0 TiB \+ hot-spare · odporność: 1 dysk$/);
  assert.match(tank.querySelector('.stat-rows').textContent, /4 danych \+ spare/);
  assert.equal(tank.querySelector('.pc-reason'), null, 'healthy pool has no reason line');
  assert.ok(tank.querySelector('[data-act="scrub"]'), 'idle pool offers scrub');
  assert.ok(tank.querySelector('[data-act="details"]'), 'card offers the details button');
  assert.ok(tank.querySelector('[data-act="more"]'), 'card offers the more menu');

  const backup = cards[1];
  assert.match(backup.querySelector('.pc-reason').textContent, /pending sectors/);
  assert.ok(backup.querySelector('[data-act="pause"]'), 'running scrub offers pause');
  assert.ok([...backup.querySelectorAll('tf-chip')].some((c) => /37%/.test(c.getAttribute('label') || '')), 'scan progress chip');

  assert.equal(body.querySelector('#nas-pools-count').getAttribute('label'), '2');
  assert.equal(body.querySelector('#nas-pools-sub'), null, 'n05 head is title + chip + the two buttons only');
  const free = body.querySelector('#nas-free-card');
  assert.equal(free.hidden, false);
  assert.equal(body.querySelector('#nas-free-count').getAttribute('label'), '2', 'free disk plus the hot-spare');
  assert.equal(body.querySelectorAll('#nas-free-cells .disk-cell[data-disk]').length, 2);
  const spare = body.querySelector('#nas-free-cells .disk-cell.spare[data-disk="sdf"]');
  assert.match(spare.textContent, /hot-spare \(tank\)/);
  assert.equal(spare.querySelector('.disk-kind').textContent, 'nvme', 'the spare cell carries the media badge');
  assert.match(body.querySelector('#nas-free-hint').textContent, /hot-spare .* w tank/);
  assert.ok(body.querySelector('#nas-free-cells .disk-cell.empty[data-act="create"]'), 'free disk offers the wizard cell');
  screen.dispose();
});

test('without spares the free-disk hint counts the free disks and their size', async () => {
  const screen = fakeScreen({ tentaNasPoolsListRequest: { pools: [pool()], freeDisks: [freeDisk] }, tentaNasDisksListRequest: inventory });
  const body = mount();
  await drawPools(screen, body);
  await flush();
  assert.equal(body.querySelectorAll('#nas-free-cells .disk-cell.spare').length, 0);
  assert.match(body.querySelector('#nas-free-hint').textContent, /^1 dysk · 4\.0 TiB$/);
  screen.dispose();
});

test('an empty node shows the empty state and hides the free-disk strip', async () => {
  const screen = fakeScreen({ tentaNasPoolsListRequest: { pools: [], freeDisks: [] }, tentaNasDisksListRequest: { disks: [] } });
  const body = mount();
  await drawPools(screen, body);
  await flush();
  assert.equal(body.querySelectorAll('.pool-card').length, 0);
  assert.ok(body.querySelector('#nas-pools-list tf-empty-state'), 'empty state rendered');
  assert.equal(body.querySelector('#nas-free-card').hidden, true);
  screen.dispose();
});

test('clicking a card opens the pool; the scrub button starts a scrub instead', async () => {
  const screen = fakeScreen({
    tentaNasPoolsListRequest: { pools: [pool()], freeDisks: [] },
    tentaNasDisksListRequest: { disks: [] },
    tentaNasPoolScrubRequest: { job: { jobId: 'job-1', kind: 'pool_scrub', status: 'running' } },
  });
  const body = mount();
  await drawPools(screen, body);
  await flush();
  click(body.querySelector('.pool-card[data-pool="tank"] .pc-name'));
  assert.deepEqual(screen.openedPools.map((o) => o.name), ['tank']);

  click(body.querySelector('.pool-card[data-pool="tank"] [data-act="scrub"]'));
  await flush();
  await flush();
  const scrub = screen.calls.find((c) => c.kind === 'tentaNasPoolScrubRequest');
  assert.ok(scrub, 'scrub request sent');
  assert.equal(scrub.payload.name, 'tank');
  assert.equal(scrub.payload.action, 'start');
  assert.equal(scrub.payload.sudoPassword, 'hunter2');
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['job-1'], 'job answer opens the log');
  assert.equal(screen.openedPools.length, 1, 'the scrub click did not open the pool');
  screen.dispose();
});

test('a list failure leaves the tab standing instead of throwing', async () => {
  const screen = fakeScreen({ tentaNasPoolsListRequest: () => { throw new Error('zpool unavailable'); }, tentaNasDisksListRequest: { disks: [] } });
  const body = mount();
  await drawPools(screen, body);
  await flush();
  assert.equal(body.querySelectorAll('.pool-card').length, 0);
  assert.equal(body.querySelector('#nas-pools-count').getAttribute('label'), '0');
  screen.dispose();
});
