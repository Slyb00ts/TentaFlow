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

const { drawPoolDetail } = await import('./pool-detail.js');

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
  assert.equal(cells[0].querySelector('.disk-kind').textContent, 'hdd', 'the media badge comes from the inventory');
  assert.equal(cells[0].querySelector('.dc-sub').textContent, '4.0 TiB · 42°C', 'size · temperature, as the mockup');
  assert.equal(cells[1].querySelector('.disk-kind'), null, 'a leaf the inventory does not know gets no badge');
  assert.equal(cells[2].querySelector('.dc-name tf-chip').getAttribute('label'), 'Zdegradowana', 'a problem disk keeps its state chip');
  assert.equal(body.querySelector('.vdev-group[data-vdev="cache-0"] .disk-kind').textContent, 'nvme');
  screen.dispose();
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
  // The pane is rebuilt on every poll, so the chart may not own the samples;
  // driving the poll by hand is what proves the ring survives the repaint.
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
  assert.equal(points(), 2, 'the repainted chart is re-seeded from the kept samples');
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
