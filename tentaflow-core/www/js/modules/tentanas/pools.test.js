// =============================================================================
// File: modules/tentanas/pools.test.js
// Description: The pools tab against a fake screen: one card per pool from
// PoolsListResponse with the health/state/layout chips and the capacity
// split, the free-disk strip (spares carry the media badge from the disk
// inventory), the empty state when no pool exists, and the card click opening
// the pool. Runs under happy-dom with the `/js/` hook.
// =============================================================================

import { fakeScreen as makeScreen, flush, click } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawPools } = await import('./pools.js');

function fakeScreen(fixtures, options) {
  return makeScreen({
    tentaNasElasticArraysListRequest: { arrays: [] },
    tentaNasElasticCapabilitiesRequest: { freeDisks: fixtures.tentaNasPoolsListRequest?.freeDisks || [] },
    ...fixtures,
  }, options);
}

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

const elastic = { name: 'media', kind: 'elastic-array', filesystem: 'xfs', state: 'active', unionPath: '/mnt/media', usableBytes: null, usedBytes: null, dataDisks: [], parityDisks: [], protection: { status: 'unprotected' } };

for (const failed of ['zfs', 'elastic']) {
  test(`błąd ${failed} nie ukrywa niezależnego wyniku drugiej listy`, async () => {
    const failure = () => { throw new Error(`Brak odczytu ${failed}`); };
    const screen = fakeScreen({
      tentaNasPoolsListRequest: failed === 'zfs' ? failure : { pools: [pool()] },
      tentaNasElasticArraysListRequest: failed === 'elastic' ? failure : { arrays: [elastic] },
      tentaNasDisksListRequest: inventory,
    });
    const body = mount();
    await drawPools(screen, body);
    await flush();
    assert.equal(body.querySelectorAll(failed === 'zfs' ? '[data-array="media"]' : '[data-pool="tank"]').length, 1);
    assert.equal(body.querySelectorAll('tf-empty-state').length, 0);
    assert.equal(body.querySelector('#nas-pools-count').getAttribute('label'), '1 + ?');
    assert.match(body.querySelector('#nas-pools-errors').innerHTML, /Brak odczytu/);
    screen.dispose();
  });
}

test('wolny ZFS nie blokuje Elastic, a spóźniona lista nie odmalowuje nowej powierzchni', async () => {
  let release;
  const screen = fakeScreen({
    tentaNasPoolsListRequest: () => new Promise((resolve) => { release = resolve; }),
    tentaNasElasticArraysListRequest: { arrays: [elastic] },
    tentaNasDisksListRequest: inventory,
  });
  const body = mount();
  const pending = drawPools(screen, body);
  await flush();
  assert.equal(screen.calls.filter((call) => call.kind === 'tentaNasPoolsListRequest').length, 1);
  assert.ok(body.querySelector('[data-array="media"]'));
  let opened;
  screen.openArray = (name) => { opened = name; };
  click(body.querySelector('[data-array="media"] tf-button'));
  assert.equal(opened, 'media');
  body.textContent = 'nowy widok';
  release({ pools: [pool()] });
  await pending;
  assert.equal(body.textContent, 'nowy widok');
  screen.dispose();
});

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

  assert.deepEqual(screen.calls.map((c) => c.kind).sort(), ['tentaNasDisksListRequest', 'tentaNasElasticArraysListRequest', 'tentaNasElasticCapabilitiesRequest', 'tentaNasPoolsListRequest']);
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
  assert.equal(body.querySelector('#nas-pools-count').getAttribute('label'), '0 + ?');
  assert.equal(body.querySelector('tf-empty-state'), null);
  assert.equal(body.querySelector('#nas-pools-errors tf-alert').getAttribute('message'), 'zpool unavailable');
  screen.dispose();
});

// The tab polls every 5 s. Rebuilding the cards on an unchanged answer closes
// a card's menu under the admin's hand and drops the hover off its buttons.
test('an unchanged poll leaves the pool cards and the free-disk shelf alone', async () => {
  const screen = fakeScreen({
    tentaNasPoolsListRequest: { pools: [pool({ vdevs: tankVdevs })], freeDisks: [freeDisk] },
    tentaNasDisksListRequest: inventory,
  });
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = mount();
  await drawPools(screen, body);
  await flush();
  const cards = [...body.querySelectorAll('.pool-card')];
  const cells = [...body.querySelectorAll('#nas-free-cells .disk-cell')];
  assert.equal(cards.length, 1);
  assert.ok(cells.length, 'the shelf has cells');

  assert.ok(scheduled.length, 'the tab armed its poll');
  await scheduled[0]();
  await flush();
  [...body.querySelectorAll('.pool-card')].forEach((el, i) => assert.equal(el === cards[i], true, `pool card ${i} survives the poll`));
  [...body.querySelectorAll('#nas-free-cells .disk-cell')].forEach((el, i) => assert.equal(el === cells[i], true, `free-disk cell ${i} survives the poll`));
  screen.dispose();
});

// B3 (critic-mockups-n01-n10-2026-09-21.md): a scrub's own % and "za N min"
// used to sit INSIDE the joined string the whole list was compared against,
// so a scan ticking rebuilt every card, its menu button and any open
// `tf-menu` at least once a minute, and every 5 s during a scrub. The card
// must instead be a stable skeleton keyed by pool name, with only the moving
// values patched into it.
test('two polls with a changed scrub % and a changed next-scrub time keep the card, its menu button and an open tf-menu the same nodes; the values update', async () => {
  const inMs = (ms) => new Date(Date.now() + ms).toISOString().slice(0, 19).replace('T', ' ');
  let call = 0;
  const scans = [
    { kind: 'scrub', status: 'running', progressPct: 10, errors: 0 },
    { kind: 'scrub', status: 'running', progressPct: 55, errors: 0 },
  ];
  // Minutes away, then hours away: `fmtIn` buckets these into visibly
  // different Polish text ("za N min" vs "za N godz"), so the assertion does
  // not depend on exact wording, only on the text having actually moved.
  const nextScrubs = [inMs(5 * 60 * 1000), inMs(3 * 3600 * 1000)];
  const screen = fakeScreen({
    tentaNasPoolsListRequest: () => ({ pools: [pool({ scan: scans[call], nextScrubAt: nextScrubs[call] })], freeDisks: [] }),
    tentaNasDisksListRequest: { disks: [] },
  });
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = mount();
  await drawPools(screen, body);
  await flush();

  const card = body.querySelector('.pool-card[data-pool="tank"]');
  const moreBtn = card.querySelector('[data-act="more"]');
  const pauseBtn = card.querySelector('[data-act="pause"]');
  const menu = card.querySelector('tf-menu');
  assert.ok(card && moreBtn && pauseBtn && menu, 'the running scan offers its card, more button, pause button and menu');
  menu.anchor = moreBtn;
  menu.open();
  assert.ok(menu.hasAttribute('open'), 'the menu opened');
  const scanChipBefore = [...card.querySelectorAll('tf-chip')].find((c) => /%/.test(c.getAttribute('label') || ''));
  assert.match(scanChipBefore.getAttribute('label'), /10%/);
  const nextScrubTextBefore = card.querySelector('.text-3').textContent;

  call = 1;
  assert.ok(scheduled.length, 'the tab armed its poll');
  await scheduled[0]();
  await flush();

  assert.ok(body.querySelector('.pool-card[data-pool="tank"]') === card, 'the card is the SAME node after the scrub %/time changed');
  assert.ok(card.querySelector('[data-act="more"]') === moreBtn, 'the more button is the SAME node');
  assert.ok(card.querySelector('[data-act="pause"]') === pauseBtn, 'the scan-action button is the SAME node');
  assert.ok(card.querySelector('tf-menu') === menu, 'the tf-menu is the SAME node');
  assert.ok(menu.hasAttribute('open'), 'the open menu is still open after the poll');
  const scanChipAfter = [...card.querySelectorAll('tf-chip')].find((c) => /%/.test(c.getAttribute('label') || ''));
  assert.match(scanChipAfter.getAttribute('label'), /55%/, 'the scan % actually updated');
  assert.notEqual(card.querySelector('.text-3').textContent, nextScrubTextBefore, 'the next-scrub time actually updated');
  screen.dispose();
});

// B3: the old whole-list `patchHtml` rebuilt every card whenever ANY pool
// changed, so adding or removing one pool tore down every sibling's menu and
// buttons too.
test('a pool added or removed changes only that card', async () => {
  let call = 0;
  const sets = [
    [pool({ name: 'tank' }), pool({ name: 'backup' })],
    [pool({ name: 'tank' }), pool({ name: 'backup' }), pool({ name: 'extra' })],
    [pool({ name: 'tank' }), pool({ name: 'extra' })],
  ];
  const screen = fakeScreen({
    tentaNasPoolsListRequest: () => ({ pools: sets[call], freeDisks: [] }),
    tentaNasDisksListRequest: { disks: [] },
  });
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = mount();
  await drawPools(screen, body);
  await flush();
  const tankCard = body.querySelector('.pool-card[data-pool="tank"]');
  const backupCard = body.querySelector('.pool-card[data-pool="backup"]');
  assert.ok(tankCard && backupCard, 'both pools start with a card');

  call = 1;
  assert.ok(scheduled.length, 'the tab armed its poll');
  await scheduled[0]();
  await flush();
  assert.ok(body.querySelector('.pool-card[data-pool="tank"]') === tankCard, 'tank keeps its card when a pool is added');
  assert.ok(body.querySelector('.pool-card[data-pool="backup"]') === backupCard, 'backup keeps its card when a pool is added');
  const extraCard = body.querySelector('.pool-card[data-pool="extra"]');
  assert.ok(extraCard, 'the new pool gets its own card');

  call = 2;
  await scheduled[0]();
  await flush();
  assert.equal(body.querySelector('.pool-card[data-pool="backup"]'), null, 'the removed pool loses its card');
  assert.ok(body.querySelector('.pool-card[data-pool="tank"]') === tankCard, 'tank keeps its card when a pool is removed');
  assert.ok(body.querySelector('.pool-card[data-pool="extra"]') === extraCard, 'extra keeps its card when a pool is removed');
  screen.dispose();
});

// M2 (critic-round2-wave1-2026-09-22.md): the Elastic Array card used to be
// built by `elasticCardHtml`, which baked used bytes, the last sync and the
// state into the returned string, so ANY of those changing (which happens on
// every write to the array) gave `patchKeyedList` a fresh string and rebuilt
// the whole card, "Szczegóły" button included. It is now a skeleton
// (`elasticCardSkeletonHtml`) painted in place by `paintElasticCard`, the
// same split as a ZFS pool card.
const GiB = 1024 ** 3;
function elasticArray(overrides) {
  return {
    name: 'media', kind: 'elastic-array', filesystem: 'xfs', state: 'active', stateDetail: '',
    unionPath: '/mnt/media', usableBytes: 20 * GiB, usedBytes: 5 * GiB,
    dataDisks: [{ sizeBytes: 10 * GiB }, { sizeBytes: 10 * GiB }],
    parityDisks: [{ sizeBytes: 10 * GiB }],
    protection: { status: 'protected', protectedAsOf: '2026-09-01 02:00:00' },
    ...overrides,
  };
}

test('two polls with changed used bytes, last sync and state keep the Elastic Array card and its "Szczegóły" button the same nodes; the values update', async () => {
  let call = 0;
  const versions = [
    elasticArray({ usedBytes: 5 * GiB, state: 'active', protection: { status: 'protected', protectedAsOf: '2026-09-01 02:00:00' } }),
    elasticArray({ usedBytes: 12 * GiB, state: 'needs_attention', protection: { status: 'unprotected', protectedAsOf: '2026-09-07 02:00:00' } }),
  ];
  const screen = fakeScreen({
    tentaNasPoolsListRequest: { pools: [], freeDisks: [] },
    tentaNasElasticArraysListRequest: () => ({ arrays: [versions[call]] }),
    tentaNasDisksListRequest: { disks: [] },
  });
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = mount();
  await drawPools(screen, body);
  await flush();

  const card = body.querySelector('.pool-card[data-array="media"]');
  const detailsBtn = card.querySelector('[data-act="array-details"]');
  assert.ok(card && detailsBtn, 'the array card and its details button are on screen');
  assert.match(card.querySelector('[data-f="cap-value"]').textContent, /5\.0 GiB/, 'used bytes painted');
  const stateLabelBefore = card.querySelector('[data-f="state"]').getAttribute('label');
  const lastSyncBefore = card.querySelector('[data-f="last-sync"]').textContent;

  call = 1;
  assert.ok(scheduled.length, 'the tab armed its poll');
  await scheduled[0]();
  await flush();

  assert.ok(body.querySelector('.pool-card[data-array="media"]') === card, 'the array card is the SAME node after used bytes/sync/state changed');
  assert.ok(card.querySelector('[data-act="array-details"]') === detailsBtn, 'the "Szczegóły" button is the SAME node');
  assert.match(card.querySelector('[data-f="cap-value"]').textContent, /12 GiB/, 'used bytes actually updated');
  assert.notEqual(card.querySelector('[data-f="state"]').getAttribute('label'), stateLabelBefore, 'the state chip label actually updated');
  assert.notEqual(card.querySelector('[data-f="last-sync"]').textContent, lastSyncBefore, 'the last-sync date actually updated');
  screen.dispose();
});
