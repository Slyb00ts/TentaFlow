// =============================================================================
// File: modules/tentanas/pool-detail.test.js
// Description: The pool detail view against a fake screen: the KPI row and
// topology come from PoolGetResponse (one group per vdev, one cell per
// disk, the add-vdev buttons for an admin), the topology tab also carries the
// pool properties and the danger zone (n06), the Właściwości tab renders the
// same two cards, the inner tabs load datasets and snapshots with the
// expected requests, and the danger zone opens the retype dialog. Runs under
// happy-dom.
// =============================================================================

import { fakeScreen, flush, click, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawPoolDetail, openAddVdevDialog, openReplaceWizard, isUnresolvedLeafName } = await import('./pool-detail.js');
const { KIND_BADGE } = await import('./format.js');

const TB = 1024 ** 4;
const disk = (name, overrides = {}) => ({ diskId: name, name, path: `/dev/${name}`, kind: 'hdd', model: 'WD Red', serial: `WD-${name}`, sizeBytes: 4 * TB, state: 'online', health: 'ok', readErrors: 0, writeErrors: 0, cksumErrors: 0, note: '', ...overrides });

const poolGet = {
  pool: {
    name: 'tank', guid: '42', state: 'online', health: 'ok', healthReason: '', layout: 'raidz1', dataDisks: 3, faultTolerance: 1,
    sizeBytes: 12 * TB, usableBytes: 8 * TB, usedBytes: 2 * TB, availableBytes: 6 * TB, fragmentationPct: 4, compression: 'zstd', compressRatio: 1.3, dedupRatio: 1,
    encryption: 'off', autotrim: false, ashift: 12, readOnly: false, datasetCount: 2, snapshotCount: 6, readErrors: 0, writeErrors: 0, cksumErrors: 0,
    lastScrubAt: '2026-08-30 02:00:00', nextScrubAt: '2026-09-06 02:00:00', scrubSchedule: { every: 'weekly', hour: 2, minute: 0, weekday: 0, day: 1 },
    scan: { kind: 'none', status: 'idle', progressPct: 0, errors: 0 }, io: { readBps: 0, writeBps: 0, readIops: 0, writeIops: 0 },
    vdevs: [
      { id: 'raidz1-0', role: 'data', kind: 'raidz1', state: 'online', faultTolerance: 1, disks: [disk('sda'), disk('sdb'), disk('sdc', { state: 'degraded', readErrors: 3 })] },
      { id: 'cache-0', role: 'cache', kind: 'stripe', state: 'online', faultTolerance: 0, disks: [disk('nvme0n1', { kind: 'nvme', sizeBytes: 0.5 * TB })] },
    ],
  },
  properties: [
    { name: 'compression', value: 'zstd', source: 'local', editable: true },
    { name: 'atime', value: 'off', source: 'default', editable: true },
  ],
  datasets: [
    { name: 'tank', kind: 'filesystem', usedBytes: 2 * TB, availableBytes: 6 * TB, referencedBytes: TB, compression: 'zstd', compressRatio: 1.3, encrypted: false, mounted: true, mountpoint: '/tank', snapshotCount: 2, quotaBytes: 0, scheduled: false },
    { name: 'tank/home', kind: 'filesystem', usedBytes: TB, availableBytes: 6 * TB, referencedBytes: TB, compression: 'inherit', compressRatio: 1.1, encrypted: false, mounted: true, mountpoint: '/tank/home', snapshotCount: 4, quotaBytes: 0, scheduled: true },
  ],
  alerts: [],
  history: [],
};

function makeScreen(extra = {}) {
  const screen = fakeScreen({
    tentaNasPoolGetRequest: poolGet,
    tentaNasDisksListRequest: {
      disks: [
        disk('sdd', { role: 'free' }),
        disk('sda', { role: 'member', temperatureC: 42 }),
        disk('nvme0n1', { role: 'member', kind: 'nvme', temperatureC: 38 }),
      ],
    },
    tentaNasDatasetsListRequest: { datasets: poolGet.datasets },
    tentaNasSnapshotsListRequest: { snapshots: [], total: 0, totalUsedBytes: 0 },
    tentaNasSnapshotSchedulesListRequest: { schedules: [] },
    tentaNasSharesListRequest: { shares: [{ shareId: 'sh-1', name: 'home', protocol: 'smb', sourcePath: '/tank/home', dataset: 'tank/home', enabled: true, mounts: [], sessions: 0, state: 'active' }] },
    ...extra,
  });
  screen.pool = 'tank';
  screen.poolTab = 'topology';
  screen.dataset = null;
  screen.locations = 0;
  screen.setLocation = () => { screen.locations += 1; };
  screen.renderAlertList = () => {};
  return screen;
}

function mount() {
  const body = document.createElement('div');
  document.body.appendChild(body);
  return body;
}

test('renders the KPI row and one topology group per vdev with the free-disk actions', async () => {
  const screen = makeScreen();
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  assert.deepEqual(screen.calls.map((c) => c.kind).sort(), ['tentaNasDisksListRequest', 'tentaNasPoolGetRequest']);
  assert.deepEqual(screen.calls.find((c) => c.kind === 'tentaNasPoolGetRequest').payload, { name: 'tank' });

  assert.equal(body.querySelector('#nas-pool-head'), null, 'n06 carries no separate pool-identity card');
  assert.equal(body.querySelector('#nas-pool-kpi').children.length, 4, 'four KPI tiles');
  const crumbs = [...body.querySelectorAll('.nas-crumbs .tf-breadcrumb-item')].map((c) => c.textContent);
  assert.deepEqual(crumbs, ['Pule', 'tank']);
  assert.ok(body.querySelector('#nas-pool-tab-body [data-act="scrub-start"]'), 'idle pool offers a scrub in the topology panel');
  const groups = [...body.querySelectorAll('.vdev-group[data-vdev]')];
  assert.deepEqual(groups.map((g) => g.dataset.vdev), ['raidz1-0', 'cache-0']);
  assert.equal(groups[0].querySelectorAll('.disk-cell').length, 3);
  assert.equal(groups[1].querySelectorAll('.disk-cell').length, 1);
  const addButtons = [...body.querySelectorAll('[data-act="add-vdev"]')].map((b) => b.dataset.role);
  assert.deepEqual(addButtons, ['data', 'cache', 'spare'], 'the three shortcuts of the mockup');
  // A composite (multi-disk) non-data vdev is not a bare leaf (MINOR 10 only
  // drops the role from a bare leaf's header): `vg-type` still names its role.
  assert.equal(groups[1].querySelector('.vg-type').textContent, 'Cache (L2ARC) · Stripe');
  screen.dispose();
});

test('the topology tab carries properties and the danger zone, and no invented alerts card', async () => {
  const screen = makeScreen();
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  // n06 order: topology → scrub → IO → properties → danger zone.
  const titles = [...body.querySelectorAll('#nas-pool-tab-body .section-card-head .title')].map((t) => t.textContent.trim());
  assert.deepEqual(titles, ['Topologia puli', 'Scrub i spójność', 'Statystyki IO puli', 'Właściwości puli']);
  assert.equal(body.querySelector('#nas-pool-alerts'), null, 'no Alerty card on n06');

  const props = body.querySelector('#nas-pool-props');
  assert.deepEqual(props.rows.map((r) => r._prop.name), ['compression', 'atime']);
  const danger = body.querySelector('#nas-pool-tab-body .danger-zone');
  assert.ok(danger, 'danger zone sits under the properties');
  assert.ok(danger.querySelector('[data-act="export"]'));
  click(danger.querySelector('[data-act="destroy"]'));
  await flush();
  const dlg = document.querySelector('tf-window');
  assert.ok(dlg, 'destroy dialog opened');
  assert.ok(dlg.querySelector('#nas-retype'), 'with the retype gate');
  assert.ok(dlg.querySelector('[data-action="confirm"]').hasAttribute('disabled'));
  dlg.remove();
  screen.dispose();
});

test('a topology cell follows n06:223 — media badge, temperature, state chip only when degraded', async () => {
  const screen = makeScreen();
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const cells = [...body.querySelectorAll('.vdev-group[data-vdev="raidz1-0"] .disk-cell')];
  assert.equal(cells[0].querySelector('.dc-name tf-chip'), null, 'an online disk carries no ONLINE chip');
  assert.equal(cells[0].querySelector('.disk-kind').textContent, 'HDD', 'the media badge comes from the inventory, mapped like n11 (MINOR 6)');
  assert.equal(cells[0].querySelector('.dc-sub').textContent, '4.0 TiB · 42°C', 'size · temperature, as the mockup');
  assert.equal(cells[1].querySelector('.disk-kind'), null, 'a leaf the inventory does not know gets no badge');
  assert.equal(cells[2].querySelector('.dc-name tf-chip').getAttribute('label'), 'Zdegradowana', 'a problem disk keeps its state chip');
  assert.equal(body.querySelector('.vdev-group[data-vdev="cache-0"] .disk-kind').textContent, 'NVMe');
  screen.dispose();
});

// ---------------------------------------------------------------------------
// MINOR 12: `KIND_BADGE` used to be copied verbatim into pool-detail.js and
// elastic-detail.js (n11). It now lives once in format.js. Proving that
// without touching elastic-detail.js (owned by another agent right now):
// mutate the SAME object format.js exports and check pool-detail.js's
// painted badge follows. If pool-detail.js still carried its own copy, this
// mutation would have no effect on what n06 paints.
// ---------------------------------------------------------------------------

test('n06 paints the media badge straight from format.js\'s shared KIND_BADGE (MINOR 12)', async () => {
  const original = KIND_BADGE.hdd;
  KIND_BADGE.hdd = ['test-cls', 'TESTHDD'];
  try {
    const screen = makeScreen();
    const body = mount();
    await drawPoolDetail(screen, body);
    await flush();
    const badge = body.querySelector('.vdev-group[data-vdev="raidz1-0"] .disk-cell .disk-kind');
    assert.equal(badge.textContent, 'TESTHDD', 'n06 reads the same KIND_BADGE object format.js exports, not a private copy');
    assert.ok(badge.classList.contains('test-cls'));
    screen.dispose();
  } finally {
    KIND_BADGE.hdd = original;
  }
});

test('only a data vdev advertises a fault tolerance; the others get their own hint (n06:233-248)', async () => {
  const screen = makeScreen();
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const hint = (id) => body.querySelector(`.vdev-group[data-vdev="${id}"] .vg-head .hint`).textContent;
  assert.equal(hint('raidz1-0'), 'odporność: przetrwa awarię 1 dysku jednocześnie');
  assert.match(hint('cache-0'), /^cache odczytu \(L2ARC\)/);
  assert.ok(!/odporność/.test(hint('cache-0')), 'a cache vdev never claims a tolerance');
  screen.dispose();
});

test('the IO card carries the live stream chart of n06:283 and keeps its samples across a poll', async () => {
  const screen = makeScreen();
  // Driving the poll by hand: the chart mounted with the pane must be FED the
  // new sample, never re-mounted from scratch.
  const polls = [];
  screen.later = (fn) => { polls.push(fn); return 0; };
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const card = [...body.querySelectorAll('#nas-pool-tab-body .section-card')].find((c) => /Statystyki IO puli/.test(c.textContent));
  assert.ok(card.querySelector('#nas-pool-io-live'), 'the IO card owns a stream chart');
  assert.match(card.querySelector('.live-label').textContent, /na żywo/);
  const points = () => body.querySelector('#nas-pool-io-live polyline[data-series-id="read"]').getAttribute('points').trim().split(' ').length;
  assert.equal(points(), 1, 'seeded with the sample taken at paint time');

  await polls.pop()();
  await flush();
  assert.equal(points(), 2, 'the poll pushed its sample into the chart on screen');
  screen.dispose();
});

test('a non-admin gets no scrub or add-vdev actions', async () => {
  const screen = makeScreen();
  screen.isAdmin = false;
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  assert.equal(body.querySelector('[data-act="scrub-start"]'), null);
  assert.equal(body.querySelectorAll('[data-act="add-vdev"]').length, 0);
  screen.dispose();
});

test('switching the inner tabs loads datasets and snapshots', async () => {
  const screen = makeScreen();
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const tabs = body.querySelector('#nas-pool-tabs');
  const sent = () => screen.calls.map((c) => c.kind);

  tabs.dispatchEvent(new window.CustomEvent('change', { detail: { value: 'datasets' } }));
  await flush();
  await flush();
  assert.equal(screen.poolTab, 'datasets');
  assert.equal(screen.locations, 1, 'the URL follows the inner tab');
  const dsList = screen.calls.find((c) => c.kind === 'tentaNasDatasetsListRequest');
  assert.ok(dsList, 'datasets requested');
  assert.deepEqual(dsList.payload, { pool: 'tank' });
  assert.ok(sent().includes('tentaNasSharesListRequest'), 'shares requested for the share chips');
  assert.equal(body.querySelector('#nas-ds-table').rows.length, 2);

  tabs.dispatchEvent(new window.CustomEvent('change', { detail: { value: 'snapshots' } }));
  await flush();
  await flush();
  const snapList = screen.calls.find((c) => c.kind === 'tentaNasSnapshotsListRequest');
  assert.ok(snapList, 'snapshots requested');
  assert.equal(snapList.payload.pool, 'tank');
  assert.equal(snapList.payload.recursive, true);
  assert.ok(sent().includes('tentaNasSnapshotSchedulesListRequest'), 'snapshot schedules requested');
  assert.ok(body.querySelector('#nas-snap-filters'), 'the snapshot list carries the filter chips');
  assert.ok(body.querySelector('#nas-snap-schedule'), 'the schedule card renders');

  assert.deepEqual([...body.querySelectorAll('#nas-pool-tabs tf-tab')].map((t) => t.id), ['topology', 'datasets', 'snapshots', 'stats', 'properties']);
  screen.dispose();
});

test('the Właściwości tab renders the same properties table and danger zone as the topology foot', async () => {
  const screen = makeScreen();
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  assert.deepEqual([...body.querySelectorAll('#nas-pool-tabs tf-tab')].map((t) => [t.textContent.replace(/^\d+/, '').trim(), t.getAttribute('count')]),
    [['Topologia', null], ['Datasety', '2'], ['Snapshoty', '6'], ['Statystyki', null], ['Właściwości', null]],
    'the five-tab strip of n06 = n09 = n10, with the pool counts');

  body.querySelector('#nas-pool-tabs').dispatchEvent(new window.CustomEvent('change', { detail: { value: 'properties' } }));
  await flush();
  assert.equal(screen.poolTab, 'properties');
  assert.equal(screen.locations, 1, 'the deep link follows the inner tab');
  const titles = [...body.querySelectorAll('#nas-pool-tab-body .section-card-head .title')].map((t) => t.textContent.trim());
  assert.deepEqual(titles, ['Właściwości puli'], 'only the properties card, no topology above it');
  assert.deepEqual(body.querySelector('#nas-pool-props').rows.map((r) => r._prop.name), ['compression', 'atime']);
  const danger = body.querySelector('#nas-pool-tab-body .danger-zone');
  assert.ok(danger, 'the danger zone follows the properties here too');
  assert.ok(danger.querySelector('[data-act="export"]'));
  assert.ok(danger.querySelector('[data-act="destroy"]'));
  screen.dispose();
});

test('a failed load shows the error with the breadcrumb back to the pools', async () => {
  const screen = makeScreen({ tentaNasPoolGetRequest: () => { throw new Error('no such pool'); } });
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  assert.match(body.querySelector('tf-alert').getAttribute('message'), /no such pool/);
  click(body.querySelector('.nas-crumbs a'));
  assert.equal(screen.pool, null);
  assert.equal(screen.locations, 1);
  screen.dispose();
});

// "nigdy pełne odświeżenie całości" (research/03-ui-wzorce-mockupy.md). The
// topology pane was rebuilt by every 5 s poll, destroying every vdev cell,
// every button in it and the live chart. The three IO numbers are the only
// part a poll moves on an otherwise idle pool, so they are written as TEXT
// into slots the pane keeps instead of being baked into its markup.
test('a poll that only moves the IO numbers leaves the topology pane standing', async () => {
  let readBps = 1_000_000;
  const screen = makeScreen({
    tentaNasPoolGetRequest: () => ({ ...poolGet, pool: { ...poolGet.pool, io: { readBps, writeBps: 0, readIops: 12, writeIops: 0 } } }),
  });
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const pane = body.querySelector('#nas-pool-tab-body');
  const cells = [...pane.querySelectorAll('.disk-cell')];
  const chart = pane.querySelector('#nas-pool-io-live');
  const throughput = pane.querySelector('[data-io="throughput"]');
  const tiles = [...body.querySelectorAll('#nas-pool-kpi tf-stat-card')];
  assert.equal(cells.length, 4, 'one cell per pool disk');
  assert.equal(tiles.length, 4, 'four KPI tiles');
  assert.ok(chart, 'the live chart is mounted');
  const before = throughput.textContent;
  assert.match(before, /MB\/s/, 'the readout is written as text');

  readBps = 8_000_000;
  assert.equal(scheduled.length, 1, 'exactly one polling chain');
  await scheduled[0]();
  await flush();
  [...pane.querySelectorAll('.disk-cell')].forEach((el, i) => assert.equal(el === cells[i], true, `disk cell ${i} survives the poll`));
  [...body.querySelectorAll('#nas-pool-kpi tf-stat-card')].forEach((el, i) => assert.equal(el === tiles[i], true, `KPI tile ${i} survives the poll`));
  assert.equal(pane.querySelector('#nas-pool-io-live') === chart, true, 'the chart keeps the points it accumulated');
  assert.equal(pane.querySelector('[data-io="throughput"]') === throughput, true, 'the readout element survives');
  assert.equal(throughput.textContent !== before, true, 'and its number still moved');
  screen.dispose();
});

// A log or special vdev on a spinning disk buys nothing — a SLOG exists to cut
// write latency, a special vdev to serve metadata — and neither can be removed
// from a raidz pool afterwards. The dialog offered both roles against any free
// disk, which is the opposite of what `pool.hint_special` promises.
const pickDisk = (win, diskId, checked) => {
  const cell = win.querySelector(`.disk-cell[data-disk="${diskId}"]`);
  cell.querySelector('tf-checkbox').dispatchEvent(
    new window.CustomEvent('change', { bubbles: true, detail: { checked } }),
  );
};

test('a special or log vdev refuses a spinning disk and says which one', async () => {
  const free = [disk('sdd'), disk('nvme1n1', { kind: 'nvme', sizeBytes: 0.5 * TB })];
  const screen = fakeScreen({});
  const win = openAddVdevDialog(screen, poolGet.pool, 'special', free, () => {});
  await flush();

  const err = win.querySelector('#nas-av-error');
  const confirm = win.querySelector('[data-action="confirm"]');
  assert.ok(err.hidden, 'nothing is wrong before a disk is picked');

  pickDisk(win, 'sdd', true);
  await flush();
  assert.equal(err.hidden, false, 'picking an HDD for a special vdev is refused');
  assert.match(err.textContent, /sdd/, 'the refusal names the offending disk');
  assert.match(err.textContent, /SSD|NVMe/, 'and what the role actually needs');
  assert.ok(confirm.hasAttribute('disabled'), 'there is no "add anyway"');

  pickDisk(win, 'sdd', false);
  pickDisk(win, 'nvme1n1', true);
  await flush();
  assert.ok(err.hidden, 'flash clears the refusal');
  assert.ok(!confirm.hasAttribute('disabled'), 'and the vdev can be added');

  win.close(true);
});

test('a data vdev still accepts spinning disks', async () => {
  const free = [disk('sdd')];
  const win = openAddVdevDialog(fakeScreen({}), poolGet.pool, 'data', free, () => {});
  await flush();
  pickDisk(win, 'sdd', true);
  await flush();
  assert.ok(win.querySelector('#nas-av-error').hidden, 'the gate is only for log and special');
  assert.ok(!win.querySelector('[data-action="confirm"]').hasAttribute('disabled'));
  win.close(true);
});

// ---------------------------------------------------------------------------
// Patch-in-place (owner's rule: patch only what changed, never rebuild a
// subtree on a poll; a chart is never re-mounted by a poll)
// ---------------------------------------------------------------------------

// A screen whose PoolGet answer is rebuilt from `live` on every request, and
// whose poll chain is driven by hand.
function liveScreen(live, { disks } = {}) {
  const screen = makeScreen({
    tentaNasPoolGetRequest: () => ({ ...poolGet, pool: { ...poolGet.pool, ...live.pool() } }),
    ...(disks ? { tentaNasDisksListRequest: () => ({ disks: disks() }) } : {}),
  });
  const scheduled = [];
  screen.later = (fn) => { scheduled.push(fn); };
  screen.poll = async () => { await scheduled.shift()(); await flush(); };
  return screen;
}

// B1 (critic n01-n10): a running scrub moved `progressPct` / `etaSecs` on
// every 5 s poll, and the whole topology pane — every vdev cell, every
// button, the live chart, the properties table and the danger zone — was
// re-parsed from one joined string each time.
test('a running scrub moves its percentage in place: cells, buttons, chart and properties keep their nodes', async () => {
  let progressPct = 42;
  const screen = liveScreen({ pool: () => ({ scan: { kind: 'scrub', status: 'running', progressPct, etaSecs: 3600, scannedBytes: TB, errors: 0 } }) });
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const pane = body.querySelector('#nas-pool-tab-body');
  const SEL = {
    group: '.vdev-group[data-vdev="raidz1-0"]',
    cell: '.vdev-group[data-vdev="raidz1-0"] .disk-cell[data-device="sda"]',
    cellButton: '.disk-cell[data-device="sda"] [data-act="offline"]',
    pause: '[data-act="scrub-pause"]',
    stop: '[data-act="scrub-stop"]',
    addData: '[data-act="add-vdev"][data-role="data"]',
    chip: '[data-part="scrub-actions"] tf-chip',
    bar: '[data-slot="scan-bar"] tf-progress-bar',
    chart: '#nas-pool-io-live',
    props: '#nas-pool-props',
    exportBtn: '[data-act="export"]',
    force: '#nas-export-force',
  };
  const before = Object.fromEntries(Object.entries(SEL).map(([k, sel]) => [k, pane.querySelector(sel)]));
  for (const [k, el] of Object.entries(before)) assert.ok(el, `${k} is on screen`);
  assert.equal(before.chip.getAttribute('label'), 'Scrub 42%');
  assert.equal(before.bar.getAttribute('value'), '42');
  const rows = before.props.rows;

  progressPct = 57;
  await screen.poll();
  progressPct = 63;
  await screen.poll();
  for (const [k, el] of Object.entries(before)) assert.equal(pane.querySelector(SEL[k]) === el, true, `${k} is the same node after two polls`);
  assert.equal(before.chip.getAttribute('label'), 'Scrub 63%', 'the new percentage is written into the chip on screen');
  assert.equal(before.bar.getAttribute('value'), '63', 'and into the progress bar on screen');
  assert.equal(before.props.rows === rows, true, 'unchanged properties are not re-assigned to the table (M10)');
  screen.dispose();
});

test('a disk that comes or goes changes only its own cell; every other group and cell keeps its node', async () => {
  const vdevs = () => poolGet.pool.vdevs.map((v) => ({ ...v, disks: [...v.disks] }));
  let layout = vdevs();
  const screen = liveScreen({ pool: () => ({ vdevs: layout }) });
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const pane = body.querySelector('#nas-pool-tab-body');
  const cell = (name) => pane.querySelector(`.disk-cell[data-device="${name}"]`);
  const raidz = pane.querySelector('.vdev-group[data-vdev="raidz1-0"]');
  const cache = pane.querySelector('.vdev-group[data-vdev="cache-0"]');
  const [sda, sdb, sdc, nvme] = ['sda', 'sdb', 'sdc', 'nvme0n1'].map(cell);
  const cellsHost = raidz.querySelector('.disk-cells');

  // sdb leaves the raidz group, a spare vdev arrives.
  layout = vdevs();
  layout[0].disks = layout[0].disks.filter((d) => d.name !== 'sdb');
  layout.push({ id: 'spare-0', role: 'spare', kind: 'stripe', state: 'online', faultTolerance: 0, disks: [disk('sde')] });
  let added = 0;
  let removed = 0;
  const obs = new window.MutationObserver((recs) => recs.forEach((r) => { added += r.addedNodes.length; removed += r.removedNodes.length; }));
  obs.observe(pane.querySelector('[data-part="vdevs"]'), { childList: true });
  obs.observe(cellsHost, { childList: true });
  await screen.poll();
  await flush();
  obs.disconnect();

  assert.equal(cell('sdb'), null, 'the removed disk is gone');
  assert.ok(pane.querySelector('.vdev-group[data-vdev="spare-0"] .disk-cell[data-device="sde"]'), 'the new vdev and its disk are there');
  assert.equal(pane.querySelector('.vdev-group[data-vdev="raidz1-0"]') === raidz, true, 'the raidz group is the same node');
  assert.equal(pane.querySelector('.vdev-group[data-vdev="cache-0"]') === cache, true, 'the cache group is the same node');
  assert.equal(cell('sda') === sda && cell('sdc') === sdc && cell('nvme0n1') === nvme, true, 'every surviving cell is the same node');
  assert.equal(sdb.isConnected, false);
  assert.equal(added, 1, 'exactly one node was added (the new group)');
  assert.equal(removed, 1, 'exactly one node was removed (the gone cell)');
  screen.dispose();
});

// M3 (critic n01-n10): the dot was the zpool LEAF state, so a reallocating
// disk in an ONLINE vdev showed green and the "disk warnings" KPI said 0.
test('a disk SMART warns about shows the warning tone inside an ONLINE leaf, and the KPI counts it (M3)', async () => {
  let health = 'ok';
  let leafState = 'online';
  const screen = liveScreen(
    { pool: () => ({ vdevs: [{ id: 'mirror-0', role: 'data', kind: 'mirror', state: 'online', faultTolerance: 1, disks: [disk('sda', { state: leafState }), disk('sdb')] }] }) },
    { disks: () => [disk('sda', { role: 'member', health, healthReason: health === 'ok' ? '' : '8 reallocated sectors; 54°C', temperatureC: 47 }), disk('sdb', { role: 'member' })] },
  );
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const cell = () => body.querySelector('.disk-cell[data-device="sda"]');
  const dot = () => cell().querySelector('.health-dot');
  const stateTile = () => body.querySelector('#nas-pool-kpi tf-stat-card[data-kpi="state"]');
  assert.ok(dot().classList.contains('ok'), 'a healthy disk is green');
  assert.match(stateTile().getAttribute('delta'), /0 ostrzeżeń dysku/);

  health = 'warning';
  await screen.poll();
  assert.ok(dot().classList.contains('warn'), 'the dot follows SMART health, not the ONLINE leaf');
  assert.ok(!dot().classList.contains('ok'));
  const chip = cell().querySelector('.dc-name tf-chip');
  assert.equal(chip.getAttribute('status'), 'warn', 'a status tf-chip accepts (its allowlist has no "warning")');
  assert.ok(chip.querySelector('.tf-chip.warn'), 'and the component really renders it as warn, not the neutral fallback');
  // M5: the chip shows the translated health grade, never the server's raw
  // English SMART text — that goes in `title=` only, for support.
  assert.equal(chip.getAttribute('label'), 'Uwaga', 'the chip names the health grade, not raw SMART text');
  assert.equal(chip.getAttribute('title'), '8 reallocated sectors; 54°C', 'the server reason lives in the title only');
  assert.ok(cell().classList.contains('warn'));
  assert.match(stateTile().getAttribute('delta'), /1 ostrzeżenie dysku/, 'the KPI counts the SMART warning (n06:188)');

  // A FAULTED leaf stays red even when SMART says the disk is fine.
  health = 'ok';
  leafState = 'faulted';
  await screen.poll();
  assert.ok(dot().classList.contains('err'), 'a faulted leaf is red whatever SMART says');
  assert.equal(cell().querySelector('.dc-name tf-chip').getAttribute('status'), 'err');
  screen.dispose();
});

// MAJOR 20 / M16 (critic n11-n19): the replace wizard did `win.innerHTML =`
// on every 1.5 s tick (the log lost its scroll position), and it stayed on
// step 2 for the whole resilver because it looked at the scan only after the
// job — which spans the resilver — had finished.
test('the replace wizard patches its tick in place and reaches step 3 while the job is still resilvering', async () => {
  const realSetTimeout = globalThis.setTimeout;
  const ticks = [];
  globalThis.setTimeout = (fn, ms, ...rest) => (ms === 1500 ? (ticks.push(fn), 0) : realSetTimeout(fn, ms, ...rest));
  try {
    let job = { jobId: 'j1', status: 'running', progressPct: 5, log: ['zpool replace tank sdc sdd'] };
    let scan = { kind: 'none', status: 'idle' };
    const screen = fakeScreen({
      tentaNasPoolReplaceDiskRequest: { job },
      tentaNasJobGetRequest: () => ({ job }),
      tentaNasPoolGetRequest: () => ({ ...poolGet, pool: { ...poolGet.pool, scan } }),
    });
    const pool = poolGet.pool;
    const win = openReplaceWizard(screen, { pool, vdev: pool.vdevs[0], disk: pool.vdevs[0].disks[2], freeDisks: [disk('sdd', { role: 'free' })], disks: [] });
    await flush();
    click(win.querySelector('.target-option[data-disk="sdd"]'));
    click(win.querySelector('[data-wizard-next]'));
    await flush();
    await flush();
    assert.equal(ticks.length, 1, 'the job is being followed');

    const rail = [...win.querySelectorAll('.install-step')];
    const cancel = win.querySelector('[data-wizard-cancel]');
    const bar = win.querySelector('.install-step-body tf-progress-bar');
    const log = win.querySelector('.install-step-body .job-log');
    const firstText = log.firstChild;
    assert.ok(rail[1].classList.contains('active'), 'step 2 while zpool replace runs');
    assert.equal(bar.getAttribute('value'), '5');

    job = { ...job, progressPct: 30, log: [...job.log, 'resilver started'] };
    await ticks.shift()();
    await flush();
    assert.equal(win.querySelector('.install-step-body tf-progress-bar') === bar, true, 'the bar is the same node');
    assert.equal(bar.getAttribute('value'), '30', 'and shows the new progress');
    assert.equal(win.querySelector('.install-step-body .job-log') === log, true, 'the log is the same node');
    assert.equal(log.firstChild === firstText, true, 'the text already on screen is untouched — the tail is appended');
    assert.match(log.textContent, /resilver started$/);
    assert.equal(win.querySelector('[data-wizard-cancel]') === cancel, true, 'the footer buttons survive the tick');
    assert.equal([...win.querySelectorAll('.install-step')].every((el, i) => el === rail[i]), true, 'the step rail survives the tick');

    // The job is still running, but the pool now reports the resilver it started.
    scan = { kind: 'resilver', status: 'running', progressPct: 40, etaSecs: 1800, errors: 0 };
    await ticks.shift()();
    await flush();
    assert.ok(rail[2].classList.contains('active'), 'step 3 is reached during the resilver, not after it');
    assert.ok(rail[1].classList.contains('done'));
    assert.match(win.querySelector('.install-step-body tf-progress-bar').getAttribute('label'), /resilver 40%/);
    assert.equal(ticks.length, 1, 'still following the job');
    win.remove();
    screen.dispose();
  } finally {
    globalThis.setTimeout = realSetTimeout;
  }
});

// ---------------------------------------------------------------------------
// Machine-identifier hard rule (critic M4): a bare-leaf vdev's `id` is the
// leaf's own path (often a by-id serial/WWN path), never a name zpool made
// up — the old `cache-0` fixture hid this because it happened to read like a
// harmless label.
// ---------------------------------------------------------------------------

test('a bare-leaf vdev header names its role, never the by-id path zpool prints as its id (M4)', async () => {
  const screen = makeScreen({
    tentaNasPoolGetRequest: {
      ...poolGet,
      pool: {
        ...poolGet.pool,
        vdevs: [
          { id: 'raidz1-0', role: 'data', kind: 'raidz1', state: 'online', faultTolerance: 1, disks: [disk('sda'), disk('sdb'), disk('sdc')] },
          { id: 'wwn-0x5000c500a1b2c3d4', role: 'spare', kind: 'disk', state: 'online', faultTolerance: 0, disks: [disk('sdk')] },
          { id: 'nvme-eui.0000000000000001', role: 'cache', kind: 'disk', state: 'online', faultTolerance: 0, disks: [disk('nvme2n1', { kind: 'nvme' })] },
          { id: 'nvme-eui.0000000000000002', role: 'cache', kind: 'disk', state: 'online', faultTolerance: 0, disks: [disk('nvme3n1', { kind: 'nvme' })] },
        ],
      },
    },
  });
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const label = (id) => body.querySelector(`.vdev-group[data-vdev="${id}"] .vg-head .mono`);
  const vgType = (id) => body.querySelector(`.vdev-group[data-vdev="${id}"] .vg-head .vg-type`).textContent;
  assert.equal(label('raidz1-0').textContent, 'raidz1-0', 'a zpool-generated group id is not a machine identifier and stays visible');
  const spareLabel = label('wwn-0x5000c500a1b2c3d4');
  assert.equal(spareLabel.textContent, 'Zapasowy', 'the by-id path never appears as visible text');
  assert.equal(spareLabel.getAttribute('title'), 'wwn-0x5000c500a1b2c3d4', 'the real id lives in title= only');
  assert.equal(label('nvme-eui.0000000000000001').textContent, 'Cache (L2ARC) 1', 'an ordinal tells apart two bare vdevs of the same role');
  assert.equal(label('nvme-eui.0000000000000002').textContent, 'Cache (L2ARC) 2');
  // MINOR 10: the role already names the bare leaf in the mono span above —
  // `vg-type` must not repeat it, only the layout ("Pojedynczy dysk").
  assert.equal(vgType('wwn-0x5000c500a1b2c3d4'), 'Pojedynczy dysk', 'vg-type shows only the layout for a bare leaf, not "Zapasowy" again');
  assert.ok(!vgType('wwn-0x5000c500a1b2c3d4').includes('Zapasowy'), 'the role is not repeated in the header');
  assert.equal(vgType('nvme-eui.0000000000000001'), 'Pojedynczy dysk', 'same for a bare cache leaf');
  assert.ok(!vgType('nvme-eui.0000000000000001').includes('Cache'), 'the role is not repeated in the header');
  // A composite (non-bare) vdev still carries its role in vg-type — only a
  // bare leaf's own role, already shown in the mono label, is dropped here.
  assert.equal(vgType('raidz1-0'), 'RAIDZ1', 'a data vdev never had the role prefix (unaffected by MINOR 10)');
  screen.dispose();
});

// ---------------------------------------------------------------------------
// A missing leaf (M4): `zpool status` can no longer resolve the device and
// prints its numeric GUID instead. `leaf()` (tentaflow-core/src/tentanas/
// pools.rs) keeps that GUID verbatim in `name` — it must never reach the
// screen as visible text, in the cell or in the replace wizard.
//
// `zpool`'s "was /dev/…" annotation on a removed leaf is not parenthesized,
// so `parse_config_row` (pools.rs:176-180, note captured only from `(...)`)
// never puts it in `note` — a fixture that expects a name recovered from
// `note` exercises a shape the parser cannot produce. There is nothing in
// the wire payload a GUID-only leaf can be named from, so it always falls
// back to the translated "missing disk" label.
// ---------------------------------------------------------------------------

test('a leaf zpool can no longer find shows "brak dysku" in its cell, never the raw GUID (M4)', async () => {
  const missing = disk('17705618834980784999', { name: '17705618834980784999', state: 'unavail', note: '' });
  const screen = makeScreen({
    tentaNasPoolGetRequest: {
      ...poolGet,
      pool: { ...poolGet.pool, vdevs: [{ id: 'raidz1-0', role: 'data', kind: 'raidz1', state: 'degraded', faultTolerance: 1, disks: [disk('sda'), missing] }] },
    },
  });
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const nameSpan = (guid) => body.querySelector(`.disk-cell[data-device="${guid}"] .dc-name .mono`);
  assert.equal(nameSpan('17705618834980784999').textContent, 'brak dysku', 'a bare GUID names no real device: the translated missing-disk label');
  assert.equal(nameSpan('17705618834980784999').getAttribute('title'), '17705618834980784999', 'the GUID lives in title= only');
  // The wire identity (device actions, replace) is unchanged: still the GUID.
  assert.equal(body.querySelector('.disk-cell[data-device="17705618834980784999"]') !== null, true);
  screen.dispose();
});

// ---------------------------------------------------------------------------
// MAJOR 3: a REMOVED or FAULTED leaf whose by-id symlink is gone. `zpool`
// still prints it BY PATH, but `kernel_name_of` cannot canonicalise it
// (zfs.rs:209-219), and `strip_partition_suffix` only folds the kernel's
// own naming schemes, never a by-id name (zfs.rs:226-229) — the core keeps
// the by-id basename verbatim, e.g. `wwn-0x…-part1` or `ata-…-part1`. That
// is a machine identifier exactly like the GUID shape, so it must never
// reach the cell as text either.
// ---------------------------------------------------------------------------

test('a by-id leaf name never shows in its cell: the inventory\'s kernel name when it resolves, else "brak dysku" (MAJOR 3)', async () => {
  const resolved = disk('wwn-0x5000c500a1b2c3d4-part1', { name: 'wwn-0x5000c500a1b2c3d4-part1', diskId: 'sn-wd-9001', state: 'unavail', note: '' });
  const unresolved = disk('ata-WDC_WD40EFRX-68N32N0_WD-WCC7K0000000-part1', { name: 'ata-WDC_WD40EFRX-68N32N0_WD-WCC7K0000000-part1', diskId: undefined, state: 'unavail', note: '' });
  const screen = makeScreen({
    tentaNasDisksListRequest: { disks: [disk('sdk', { role: 'member', diskId: 'sn-wd-9001' })] },
    tentaNasPoolGetRequest: {
      ...poolGet,
      pool: { ...poolGet.pool, vdevs: [{ id: 'raidz1-0', role: 'data', kind: 'raidz1', state: 'degraded', faultTolerance: 1, disks: [disk('sda'), resolved, unresolved] }] },
    },
  });
  const body = mount();
  await drawPoolDetail(screen, body);
  await flush();
  const nameSpan = (id) => body.querySelector(`.disk-cell[data-device="${id}"] .dc-name .mono`);
  assert.equal(nameSpan('wwn-0x5000c500a1b2c3d4-part1').textContent, 'sdk', 'a by-id leaf the disk inventory still knows shows its real kernel name');
  assert.equal(nameSpan('wwn-0x5000c500a1b2c3d4-part1').getAttribute('title'), 'wwn-0x5000c500a1b2c3d4-part1', 'the by-id path lives in title= only, never as text');
  assert.equal(nameSpan('ata-WDC_WD40EFRX-68N32N0_WD-WCC7K0000000-part1').textContent, 'brak dysku', 'no inventory match: the translated missing-disk label, never the ata- basename');
  assert.equal(nameSpan('ata-WDC_WD40EFRX-68N32N0_WD-WCC7K0000000-part1').getAttribute('title'), 'ata-WDC_WD40EFRX-68N32N0_WD-WCC7K0000000-part1');
  // The wire identity (device actions, replace) is unchanged: still the by-id text.
  assert.equal(body.querySelector('.disk-cell[data-device="wwn-0x5000c500a1b2c3d4-part1"]') !== null, true);
  screen.dispose();
});

test('the replace wizard never shows a by-id leaf\'s raw path: window title and run heading show its kernel name instead (MAJOR 3)', async () => {
  const missing = disk('wwn-0x5000c500a1b2c3d4-part1', { name: 'wwn-0x5000c500a1b2c3d4-part1', diskId: 'sn-wd-9001', state: 'unavail', note: '' });
  const inventory = [disk('sdk', { role: 'member', diskId: 'sn-wd-9001' }), disk('sdd', { role: 'free' })];
  const screen = fakeScreen({
    tentaNasPoolReplaceDiskRequest: { job: { jobId: 'j1', status: 'running', progressPct: 0, log: [] } },
    tentaNasJobGetRequest: () => ({ job: { jobId: 'j1', status: 'running', progressPct: 5, log: [] } }),
    tentaNasPoolGetRequest: () => ({ pool: { scan: { kind: 'none', status: 'idle' } } }),
  });
  const pool = { ...poolGet.pool, vdevs: [{ id: 'raidz1-0', role: 'data', kind: 'raidz1', state: 'degraded', faultTolerance: 1, disks: [disk('sda'), disk('sdb'), missing] }] };
  const win = openReplaceWizard(screen, { pool, vdev: pool.vdevs[0], disk: missing, freeDisks: [disk('sdd', { role: 'free' })], disks: inventory });
  await flush();
  // tf-window renders its `title` attribute into `.tf-window-title-text` and
  // removes the attribute itself (native tooltip would duplicate it).
  const titleText = win.shadowRoot.querySelector('.tf-window-title-text').textContent;
  assert.match(titleText, /sdk/, 'the window title names the kernel device, not the by-id path');
  assert.ok(!titleText.includes('wwn-0x5000c500a1b2c3d4'));
  click(win.querySelector('.target-option[data-disk="sdd"]'));
  click(win.querySelector('[data-wizard-next]'));
  await flush();
  await flush();
  const heading = win.querySelector('.install-step-body .wizard-section-title').textContent;
  assert.match(heading, /sdk/, 'the run-step heading names the kernel device');
  assert.ok(!heading.includes('wwn-0x5000c500a1b2c3d4'), 'never the by-id path');
  win.remove();
  screen.dispose();
});

// ---------------------------------------------------------------------------
// A paint step must not silently stop n06 from polling (MINOR 7).
// ---------------------------------------------------------------------------

test('a paint step throwing is logged and does not stop n06 from polling', async () => {
  const errors = [];
  const realError = console.error;
  console.error = (...args) => errors.push(args);
  try {
    const scheduled = [];
    const screen = makeScreen();
    screen.later = (fn) => { scheduled.push(fn); };
    const body = mount();
    await drawPoolDetail(screen, body);
    await flush();
    assert.equal(scheduled.length, 1, 'the first poll is scheduled');

    // Break a paint step from outside: `paintKpis` reads the tab strip for
    // its dataset/snapshot counts.
    body.querySelector('#nas-pool-tabs').remove();
    await scheduled.shift()();
    await flush();
    assert.equal(errors.length, 1, 'the paint error is logged, not swallowed');
    assert.equal(scheduled.length, 1, 'the loop rescheduled itself instead of dying');

    await scheduled.shift()();
    await flush();
    assert.equal(errors.length, 2, 'and keeps logging (and polling) on the next tick too');
    assert.equal(scheduled.length, 1);
    screen.dispose();
  } finally {
    console.error = realError;
  }
});

// ---------------------------------------------------------------------------
// The replace wizard must refresh the pool page after a failed job the same
// way it does after a successful one (MINOR 7).
// ---------------------------------------------------------------------------

test('a failed replace job still refreshes the pool page, the same as a successful one', async () => {
  const screen = fakeScreen({
    tentaNasPoolReplaceDiskRequest: { job: { jobId: 'j1', status: 'running', progressPct: 0, log: [] } },
    tentaNasJobGetRequest: () => ({ job: { jobId: 'j1', status: 'failed', error: 'zpool replace failed: no such device' } }),
  });
  const pool = poolGet.pool;
  let done = 0;
  const win = openReplaceWizard(screen, { pool, vdev: pool.vdevs[0], disk: pool.vdevs[0].disks[2], freeDisks: [disk('sdd', { role: 'free' })], disks: [], onDone: () => { done += 1; } });
  await flush();
  click(win.querySelector('.target-option[data-disk="sdd"]'));
  click(win.querySelector('[data-wizard-next]'));
  await flush();
  await flush();
  assert.ok(win.querySelector('.result-box.err'), 'the wizard shows the failure');
  assert.equal(done, 1, 'onDone still runs, so the pool page drops the stale pre-replace topology');
  win.remove();
  screen.dispose();
});

test('a device-mapper kernel name is a disk, a device-mapper by-id link is an id', () => {
  // `dm-0` is what the kernel calls a LUKS/LVM device: hiding it behind
  // "brak dysku" would call a working disk missing.
  for (const kernel of ['dm-0', 'dm-12', 'sdd', 'nvme0n1', 'md0', 'mmcblk0', 'vda', 'xvda', 'loop0']) {
    assert.equal(isUnresolvedLeafName(kernel), false, `${kernel} is a kernel name`);
  }
  for (const id of [
    'dm-name-luks-root', 'dm-uuid-CRYPT-LUKS2-abc', 'wwn-0x5000c500a1b2c3d4-part1', 'ata-WDC_WD80-part1', '12837465019283746501',
    // MAJOR 3 residual (MINOR C): a bare partition-UUID leaf name — how
    // TrueNAS builds its pools — plus the by-id/by-path prefixes the
    // original `BY_ID_NAME_RE` missed.
    '4fa78b34-9e2a-4c1d-8f3a-1b2c3d4e5f6a',
    'virtio-serial-0',
    'mmc-SD16G_0xabcdef01',
    'md-name-orion:0',
    'md-uuid-abcdef01:23456789:abcdef01:23456789',
    'lvm-pv-uuid-AbCd1234EfGh5678IjKl9012MnOp',
    'pci-0000:00:1f.2-ata-1',
  ]) {
    assert.equal(isUnresolvedLeafName(id), true, `${id} is an id`);
  }
});
