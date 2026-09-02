// =============================================================================
// File: modules/tentanas/pools.test.js
// Description: The pools tab against a fake screen: one card per pool from
// PoolsListResponse with the health/state/layout chips and the capacity
// split, the free-disk strip, the empty state when no pool exists, and the
// card click opening the pool. Runs under happy-dom with the `/js/` hook.
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

function mount() {
  const body = document.createElement('div');
  document.body.appendChild(body);
  return body;
}

test('renders one card per pool with chips, capacity split and the free-disk strip', async () => {
  const screen = fakeScreen({
    tentaNasPoolsListRequest: {
      pools: [pool(), pool({ name: 'backup', health: 'warning', healthReason: 'one disk reports pending sectors', state: 'degraded', layout: 'mirror', dataDisks: 2, scan: { kind: 'scrub', status: 'running', progressPct: 37, errors: 0 } })],
      freeDisks: [freeDisk],
    },
  });
  const body = mount();
  await drawPools(screen, body);
  await flush();

  assert.equal(screen.calls[0].kind, 'tentaNasPoolsListRequest');
  const cards = [...body.querySelectorAll('.pool-card[data-pool]')];
  assert.deepEqual(cards.map((c) => c.dataset.pool), ['tank', 'backup']);

  const tank = cards[0];
  assert.match(tank.querySelector('.pc-cap .v').textContent, /3\.0 TiB \/ 12 TiB · 25%/);
  const chips = [...tank.querySelectorAll('.pc-head tf-chip')].map((c) => c.getAttribute('label'));
  assert.ok(chips.some((l) => /RAIDZ1 · 4×/.test(l)), `layout chip present: ${chips.join(' | ')}`);
  assert.equal(tank.querySelector('.pc-reason'), null, 'healthy pool has no reason line');
  assert.ok(tank.querySelector('[data-act="scrub"]'), 'idle pool offers scrub');

  const backup = cards[1];
  assert.match(backup.querySelector('.pc-reason').textContent, /pending sectors/);
  assert.ok(backup.querySelector('[data-act="pause"]'), 'running scrub offers pause');
  assert.ok([...backup.querySelectorAll('tf-chip')].some((c) => /37%/.test(c.getAttribute('label') || '')), 'scan progress chip');

  assert.match(body.querySelector('#nas-pools-sub').textContent, /2/);
  const free = body.querySelector('#nas-free-card');
  assert.equal(free.hidden, false);
  assert.equal(body.querySelectorAll('#nas-free-cells .disk-cell[data-disk]').length, 1);
  assert.match(body.querySelector('#nas-free-hint').textContent, /4\.0 TiB/);
  screen.dispose();
});

test('an empty node shows the empty state and hides the free-disk strip', async () => {
  const screen = fakeScreen({ tentaNasPoolsListRequest: { pools: [], freeDisks: [] } });
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

test('a list failure lands in the subtitle instead of throwing', async () => {
  const screen = fakeScreen({ tentaNasPoolsListRequest: () => { throw new Error('zpool unavailable'); } });
  const body = mount();
  await drawPools(screen, body);
  await flush();
  assert.match(body.querySelector('#nas-pools-sub').textContent, /zpool unavailable/);
  screen.dispose();
});
