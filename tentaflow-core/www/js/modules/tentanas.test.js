// =============================================================================
// File: modules/tentanas.test.js
// Description: The TentaNas shell against a stubbed transport: the fleet view
// (canonical header, tab strip with no active tab, client-side alert and
// resource aggregation including an unreachable node), the node view header,
// the overview (KPI tiles, ARC card, live charts), the disks tab (filters,
// bulk SMART selection), the disk detail (pool error block, replace wizard)
// and the environment tab (elevation rows, helper catalog).
// Runs under happy-dom through the shared TentaNas test bootstrap.
// =============================================================================

import { window, flush, click, windowTitle } from './tentanas/_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { default: Screen } = await import('./tentanas.js');

const LOCAL = 'nodeaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const REMOTE = 'nodebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
const MAC = 'nodeccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc';

function node(overrides) {
  return {
    nodeId: LOCAL, nodeName: 'orion', isLocal: true, online: true, instanceStatus: 'ready', health: 'ok',
    osName: 'Debian 12', zfsVersion: '2.2.4', elevationMode: 'helper', disksTotal: 2, disksWarning: 0,
    poolsTotal: 1, sharesTotal: 1, alertsActive: 0, capacityBytes: 4e12, usedBytes: 1e12, updatedAt: '2026-09-02 10:00:00',
    features: ['OpenZFS 2.2.4', 'SMB'],
    ...overrides,
  };
}

function disk(overrides) {
  return {
    diskId: 'sda', name: 'sda', path: '/dev/sda', kind: 'hdd', model: 'WD Red', serial: 'WD-1', wwn: null, sizeBytes: 2e12,
    transport: 'sata', rotational: true, removable: false, firmware: null, role: 'free', memberOf: null, health: 'ok', healthReason: '',
    temperatureC: 34, powerOnHours: 100, reallocatedSectors: 0, pendingSectors: 0, crcErrors: 0, mediaErrors: null, wearPct: null,
    smartAvailable: true, smartPassed: true, smartReadAt: '2026-09-02 09:59:00',
    io: { readBps: 1048576, writeBps: 0, readIops: 10, writeIops: 0, awaitMs: 2.5, utilPct: 3 }, ioHistoryBps: [0, 1, 2], mountpoints: [],
    vdevRole: '', vdevKind: '', fsType: null, arrayRole: '',
    ...overrides,
  };
}

const environment = {
  platform: 'linux', fullSupport: true, osName: 'Debian', osVersion: '12', kernel: '6.1', hostname: 'orion', packageManager: 'apt',
  ramBytes: 8e9, uptimeSecs: 3600, probedAt: '2026-09-02 09:00:00',
  features: [{ id: 'zfs', status: 'ok', version: '2.2.4', requiredVersion: null, binaries: ['zpool', 'zfs'], kernelModule: 'zfs', packages: ['zfs'], detail: 'JSON ✓', optional: false }],
  elevation: {
    mode: 'helper', helperState: 'ok', helperPath: '/usr/local/libexec/tentanas-helper', helperVersion: '1.4.0',
    sudoersPath: '/etc/sudoers.d/tentanas', coreUser: 'tentaflow', coreVersion: '1.4.0', armedUntil: null, ttlSecs: 900,
    provisionedAt: '2026-07-12 08:00:00', provisionedBy: 'anna', auditEntries: 12841, coreCompatible: true,
  },
};

const pool = {
  name: 'tank', guid: '1', kind: 'zfs', state: 'online', health: 'ok', healthReason: '',
  sizeBytes: 4e12, allocBytes: 1e12, freeBytes: 3e12, usableBytes: 3.2e12, usedBytes: 1e12, availableBytes: 2.2e12,
  capacityPct: 31, fragmentationPct: 4, compressRatio: 1.31, dedupRatio: 1, ashift: 12, autotrim: false, readOnly: false,
  layout: 'raidz2', dataDisks: 2, faultTolerance: 2,
  vdevs: [{
    id: 'raidz2-0', role: 'data', kind: 'raidz2', state: 'online', faultTolerance: 2,
    disks: [
      { diskId: 'sda', name: 'sda', path: '/dev/sda', state: 'online', readErrors: 0, writeErrors: 0, cksumErrors: 0, sizeBytes: 2e12, note: '' },
      { diskId: 'sdd', name: 'sdd', path: '/dev/sdd', state: 'online', readErrors: 1, writeErrors: 0, cksumErrors: 2, sizeBytes: 2e12, note: '' },
    ],
  }],
  scan: { kind: 'scrub', status: 'finished', progressPct: 100, startedAt: null, finishedAt: '2026-08-30 04:00:00', durationSecs: 600, etaSecs: 0, errors: 0, scannedBytes: 1e12 },
  readErrors: 0, writeErrors: 0, cksumErrors: 0, datasetCount: 2, snapshotCount: 5,
  io: { readBps: 0, writeBps: 0, readIops: 0, writeIops: 0 },
  compression: 'zstd', encryption: false, scrubSchedule: null, lastScrubAt: '2026-08-30 04:00:00', nextScrubAt: null,
};

const arc = {
  sizeBytes: 2e9, maxBytes: 2e9, minBytes: 1e8, ramBytes: 8e9, hitRatio: 94.2,
  mruBytes: 8e8, mfuBytes: 1.2e9, demandHits: 91, prefetchHits: 9,
  slogPools: [], l2arcPools: [], limitSource: 'modprobe',
};

const share = {
  shareId: 's1', name: 'projekty', protocol: 'smb', sourcePath: '/tank/projekty', dataset: 'tank/projekty',
  enabled: true, smb: null, nfs: null, fleetMount: true,
  mounts: [{ nodeId: LOCAL, nodeName: 'orion', state: 'source', detail: '', mountpoint: '/tank/projekty', checkedAt: null }],
  sessions: 14, state: 'active', stateDetail: '', createdAt: '2026-01-01 00:00:00', updatedAt: '2026-01-01 00:00:00',
};

// Records every call with its forwarding options so the tests can assert
// which node a request was addressed to.
const calls = [];
function stubTransport(fixtures) {
  const answer = (kind, payload, options) => {
    calls.push({ kind, payload, options: options || {} });
    if (!(kind in fixtures)) return Promise.reject(new Error(`unexpected request ${kind}`));
    const f = fixtures[kind];
    try {
      return Promise.resolve(typeof f === 'function' ? f(payload, options || {}) : f);
    } catch (e) {
      return Promise.reject(e);
    }
  };
  ApiBinary.one = (kind, payload) => answer(kind, payload, {});
  ApiBinary.action = (kind, payload, options) => answer(kind, payload, options);
}

const fixtures = {
  authMeRequest: { role: 'admin' },
  tentaNasNodesListRequest: {
    localNodeId: LOCAL,
    nodes: [
      node({}),
      // 'unset' is what `elevation::Mode::as_str` puts on the wire for a node
      // with no channel; 'unarmed' was this fixture's own spelling.
      node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, health: 'warning', disksWarning: 1, elevationMode: 'unset', poolsTotal: 0, features: [] }),
      node({ nodeId: MAC, nodeName: 'mini', isLocal: false, instanceStatus: 'unsupported', osName: 'macOS', disksTotal: 0, poolsTotal: 0, features: [] }),
    ],
  },
  tentaNasEnvironmentRequest: { environment },
  tentaNasElevationPlanRequest: { plan: { helperSource: '/opt/tentaflow/tentanas-helper', helperSourcePresent: true, helperPath: '/usr/local/libexec/tentanas-helper', sudoersPath: '/etc/sudoers.d/tentanas', sudoersLine: 'tentaflow ALL=(root) NOPASSWD: /usr/local/libexec/tentanas-helper', coreUser: 'tentaflow', coreVersion: '1.4.0', commands: [['install', '-m', '0755', '/opt/tentaflow/tentanas-helper', '/usr/local/libexec/tentanas-helper']] } },
  tentaNasDisksListRequest: {
    disks: [disk({}), disk({ diskId: 'nvme0n1', name: 'nvme0n1', path: '/dev/nvme0n1', kind: 'nvme', model: 'Samsung 980', serial: 'S-1', health: 'warning', healthReason: 'pending sectors', wearPct: 12, rotational: false })],
    telemetry: { sampledAt: '2026-09-02 10:00:00', smartReadAt: '2026-09-02 09:59:00', smartState: 'ok', detail: '' },
    iopsHourAvg: 16,
  },
  tentaNasJobsListRequest: { jobs: [{ jobId: 'j1', kind: 'smart_test', subject: 'sda', status: 'running', progressPct: 40, startedBy: 'admin', startedAt: '2026-09-02 09:58:00', finishedAt: null, error: null, log: ['started'] }] },
  tentaNasAlertsListRequest: { alerts: [] },
  tentaNasPoolsListRequest: { pools: [pool], freeDisks: [disk({})] },
  tentaNasElasticArraysListRequest: { arrays: [] },
  tentaNasElasticCapabilitiesRequest: { capabilities: { mergerfs: true, snapraid: true, filesystems: ['xfs', 'ext4'] }, freeDisks: [disk({})] },
  tentaNasArcStatsRequest: { arc },
  tentaNasSharesListRequest: { shares: [share], services: [{ protocol: 'smb', installed: true, running: true, version: '4.21', configPath: '/etc/samba/tentanas.conf', detail: '' }], users: [], mountRoot: '/mnt/tentanas' },
  tentaNasSchedulesListRequest: { rows: [], smart: { enabled: true, short: { every: 'daily', hour: 1, minute: 0, weekday: 0, day: 1 }, long: { every: 'monthly', hour: 4, minute: 0, weekday: 0, day: 1 }, lastShortAt: null, lastLongAt: null, nextShortAt: null, nextLongAt: null } },
};

async function mountScreen(params = {}) {
  calls.length = 0;
  document.body.innerHTML = Screen.render();
  await Screen.mount(params);
  await flush();
  return document.getElementById('nas-root');
}

const kinds = (kind) => calls.filter((c) => c.kind === kind);

// `assert.equal(el, null)` reads harmlessly and is a trap. On FAILURE node
// builds the diff with `util.inspect` over a happy-dom element, which walks the
// whole document graph: measured at ~96–107s, after which the runner reports the
// FILE as failed with no test name at all and the remaining tests never run. It
// is fast while passing, so the cost only appears when a test breaks — mutation
// testing and real regressions, exactly when the name is what you need. Compare
// a primitive instead.
const absent = (root, sel) => root.querySelector(sel) === null;

test('mount koduje AuthMe jako unit przez rzeczywisty codec i WASM', {
  skip: existsSync(new URL('../protocol/wasm_glue_bg.wasm', import.meta.url)) ? false : 'Brak wygenerowanego WASM; kodowanie nie zostało sprawdzone',
}, async () => {
  const wasm = await import('../protocol/wasm_glue.js');
  await wasm.default({ module_or_path: readFileSync(new URL('../protocol/wasm_glue_bg.wasm', import.meta.url)) });
  // Izolowana instancja nie dziedziczy nieudanego fetch z bootstrapu DOM.
  const codec = await import('../protocol/codec.js?tentanas-auth-mount');
  await codec.codecReady;
  stubTransport(fixtures);
  const transport = ApiBinary.one;
  let decoded = null;
  let encodingError = null;
  ApiBinary.one = (kind, ...args) => {
    if (kind === 'authMeRequest') {
      try {
        const envelope = wasm.decodeEnvelope(codec.encode[kind](71, ...args, 9));
        try {
          decoded = { correlationId: envelope.correlation_id, sequence: envelope.sequence,
            body: wasm.decodeMessageBody(envelope.body) };
        } finally { envelope.free(); }
      } catch (error) { encodingError = error.message; throw error; }
    }
    return transport(kind, ...args);
  };
  try {
    const root = await mountScreen({ node: LOCAL });
    assert.equal(encodingError, null);
    assert.deepEqual(decoded, { correlationId: 71n, sequence: 9n, body: { variant: 'AuthMeRequest' } });
    assert.equal(Screen.isAdmin, true);
    assert.ok(root.querySelector('#nas-tabs'));
  } finally { Screen.unmount(); ApiBinary.one = transport; }
});

test('routing Elastic zachowuje nazwę po mount i wyklucza pool/dataset', async () => {
  const array = { name: 'media', kind: 'elastic-array', state: 'active', enabled: true, filesystem: 'xfs', unionPath: '/mnt/media', dataDisks: [], parityDisks: [], protection: { status: 'unprotected' }, snapraid: {} };
  stubTransport({ ...fixtures, tentaNasElasticArrayGetRequest: { array } });
  const root = await mountScreen({ node: LOCAL, tab: 'pools', array: 'media', pool: 'tank', dataset: 'tank/a' });
  assert.equal(Screen.array, 'media');
  assert.equal(Screen.pool, null);
  assert.equal(Screen.dataset, null);
  assert.ok(root.querySelector('.nas-elastic-detail'));
  Screen.setLocation();
  assert.match(window.location.hash, /array=media/);
  assert.doesNotMatch(window.location.hash, /[?&]pool=|dataset=/);
  click(root.querySelector('.nas-elastic-detail .nas-crumbs a'));
  await flush();
  assert.equal(Screen.array, null);
  assert.ok(root.querySelector('#nas-pools-list'));
  Screen.openArray('media');
  await flush();
  Screen.openPool('tank');
  await flush();
  assert.equal(Screen.array, null);
  assert.match(window.location.hash, /pool=tank/);
  assert.doesNotMatch(window.location.hash, /array=/);
  Screen.unmount();
});

test('rzeczywiste wiersze jobów Elastic nie mają Anuluj, log pozostaje dostępny', async () => {
  stubTransport(fixtures);
  await mountScreen({ node: LOCAL, tab: 'pools' });
  const host = document.createElement('div');
  document.body.append(host);
  const jobs = ['elastic_create', 'elastic_restore', 'pool_scrub'].map((kind) => ({ jobId: kind, kind, subject: 'media', status: 'running', progressPct: 1, log: [] }));
  host.innerHTML = jobs.map((job) => Screen.jobRowHtml(job)).join('');
  assert.equal(host.querySelectorAll('[data-act="cancel"]').length, 1);
  assert.equal(host.querySelectorAll('[data-act="log"]').length, 3);
  assert.match(host.textContent, /Tworzenie Elastic Array/);
  assert.match(host.textContent, /Przywracanie montowania Elastic Array/);
  Screen.wireJobRows(host, () => {});
  host.remove();
  Screen.unmount();
});

test('fleet view lists every node and only ready nodes open', async () => {
  stubTransport(fixtures);
  const root = await mountScreen();
  const cards = root.querySelectorAll('.node-card');
  assert.equal(cards.length, 3);
  assert.ok(cards[2].classList.contains('unsupported'), 'macOS node is marked unsupported');
  assert.match(cards[1].textContent, /vega/);

  cards[2].click();
  await flush();
  assert.equal(Screen.nodeId, null, 'unsupported node does not open');

  cards[1].click();
  await flush();
  assert.equal(Screen.nodeId, REMOTE);
  assert.ok(root.querySelector('.tf-detail-header'), 'node view header rendered');
  Screen.unmount();
});

test('the node card follows n01: health dot, stats as key/value pairs, channel chip and role', async () => {
  stubTransport(fixtures);
  const root = await mountScreen();
  const card = root.querySelectorAll('.node-card')[0];
  assert.ok(card.querySelector('.nc-head > .health-dot'), 'the head opens with a health dot, not an icon');
  assert.equal(card.querySelector('.nc-name').textContent.trim(), 'orion');
  assert.match(card.querySelector('.nc-sub').textContent, /Debian 12 · OpenZFS 2\.2\.4/);
  // n01: split bar between the head and the stats, four kv pairs, no Alerty counter.
  const order = [...card.children].map((c) => c.className.split(' ')[0]);
  assert.deepEqual(order, ['nc-head', 'split-bar', 'nc-stats', 'nc-foot']);
  const stats = [...card.querySelectorAll('.nc-stats .kv-inline')].map((kv) => kv.querySelector('.k').textContent);
  assert.deepEqual(stats, ['Pojemność łączna', 'Dyski', 'Pule', 'Share']);
  assert.equal(card.querySelector('.nc-stats .kv-inline .v').textContent, '931 GiB / 3.6 TiB', 'capacity moved into the stats');
  const foot = card.querySelector('.nc-foot');
  assert.equal(foot.querySelector('tf-chip').getAttribute('label'), 'tryb A');
  assert.match(foot.lastElementChild.textContent, /NAS floty/);
  Screen.unmount();
});

test('the node card sub carries RAM and uptime, and drops them when the node published none (n01:197)', async () => {
  stubTransport({
    ...fixtures,
    tentaNasNodesListRequest: {
      localNodeId: LOCAL,
      nodes: [
        node({ ramBytes: 137438953472, uptimeSecs: 3556800 }),
        node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, ramBytes: 0, uptimeSecs: 0 }),
      ],
    },
  });
  const root = await mountScreen();
  const subs = [...root.querySelectorAll('.node-card .nc-sub')].map((s) => s.textContent);
  assert.equal(subs[0], 'Debian 12 · OpenZFS 2.2.4 · 128 GiB RAM · uptime 41 d 4 h · ten węzeł');
  assert.equal(subs[1], 'Debian 12 · OpenZFS 2.2.4', 'a node without a summary shows neither RAM nor uptime');
  Screen.unmount();
});

test('the fleet header is the canonical detail-header and the tab strip has no active tab', async () => {
  stubTransport(fixtures);
  const root = await mountScreen();
  await flush();
  const head = root.querySelector('.tf-detail-header');
  assert.ok(head, 'detail header rendered instead of a page-head');
  assert.match(head.querySelector('.d-name').textContent, /TentaNas/);
  const chips = [...head.querySelectorAll('.d-name tf-chip')].map((c) => c.getAttribute('label'));
  assert.ok(chips.some((l) => /ostrzeżeni/.test(l)), `a warning chip is present: ${chips}`);
  assert.ok(chips.some((l) => /Usługi/.test(l)), `a services chip is present: ${chips}`);
  const sub = head.querySelector('.d-sub').textContent;
  assert.match(sub, /^flota · 3 węzły · 2 wspierane/);
  assert.match(sub, /TentaNas 1\.4\.0/);
  assert.match(sub, /ostatnie odświeżenie/);
  const badges = [...head.querySelectorAll('.d-badges tf-chip')].map((c) => c.getAttribute('label'));
  assert.match(badges[0], /1× NAS: orion/);
  assert.match(badges[1], /Kanały uprawnień: 1× tryb A · 1× nieuzbrojony/);
  assert.match(badges[3], /mesh: 3 węzły/);
  assert.ok(head.querySelector('[data-act="export-config"]'), 'export action present');

  const tabs = root.querySelector('#nas-tabs');
  assert.equal(tabs.getAttribute('value'), '', 'an empty value means no tab is active');
  assert.equal(tabs.querySelectorAll('tf-tab').length, 6);
  assert.equal(tabs.querySelector('tf-tab#pools').getAttribute('count'), '1', 'the one pool of the default fixture');
  assert.equal(tabs.querySelectorAll('button.tf-tab.active').length, 0, 'no tab is highlighted on the fleet view');
  Screen.unmount();
});

test('clicking a fleet tab opens that tab on the default node', async () => {
  stubTransport(fixtures);
  const root = await mountScreen();
  await flush();
  click(root.querySelector('#nas-tabs tf-tab#disks button.tf-tab'));
  await flush();
  assert.equal(Screen.nodeId, LOCAL, 'routed to the first supported node');
  assert.equal(Screen.tab, 'disks');
  Screen.unmount();
});

test('fleet alerts and resources aggregate every node and keep an unreachable node visible', async () => {
  stubTransport({
    ...fixtures,
    tentaNasAlertsListRequest: (payload, options) => {
      if (options.targetNodeId === REMOTE) throw new Error('mesh timeout');
      return { alerts: [{ alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'sda', title: 'sda: 3 realokacje', detail: 'w 7 dni', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null }] };
    },
    tentaNasSharesListRequest: (payload, options) => {
      if (options.targetNodeId === REMOTE) throw new Error('mesh timeout');
      return fixtures.tentaNasSharesListRequest;
    },
  });
  const root = await mountScreen();
  await flush();
  await flush();

  const alerts = root.querySelector('#nas-fleet-alerts').rows;
  assert.equal(alerts.length, 2, 'one alert plus one offline row');
  assert.match(alerts[0].alert, /3 realokacje/);
  // n01 spells the level as an alert severity, never as the disk-health word.
  assert.match(alerts[0].level, /label="ostrzeżenie"/);
  assert.ok(!/Uwaga/.test(alerts[0].level), 'the disk-health wording stays on n03/n04');
  assert.match(alerts[1].level, /offline/);
  assert.match(alerts[1].alert, /mesh timeout/);

  const res = root.querySelector('#nas-fleet-res-table').rows;
  assert.equal(res.length, 2, 'one share plus the unreachable node');
  assert.match(res[0].resource, /projekty/);
  assert.match(res[0].mounts, /mount-dots/);
  assert.match(res[1].source, /mesh timeout/);
  Screen.unmount();
});

test('requests for a remote node carry its forward target; the local node does not', async () => {
  stubTransport(fixtures);
  await mountScreen({ node: REMOTE, tab: 'disks' });
  const remote = kinds('tentaNasDisksListRequest');
  assert.ok(remote.length >= 1);
  assert.equal(remote[0].options.targetNodeId, REMOTE);
  Screen.unmount();

  await mountScreen({ node: LOCAL, tab: 'disks' });
  const local = kinds('tentaNasDisksListRequest');
  assert.ok(local.length >= 1);
  assert.equal(local[0].options.targetNodeId, undefined);
  Screen.unmount();
});

test('disks tab renders one row per disk and the filters narrow the set', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  const table = root.querySelector('#nas-disk-table');
  assert.equal(table.rows.length, 2);
  assert.match(table.rows[1].device, /nvme/);
  assert.match(table.rows[1].health, /health-dot warn/);

  // n03:254 — the health cell of a warning disk says "Uwaga", not "Ostrzeżenie".
  assert.match(table.rows[1].health, />Uwaga</);

  const chips = root.querySelector('#nas-disk-filters').filters.map((f) => f.label);
  assert.deepEqual(chips, ['Wszystkie 2', 'HDD 1', 'SSD · NVMe 1', 'Problemy 1', 'Wolne 2']);
  assert.ok(root.querySelector('#nas-disk-table').hasAttribute('selectable'), 'rows are selectable');
  assert.match(root.querySelector('.legend-strip').textContent, /brak symptomów we wszystkich źródłach/);
  // n03:478-479 — the legend strip is OK / Uwaga / Awaria.
  assert.deepEqual([...root.querySelectorAll('.legend-strip tf-chip')].map((c) => c.getAttribute('label')), ['OK', 'Uwaga', 'Awaria']);

  Screen.diskFilter = 'problems';
  Screen.applyDiskRows();
  assert.equal(table.rows.length, 1);
  assert.equal(table.rows[0]._disk.diskId, 'nvme0n1');

  Screen.diskFilter = 'all';
  Screen.diskQuery = 'wd-1';
  Screen.applyDiskRows();
  assert.equal(table.rows.length, 1);
  assert.equal(table.rows[0]._disk.diskId, 'sda');
  Screen.diskQuery = '';
  Screen.unmount();
});

test('the role chip names the pool first and then the vdev it serves (n03:210)', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      disks: [
        disk({ diskId: 'sdd', name: 'sdd', role: 'pool', memberOf: 'tank', health: 'critical', vdevRole: 'data', vdevKind: 'raidz2' }),
        disk({ diskId: 'sdn', name: 'sdn', role: 'pool', memberOf: 'tank', vdevRole: 'special', vdevKind: 'mirror' }),
        disk({ diskId: 'sdz', name: 'sdz', role: 'spare', memberOf: 'backup' }),
        disk({}),
      ],
      telemetry: fixtures.tentaNasDisksListRequest.telemetry,
    },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  const rows = root.querySelector('#nas-disk-table').rows;
  assert.equal(rows[0].role.label, 'tank · RAIDZ2', 'the vdev layout, not the generic "Pula"');
  assert.equal(rows[1].role.label, 'tank · Special', 'a non-data group is named by its role');
  assert.equal(rows[2].role.label, 'backup · Zapasowy', 'no vdev on the row → the generic role');
  assert.equal(rows[3].role.label, 'Wolny', 'a free disk belongs to no pool');
  assert.match(rows[0].health, />Awaria</, 'n03 spells a critical disk "Awaria"');
  // The chip is built from the inventory alone: the five-second disk poll
  // must not pull the pool topology a second time.
  assert.equal(kinds('tentaNasPoolsListRequest').length, 0, 'no pool listing on the disks tab');
  Screen.unmount();
});

test('an Elastic Array member is named by its array and the part it plays (n03:210)', async () => {
  // An Elastic Array leaves nothing on its disks — the union is a mergerfs
  // mount over one ordinary filesystem per branch — so without the array's own
  // field every one of these rows read "Zajęty · xfs", 23 times on the node
  // this was measured on. `arrayRole` must NOT travel as `vdevRole`: 'data'
  // there means a ZFS top-level vdev and would be printed as a RAID layout.
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      disks: [
        disk({ diskId: 'sdg', name: 'sdg', role: 'array_member', memberOf: 'produkt', arrayRole: 'data' }),
        disk({ diskId: 'nvme1n1', name: 'nvme1n1', role: 'array_member', memberOf: 'produkt', arrayRole: 'cache' }),
        disk({ diskId: 'sdh', name: 'sdh', role: 'array_member', memberOf: 'produkt', arrayRole: 'parity' }),
        disk({ diskId: 'sdi', name: 'sdi', role: 'used', fsType: 'xfs' }),
        disk({ diskId: 'sdk', name: 'sdk', role: 'array_member', memberOf: 'produkt', arrayRole: 'witness' }),
      ],
      telemetry: fixtures.tentaNasDisksListRequest.telemetry,
    },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  const rows = root.querySelector('#nas-disk-table').rows;
  assert.equal(rows[0].role.label, 'produkt \u00b7 Dane', 'the array and the part, not the filesystem');
  assert.equal(rows[1].role.label, 'produkt \u00b7 Cache');
  assert.equal(rows[2].role.label, 'produkt \u00b7 Parzysto\u015b\u0107');
  assert.equal(rows[3].role.label, 'Zaj\u0119ty \u00b7 xfs', 'a filesystem nothing owns still names itself');
  assert.equal(rows[4].role.label, 'produkt \u00b7 W macierzy', 'a part this build has no word for degrades to the membership, never a raw key');
  // Same contract the vdev chip has: the chip is built from the inventory
  // alone, so the five-second disk poll must not pull the array list too.
  assert.equal(kinds('tentaNasElasticArraysListRequest').length, 0, 'no array listing on the disks tab');
  Screen.unmount();
});

test('the bulk SMART button starts a short test for every selected disk', async () => {
  stubTransport({ ...fixtures, tentaNasDiskSmartTestRequest: { job: { jobId: 'j2', kind: 'smart_test', subject: 'sda', status: 'queued', log: [] } } });
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  const table = root.querySelector('#nas-disk-table');
  const btn = root.querySelector('[data-act="smart-bulk"]');
  assert.ok(btn.hasAttribute('disabled'), 'nothing selected yet');

  for (const row of table.rows) {
    table.dispatchEvent(new window.CustomEvent('row-select', { detail: { row, index: 0, selected: true } }));
  }
  assert.equal(btn.hasAttribute('disabled'), false);
  assert.equal(btn.textContent, 'Test SMART zaznaczonych (2)');

  await Screen.startSmartTestBulk();
  await flush();
  const sent = kinds('tentaNasDiskSmartTestRequest');
  assert.deepEqual(sent.map((c) => c.payload.diskId), ['sda', 'nvme0n1']);
  assert.ok(sent.every((c) => c.payload.kind === 'short'), 'short self-test');
  assert.equal(Screen.diskSelection.size, 0, 'the selection is cleared after the batch');
  Screen.unmount();
});

test('environment tab lists features, the fleet nodes with their capabilities and the elevation rows', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL, tab: 'environment' });
  await flush();
  await flush();
  const features = root.querySelector('#nas-feature-table').rows;
  assert.equal(features.length, 1);
  assert.match(features[0].version, /2\.2\.4 · JSON ✓/, 'version and detail merged into one column');

  const others = root.querySelector('#nas-others-table').rows;
  assert.equal(others.length, 2);
  const vega = others.find((r) => r._node.nodeId === REMOTE);
  assert.equal(vega.channel.status, 'warn', 'unarmed node flagged');
  assert.equal(others.find((r) => r._node.nodeId === MAC).features, 'platforma nieobsługiwana');

  const rows = [...root.querySelectorAll('.section-card .stat-rows .sr')].map((r) => r.textContent);
  assert.ok(rows.some((r) => /Helper.*tentanas-helper v1\.4\.0 · zgodny z core/.test(r)), `helper row: ${rows[0]}`);
  assert.ok(rows.some((r) => /Sudoers.*1 linia/.test(r)));
  assert.ok(rows.some((r) => /Provisioning.*anna/.test(r)));
  assert.ok(rows.some((r) => /Audyt wywołań.*12841 wpisów.*Dziennik/.test(r)));
  assert.equal(root.querySelector('[data-act="remove"]').textContent, 'Przejdź na tryb B…');

  // n16:185-197 — two headingless boxes; "Konsekwencje:" holds three losses.
  const boxes = [...root.querySelectorAll('.grid-2 .explain-box')];
  assert.equal(boxes.length, 2);
  assert.equal(absent(boxes[0], 'h4'), true, 'no invented heading');
  assert.match(boxes[0].textContent.trim(), /^Tryb A \(obecny\): jednorazowo podane hasło sudo/);
  assert.match(boxes[1].textContent, /Tryb B \(opt-out\):.*\(sesja 15 min\)\. Konsekwencje:/);
  assert.deepEqual([...boxes[1].querySelectorAll('.ll')].map((l) => l.textContent), [
    'harmonogramy wyłączone (scrub, mover, snapshoty),',
    'zdrowie dysków aktualizowane tylko w uzbrojonej sesji,',
    'po restarcie share\'y wracają dopiero po ręcznym uzbrojeniu.',
  ]);
  assert.equal(root.querySelectorAll('.grid-2 .explain-box .ll.good').length, 0, 'no benefit inside "Konsekwencje"');
  Screen.unmount();
});

test('the helper catalog button asks the core and renders one row per command', async () => {
  stubTransport({
    ...fixtures,
    tentaNasElevationCatalogRequest: { commands: [{ name: 'arc_limit_set', description: 'Cap the ZFS ARC.', tool: 'tee', builtin: false, needsStdin: true }] },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'environment' });
  await flush();
  await flush();
  click(root.querySelector('[data-act="catalog"]'));
  await flush();
  await flush();
  assert.equal(kinds('tentaNasElevationCatalogRequest').length, 1);
  const win = document.querySelector('tf-window.nas-modal');
  assert.equal(windowTitle(win), 'Katalog poleceń helpera');
  const rows = win.querySelector('#nas-cat-table').rows;
  assert.equal(rows.length, 1);
  assert.match(rows[0].name, /arc_limit_set/);
  assert.match(rows[0].tool, /stdin/);
  win.remove();
  Screen.unmount();
});

test('withSudo skips the prompt on a provisioned helper and asks for a password when unarmed', async () => {
  stubTransport(fixtures);
  await mountScreen({ node: LOCAL });
  await flush();
  let seen = 'unset';
  await Screen.withSudo(async (password) => { seen = password; return {}; }, 'x');
  assert.equal(seen, undefined, 'helper channel needs no password');
  assert.equal(absent(document, 'tf-window.nas-modal'), true, 'no prompt opened');

  Screen.environment = { ...environment, elevation: { ...environment.elevation, mode: 'unset', helperState: 'absent' } };
  const pending = Screen.withSudo(async (password) => { seen = password; return {}; }, 'x');
  await flush();
  const prompt = document.querySelector('tf-window.nas-modal');
  assert.ok(prompt, 'prompt opened for an unarmed channel');
  // A one-shot prompt runs the operation, it does not arm anything; the TTL
  // is still spelled out twice (n17:220, n17:227).
  const confirm = prompt.querySelector('[data-action="confirm"]');
  assert.equal(confirm.textContent, 'Wykonaj');
  assert.equal(confirm.getAttribute('icon'), 'key');
  assert.match(prompt.querySelector('.toggle-card').textContent, /Zapamiętaj na czas sesji administracyjnej \(15 min\)/);
  assert.match(prompt.querySelector('.wizard-warning.info').textContent, /Po upływie 15 min .* hasło jest czyszczone z pamięci/);
  prompt.querySelector('#nas-sudo-pass').value = 'hunter2';
  prompt.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' } }));
  await pending;
  assert.equal(seen, 'hunter2');
  prompt.remove();
  Screen.unmount();
});

test('"Uzbrój kanał" stays on the arm-channel prompt and never labels a one-shot sudo prompt', async () => {
  stubTransport(fixtures);
  await mountScreen({ node: LOCAL });
  await flush();
  Screen.environment = { ...environment, elevation: { ...environment.elevation, mode: 'unset', helperState: 'absent' } };

  const pending = Screen.withSudo(async () => ({}), 'Zniszcz pulę tank');
  await flush();
  const generic = document.querySelector('tf-window.nas-modal');
  assert.equal(generic.querySelector('[data-action="confirm"]').textContent, 'Wykonaj');
  generic.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'cancel' } }));
  assert.equal(await pending, null);
  generic.remove();

  const arming = Screen.armNode({ nodeId: REMOTE, nodeName: 'vega', isLocal: false });
  await flush();
  const prompt = document.querySelector('tf-window.nas-modal');
  assert.equal(windowTitle(prompt), 'Uzbrój kanał uprawnień — vega (tryb B)');
  assert.equal(prompt.querySelector('[data-action="confirm"]').textContent, 'Uzbrój kanał');
  prompt.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'cancel' } }));
  await arming;
  assert.equal(kinds('tentaNasElevationArmRequest').length, 0, 'a cancelled prompt arms nothing');
  prompt.remove();
  Screen.unmount();
});

// The tab used to carry NO count at all, and deliberately: `poolsTotal` is ZFS
// pools alone, so a node whose only storage is an Elastic Array would have read
// as having none — worse than silence. `arraysTotal` is on the wire now, so the
// count can be what the Pools tab actually lists. The original intent is the
// assertion that survives: the number must never be the ZFS count on its own.
test('zakładka Pule liczy pule ZFS RAZEM z macierzami, pozostałe badge pozostają pomiarami węzła', async () => {
  stubTransport({ ...fixtures, tentaNasNodesListRequest: { localNodeId: LOCAL,
    nodes: [node({ poolsTotal: 0, arraysTotal: 2 }), node({ nodeId: REMOTE, isLocal: false, poolsTotal: 7, disksTotal: 9, sharesTotal: 3 })] } });
  const root = await mountScreen({ node: LOCAL, tab: 'pools' });
  try {
    assert.equal(root.querySelector('tf-tab#pools').getAttribute('count'), '2',
      'no ZFS pool, two arrays — the node is not storage-less');
    assert.equal(root.querySelector('tf-tab#disks').getAttribute('count'), '2');
    assert.equal(root.querySelector('tf-tab#shares').getAttribute('count'), '1');
    assert.equal(root.querySelector('tf-tab#jobs').getAttribute('count'), '1');
    Screen.selectNode(REMOTE, 'pools');
    await flush();
    assert.equal(root.querySelector('tf-tab#pools').getAttribute('count'), '7', 'pools still count');
    assert.equal(root.querySelector('tf-tab#disks').getAttribute('count'), '9');
    assert.equal(root.querySelector('tf-tab#shares').getAttribute('count'), '3');
  } finally { Screen.unmount(); }
});

test('the node header carries the disk-warning and service chips plus the mockup badges', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const chips = [...root.querySelectorAll('#nas-head-chips tf-chip')].map((c) => c.getAttribute('label'));
  assert.deepEqual(chips, ['Dyski OK', 'Usługi nieaktywne']);
  const badges = [...root.querySelectorAll('#nas-head-badges tf-chip')].map((c) => c.getAttribute('label'));
  assert.equal(badges[0], 'OpenZFS 2.2.4');
  assert.equal(badges[1], 'Kanał uprawnień: tryb A');
  assert.equal(badges[2], 'mesh: 3 węzły');
  const sub = root.querySelector('#nas-head-sub').textContent;
  assert.match(sub, /^węzeł orion · uptime 1 h 0 min · TentaNas 1\.4\.0 · ostatnie odświeżenie/);
  assert.ok(!/6\.1/.test(sub), 'the kernel is not in the header sub');
  Screen.unmount();
});

test('the overview KPI tiles follow n02 and drill down into pools and disks', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const tiles = [...root.querySelectorAll('#nas-ov-kpi tf-stat-card')];
  assert.deepEqual(tiles.map((t) => t.getAttribute('label')), ['Pojemność łączna', 'Zdrowie dysków', 'IOPS (teraz)', 'Przepustowość']);
  assert.equal(tiles[1].getAttribute('value'), '1');
  assert.equal(tiles[1].getAttribute('suffix'), 'ostrzeżenie');
  assert.equal(tiles[2].getAttribute('value'), '20');
  // n02:181 — the IOPS tile compares now against the node's hourly mean.
  assert.equal(tiles[2].getAttribute('delta'), '+25% vs śr. godzinowa');
  assert.equal(tiles[2].getAttribute('delta-type'), 'up');
  // Asserted on the RENDERED delta, not on the attribute alone: tf-stat-card
  // silently rewrites any value outside its allowlist to `neutral`, so an
  // attribute-only assertion passes while the tile shows no warning at all.
  assert.equal(tiles[1].getAttribute('delta-type'), 'warn');
  const warnDelta = tiles[1].querySelector('.tf-stat-card-delta');
  assert.ok(warnDelta.classList.contains('warn'), 'the warned health tile keeps its warn tone');
  assert.match(warnDelta.textContent, /⚠/, 'and its warning glyph');

  click(tiles[1]);
  await flush();
  assert.equal(Screen.tab, 'disks');
  assert.equal(Screen.diskFilter, 'problems');
  Screen.unmount();
});

test('the IOPS tile names the hourly mean when the sampler has no baseline yet', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: { ...fixtures.tentaNasDisksListRequest, iopsHourAvg: 0 },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const iops = [...root.querySelectorAll('#nas-ov-kpi tf-stat-card')][2];
  assert.equal(iops.getAttribute('value'), '20');
  assert.equal(iops.getAttribute('delta'), 'śr. godzinowa 0 IOPS', 'no percentage against a zero baseline');
  assert.equal(iops.getAttribute('delta-type'), null);
  Screen.unmount();
});

test('the overview ARC card renders the donut and its "Zmień limit" button opens the environment tab', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const card = root.querySelector('#nas-ov-arc');
  assert.match(card.querySelector('.donut .dn-val').textContent, /94\.2%/);
  const rows = [...card.querySelectorAll('.sr')].map((r) => r.textContent);
  assert.match(rows[0], /Użycie \/ limit/);
  assert.match(rows[1], /Podział MRU \/ MFU\s*40% \/ 60%/);
  assert.match(rows[3], /SLOG \(zapisy sync\)\s*brak/);
  assert.match(rows[4], /L2ARC\s*brak — dodaj do tank/);
  click(card.querySelector('[data-act="arc-l2arc"]'));
  await flush();
  assert.equal(Screen.pool, 'tank', 'the L2ARC hint opens the pool that would get the cache vdev');
  Screen.pool = null;
  Screen.tab = 'overview';
  Screen.drawTab();
  await flush();

  const btn = root.querySelector('[data-act="arc-limit"]');
  assert.equal(btn.textContent, 'Zmień limit (25% RAM)');
  click(btn);
  await flush();
  assert.equal(Screen.tab, 'environment');
  Screen.unmount();
});

test('the overview pool mini-list opens a pool and the running jobs are listed next to the alerts', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL });
  await flush();
  assert.ok(root.querySelector('#nas-ov-jobs .job-row'), 'running job listed');
  const mini = root.querySelectorAll('#nas-ov-pools .pool-mini');
  assert.equal(mini.length, 1);
  assert.match(mini[0].querySelector('.pm-name').textContent, /tank/);
  click(mini[0]);
  await flush();
  assert.equal(Screen.tab, 'pools');
  assert.equal(Screen.pool, 'tank');
  Screen.unmount();
});

// --- n02: the node dashboard must show EVERY pool, not only the ZFS half ----
// The Pools tab lists ZFS pools and Elastic Arrays side by side, but the
// dashboard fetched only the ZFS list: on the live node `#nas-ov-pools` held
// one row while the tab held two, so a whole pool — its capacity and its
// protection state — was missing from the screen the admin lands on.
const TIB = 1024 ** 4;
const elasticArray = (overrides) => ({
  name: 'produkt', kind: 'elastic-array', filesystem: 'xfs', state: 'active', unionPath: '/mnt/produkt',
  usableBytes: 10 * TIB, usedBytes: 6 * TIB,
  dataDisks: [{ diskId: 'sdb', sizeBytes: 5 * TIB }, { diskId: 'sdc', sizeBytes: 5 * TIB }, { diskId: 'sdg', sizeBytes: 5 * TIB }],
  parityDisks: [{ diskId: 'sdh', sizeBytes: 5 * TIB }],
  protection: { status: 'window_open', protectedAsOf: '2026-09-02 09:00:00' },
  ...overrides,
});

test('the overview mini-list carries the Elastic Array too, and its row opens the ARRAY detail (n02:297)', async () => {
  stubTransport({ ...fixtures, tentaNasElasticArraysListRequest: { arrays: [elasticArray()] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const mini = [...root.querySelectorAll('#nas-ov-pools .pool-mini')];
  assert.equal(mini.length, 2, 'both pools the node has are on the dashboard');
  assert.equal(mini[0].dataset.pool, 'tank');
  assert.equal(mini[1].dataset.array, 'produkt');
  // `data-pool` on this row would send the click down the ZFS route, which has
  // no such pool to open — the row would look live and do nothing.
  assert.equal(mini[1].hasAttribute('data-pool'), false, 'the array row is keyed data-array');
  assert.match(mini[1].querySelector('.pm-name').textContent, /produkt/);
  assert.deepEqual(
    [...mini[1].querySelectorAll('tf-chip')].map((c) => c.getAttribute('label')),
    ['Aktywna', 'Dane poza checkpointem'],
    'state and protection: the two questions an array raises',
  );
  // The tone is half the message: n02:300 draws the protection chip as a
  // WARNING, and an open checkpoint window rendered in the ok colour reads as
  // "everything is fine" at a glance.
  assert.deepEqual(
    [...mini[1].querySelectorAll('tf-chip')].map((c) => c.getAttribute('status')),
    ['ok', 'warn'],
  );
  assert.equal(mini[1].querySelector('.pm-sub').textContent.trim(), 'Elastic Array · mergerfs · 3 dane (XFS) + 1 parity');
  assert.equal(mini[1].querySelector('tf-progress-bar').getAttribute('value'), '60');
  assert.equal(mini[1].querySelector('.kv-inline .v').textContent, '6.0 TiB / 10 TiB');

  click(mini[1]);
  await flush();
  assert.equal(Screen.tab, 'pools');
  assert.equal(Screen.array, 'produkt', 'the row opens the array detail');
  assert.equal(Screen.pool, null, 'and not a ZFS pool of the same name');
  Screen.unmount();
});

test('the capacity KPI of the node dashboard counts the Elastic Array and its bytes', async () => {
  stubTransport({ ...fixtures, tentaNasElasticArraysListRequest: { arrays: [elasticArray()] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const tile = root.querySelector('#nas-ov-kpi [data-kpi="pools"]');
  // ZFS alone: 2.9 TiB / "931 GiB (31%) · 1 pula". The array's 10 TiB usable
  // and 6 TiB used are bytes in the same unit, so they belong in the same sum.
  assert.equal(tile.getAttribute('value'), '13 TiB');
  assert.equal(tile.getAttribute('delta'), 'zajęte 6.9 TiB (54%) · 2 pule');
  Screen.unmount();
});

test('an Elastic Array with no capacity reading is named in the KPI, never summed as zero', async () => {
  stubTransport({ ...fixtures, tentaNasElasticArraysListRequest: { arrays: [elasticArray({ usableBytes: null, usedBytes: null })] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const row = root.querySelector('#nas-ov-pools .pool-mini[data-array="produkt"]');
  assert.ok(row, 'an unmeasured array is still a pool the admin owns');
  // `pct` reads a missing capacity as 0, and a bar at 0% claims an empty array
  // where the node merely never measured one. "Nie zmierzono ≠ zero."
  assert.equal(absent(row, 'tf-progress-bar'), true, 'no fill bar for an unmeasured array');
  assert.equal(row.querySelector('.kv-inline .v').textContent, '— / —');
  const tile = root.querySelector('#nas-ov-kpi [data-kpi="pools"]');
  assert.equal(tile.getAttribute('value'), '2.9 TiB', 'the unmeasured array adds no bytes');
  assert.equal(tile.getAttribute('delta'), 'zajęte 931 GiB (31%) · 2 pule · 1 bez pomiaru pojemności');
  Screen.unmount();
});

test('an Elastic Array list that fails costs the dashboard its array rows and nothing else', async () => {
  stubTransport({ ...fixtures, tentaNasElasticArraysListRequest: () => { throw new Error('snapraid niedostępny'); } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  assert.equal(absent(root, '#nas-ov-error tf-alert'), true, 'one failing list is not a dashboard-wide failure');
  assert.equal(root.querySelectorAll('#nas-ov-kpi tf-stat-card').length, 4, 'the KPI row survives');
  assert.equal(root.querySelectorAll('#nas-ov-pools .pool-mini').length, 1, 'the ZFS pool still shows');
  assert.ok(root.querySelector('#nas-ov-arc .donut'), 'and so does the rest of the dashboard');
  Screen.unmount();
});

// The 5 s poll rule: `paintPoolsMini` writes ONE patched string for both kinds
// of pool, so a poll that brings identical data compares equal and touches no
// node at all. A per-row write, or a second host, would blink the array row —
// and a click landing mid-rebuild would land on a detached element.
test('a dashboard poll that brings the same Elastic Array mutates no node of the pool mini-list', async () => {
  stubTransport({ ...fixtures, tentaNasElasticArraysListRequest: { arrays: [elasticArray()] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const host = root.querySelector('#nas-ov-pools');
  const rows = [...host.querySelectorAll('.pool-mini')];
  assert.equal(rows.length, 2, 'both rows are on screen before the poll');
  const bar = rows[1].querySelector('tf-progress-bar');
  const chip = rows[1].querySelector('tf-chip');

  // Collected IN THE CALLBACK: the awaits below drain happy-dom's queue, so a
  // trailing takeRecords() alone would read an empty list and pass vacuously.
  const records = [];
  const obs = new window.MutationObserver((recs) => { records.push(...recs); });
  obs.observe(host, { childList: true, subtree: true, attributes: true, characterData: true });
  try {
    await Screen.refreshOverview(root.querySelector('#nas-tab-body'));
    await flush();
    records.push(...obs.takeRecords());
  } finally {
    obs.disconnect();
  }
  assert.equal(records.length, 0, `an unchanged poll must touch no node of the mini-list, saw ${records.map((r) => `${r.type}@${r.target.nodeName}`).join(', ')}`);
  const after = [...host.querySelectorAll('.pool-mini')];
  assert.equal(after.length, 2, 'and the rows are still both there');
  assert.equal(same(after[0], rows[0]), true, 'the ZFS row is the very same node');
  assert.equal(same(after[1], rows[1]), true, 'and so is the array row');
  assert.equal(same(after[1].querySelector('tf-progress-bar'), bar), true, 'its fill bar is not rebuilt');
  assert.equal(same(after[1].querySelector('tf-chip'), chip), true, 'nor is its state chip');
  Screen.unmount();
});

test('every overview alert row carries the drill-down of its subject (n02)', async () => {
  stubTransport({
    ...fixtures,
    tentaNasAlertsListRequest: {
      alerts: [
        { alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'nvme0n1', title: 'nvme0n1: pending sectors', detail: 'w 7 dni', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
        { alertId: 'a2', severity: 'critical', subjectKind: 'elevation', subjectId: '', title: 'kanał nieuzbrojony', detail: 'SMART nieczytany', raisedAt: '2026-09-01 11:00:00', ackedAt: null, resolvedAt: null },
      ],
    },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const rows = [...root.querySelectorAll('#nas-ov-alerts .alert-row')];
  assert.equal(rows.length, 2);
  assert.deepEqual(rows.map((r) => r.querySelector('[data-goto]').textContent.trim()), ['Szczegóły', 'Uzbrój']);
  assert.ok(rows[0].querySelector('[data-ack]'), 'Potwierdź stays the ghost action next to it');

  click(rows[1].querySelector('[data-goto]'));
  await flush();
  assert.equal(Screen.tab, 'environment');
  Screen.unmount();
});

test('a portal-drift alert drills down to the Sharing tab where its target lives', async () => {
  // The alert §5.5 raises when a portal's address moves (owner decision
  // 2026-09-04). Without a branch of its own it would fall through to the
  // overview, which is where the admin already is.
  stubTransport({
    ...fixtures,
    tentaNasAlertsListRequest: {
      alerts: [{
        alertId: 'a1',
        severity: 'warning',
        subjectKind: 'target',
        // The node puts the target's NAME here, not its UUID: this is the
        // string the alert row prints next to the kind, and a bare
        // `0191f2c0-…` is the one thing an admin cannot recognise.
        subjectId: 'vm-store',
        title: 'Target vm-store: the portal address moved',
        detail: 'portal 10.10.0.5 no longer exists on storage0',
        raisedAt: '2026-09-01 10:00:00',
        ackedAt: null,
        resolvedAt: null,
      }],
    },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const row = root.querySelector('#nas-ov-alerts .alert-row');
  // The button is named after the SURFACE it lands on, the way the disk alert's
  // is ("Dyski"). "Szczegóły" is this app's word for a detail screen, and this
  // one goes to a list.
  assert.equal(row.querySelector('[data-goto]').textContent.trim(), 'Udostępnianie');
  // …and the subject kind is translated, not the raw enum the wire carries.
  assert.match(row.querySelector('.a-sub').textContent, /target vm-store/);
  click(row.querySelector('[data-goto]'));
  await flush();
  assert.equal(Screen.tab, 'shares');
  // The target's name travels with the navigation, so the Sharing tab can put
  // the admin on the thing the alert was about instead of at the top of a
  // table of twenty.
  assert.equal(Screen.targetName, 'vm-store');
  Screen.unmount();
});

test('a disk alert drills down to that disk', async () => {
  stubTransport({
    ...fixtures,
    tentaNasAlertsListRequest: { alerts: [{ alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'nvme0n1', title: 'nvme0n1', detail: '', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null }] },
    tentaNasDiskGetRequest: { disk: disk({ diskId: 'nvme0n1', name: 'nvme0n1' }), attributes: [], selfTests: [], history: [], historyDays: 7 },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  click(root.querySelector('#nas-ov-alerts [data-goto]'));
  await flush();
  assert.equal(Screen.tab, 'disks');
  assert.equal(Screen.diskId, 'nvme0n1');
  Screen.unmount();
});

test('an alert subline prints a subject name but never a machine id', async () => {
  // The node passes a NAME as `subjectId` for most kinds, and the row prints it
  // ("target vm-store"). A disk alert carries `wwn-<hex>` instead and an
  // approval carries the request UUID — neither says anything the title has
  // not said, and the mockups show no identifier in an alert row at all. The
  // value stays reachable as the tooltip.
  stubTransport({
    ...fixtures,
    tentaNasAlertsListRequest: {
      alerts: [
        { alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'wwn-5000cca27dc7a4c6', title: 'Disk sdg: warning', detail: '1 UDMA CRC errors', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
        { alertId: 'a2', severity: 'warning', subjectKind: 'approval', subjectId: '0191f2c0-4b1e-7c3a-9f2d-8ac41b5e9d70', title: 'Operacja czeka na drugiego admina', detail: 'pool tank', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
        { alertId: 'a3', severity: 'warning', subjectKind: 'elastic-array', subjectId: 'produkt', title: 'Macierz oczekuje na przywrócenie', detail: 'wymagane jawne Przywróć', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
      ],
    },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const rows = [...root.querySelectorAll('#nas-ov-alerts .alert-row')];
  assert.equal(rows.length, 3);

  const disk = rows[0].querySelector('.a-sub');
  assert.doesNotMatch(disk.textContent, /wwn-/);
  assert.equal(disk.getAttribute('title'), 'wwn-5000cca27dc7a4c6');

  assert.doesNotMatch(rows[1].querySelector('.a-sub').textContent, /0191f2c0/);

  // …and a real name still shows, next to the translated kind.
  assert.match(rows[2].querySelector('.a-sub').textContent, /produkt/);
  Screen.unmount();
});

test('the fleet node table names a node instead of printing its node id', async () => {
  // n16 prints `atlas` and `orion`. The 64-hex node id is not a name; it is the
  // tooltip, for the one case two nodes answer to the same hostname.
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL });
  await flush();
  Screen.switchTab('environment');
  await flush();
  await flush();
  const others = root.querySelector('#nas-others-table').rows;
  assert.equal(others.length, 2);
  for (const row of others) {
    assert.match(row.name, new RegExp(row._node.nodeName), 'the node is named');
    assert.doesNotMatch(row.name, /class="l2 mono"/, 'no id sub-line');
    assert.match(row.name, new RegExp(`title="${row._node.nodeId}"`), 'the id is the tooltip');
  }
  Screen.unmount();
});

test('overview feeds every poll into the live throughput and temperature charts', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const io = root.querySelector('#nas-ov-io');
  const temp = root.querySelector('#nas-ov-temp');
  assert.ok(io && temp, 'both stream charts mounted');
  const readLine = io.querySelector('polyline[data-series-id="read"]');
  assert.equal(readLine.getAttribute('points').trim().split(' ').length, 1, 'first poll = one sample');
  // Two disks at 1 MiB/s each.
  assert.match(root.querySelector('#nas-ov-io-val').textContent, /2\.0 MB\/s/);
  assert.match(root.querySelector('#nas-ov-temp-val').textContent, /34°C/);
  assert.match(root.querySelectorAll('.live-label')[0].textContent, /na żywo · okno 60 s/);
  assert.match(root.querySelectorAll('.live-label')[1].textContent, /okno 30 min/);
  // A second poll appends a sample instead of rebuilding the chart.
  await Screen.refreshOverview(root.querySelector('#nas-tab-body'));
  await flush();
  assert.strictEqual(io.querySelector('polyline[data-series-id="read"]'), readLine, 'polyline reused');
  assert.equal(readLine.getAttribute('points').trim().split(' ').length, 2);
  Screen.unmount();
});

// --- "nigdy pełne odświeżenie całości" (research/03-ui-wzorce-mockupy.md) ----
// A poll replaces DATA, never the elements that carry it. Node identity is the
// assertion: a rebuilt <tf-alert> or stat card is a blink on screen.
//
// Identity is compared as a BOOLEAN for the reason `absent` above documents —
// a failing `assert.strictEqual` over two happy-dom elements sends node's
// util.inspect through the whole document graph (~2 min, after which the FILE
// is reported failed with no test name).
const same = (a, b) => a === b;

test('a poll that brings the same telemetry keeps the very same tf-alert element', async () => {
  const telemetry = {
    sampledAt: '2026-09-02 10:00:00', smartReadAt: '2026-09-02 09:59:00', smartState: 'partial',
    detail: '/dev/sdb: smartctl failed (4): no output',
  };
  stubTransport({ ...fixtures, tentaNasDisksListRequest: { ...fixtures.tentaNasDisksListRequest, telemetry } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const host = root.querySelector('#nas-ov-telemetry');
  const alert = host.querySelector('tf-alert');
  assert.ok(alert, 'the banner is on screen');
  const message = alert.querySelector('.tf-alert-message');
  assert.equal(message.textContent, '/dev/sdb: smartctl failed (4): no output');

  await Screen.refreshOverview(root.querySelector('#nas-tab-body'));
  await flush();
  assert.equal(same(host.querySelector('tf-alert'), alert), true, 'the same element survives the poll');
  assert.equal(same(host.querySelector('.tf-alert-message'), message), true, 'and its insides are not re-rendered either');
  Screen.unmount();
});

test('the disks tab keeps its telemetry banner, and its action, across a poll', async () => {
  const telemetry = { sampledAt: '2026-09-02 10:00:00', smartReadAt: '2026-09-02 09:59:00', smartState: 'unarmed', detail: '' };
  stubTransport({ ...fixtures, tentaNasDisksListRequest: { ...fixtures.tentaNasDisksListRequest, telemetry } });
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  await flush();
  const host = root.querySelector('#nas-disks-telemetry');
  const alert = host.querySelector('tf-alert');
  const btn = alert.querySelector('tf-button');
  assert.ok(btn, 'an admin gets the arming action');

  await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
  await flush();
  assert.equal(same(host.querySelector('tf-alert'), alert), true, 'the same element');
  assert.equal(same(host.querySelector('tf-button'), btn), true, 'the same button, so a click target never moves under the cursor');
  Screen.unmount();
});

// tf-table recycles its <tr> elements but rebuilt the ACTIONS cell on every
// render: `_writeActionsCell` called the builder unconditionally and did
// `td.replaceChildren(el)`. Every row here carries four tf-buttons, so a 5 s
// poll that moved nothing but temperature destroyed and recreated all of them —
// measured on the live page at 464 structural mutations on this table in 21 s,
// with 440 tf-buttons created. The user-visible half is a click target that
// vanishes mid-gesture.
//
// Object identity cannot guard this: `diskRow` builds a new `_disk` object from
// fresh API data every poll. drawDisks declares a `rowActionsKey` signature
// instead, covering every field the buttons render or their handlers read.
test('a disk poll that changes nothing does not rebuild the row action buttons', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  await flush();
  const tbody = root.querySelector('#nas-disk-table').shadowRoot.querySelector('tbody');
  const actions = tbody.querySelector('.tf-table__actions-cell').firstElementChild;
  assert.ok(actions, 'the disks table renders a row-actions element');
  assert.equal(actions.querySelectorAll('tf-button').length, 4, 'four buttons on a free disk');

  // Counted where the defect lives. The `role` column is renderer="chip", and
  // `_writeCell` rebuilds a chip span unconditionally — a separate, pre-existing
  // churn that this test deliberately does not pin.
  // Records are COLLECTED IN THE CALLBACK, not read with takeRecords() at the
  // end: the awaits below let happy-dom deliver and drain the queue first, so a
  // trailing takeRecords() reads an empty list and the count passes vacuously
  // even while every button is being recreated.
  const records = [];
  const obs = new window.MutationObserver((recs) => { records.push(...recs); });
  obs.observe(tbody, { childList: true, subtree: true });
  let added = 0;
  let removed = 0;
  try {
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();
    records.push(...obs.takeRecords());
    for (const rec of records) {
      if (!rec.target.closest || !rec.target.closest('.tf-table__actions-cell')) continue;
      added += rec.addedNodes.length;
      removed += rec.removedNodes.length;
    }
  } finally {
    obs.disconnect();
  }
  assert.equal(added, 0, `an unchanged poll must add no node to an actions cell, added ${added}`);
  assert.equal(removed, 0, `and remove none, removed ${removed}`);
  assert.equal(
    same(tbody.querySelector('.tf-table__actions-cell').firstElementChild, actions),
    true,
    'the very same actions element, so a click target never moves under the cursor',
  );
  Screen.unmount();
});

// The correctness half of the same guard: when the signature DOES move, the
// element is rebuilt and its handlers close over the disk the latest poll
// described — never the one they were originally built with.
test('a disk action rebuilt by a poll acts on the disk as it is NOW', async () => {
  let role = 'free';
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: () => ({ ...fixtures.tentaNasDisksListRequest, disks: [disk({ role })] }),
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  await flush();
  const tbody = root.querySelector('#nas-disk-table').shadowRoot.querySelector('tbody');
  const actsOf = () => [...tbody.querySelectorAll('.tf-table__actions-cell tf-button')].map((b) => b.dataset.act);
  assert.deepEqual(actsOf(), ['locate', 'smart', 'use', 'details'], 'a free disk offers "use in pool"');

  const seen = [];
  const realLocate = Screen.locateDisk;
  Screen.locateDisk = (d, enable) => { seen.push(`${d.diskId}:${d.role}:${enable}`); };
  try {
    role = 'data';
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();
    assert.deepEqual(actsOf(), ['locate', 'smart', 'details'], 'the pool action goes once the disk is in use');
    click(tbody.querySelector('[data-act="locate"]'));
    assert.deepEqual(seen, ['sda:data:true'], 'the handler carries the role from the LATEST poll');
  } finally {
    Screen.locateDisk = realLocate;
    Screen.unmount();
  }
});

// The clearing action, offered exactly where an admin meets the problem: a
// disk whose role is the catch-all `used` carries a filesystem signature,
// belongs to no pool and to no array this node records, and can therefore go
// into neither — which is the state every disk of a dissolved Elastic Array
// is left in.
//
// The handler reads the LIVE row for the same reason the locate handler does,
// and it matters more here: a kept actions cell survives a sort, a filter and
// a poll, and this is the one action that erases a device.
test('a used disk offers the clearing action, and it acts on the disk as it is NOW', async () => {
  let role = 'used';
  let diskId = 'sda';
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: () => ({
      ...fixtures.tentaNasDisksListRequest,
      disks: [disk({ role, diskId, fsType: 'xfs' })],
    }),
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  await flush();
  const tbody = root.querySelector('#nas-disk-table').shadowRoot.querySelector('tbody');
  const actsOf = () => [...tbody.querySelectorAll('.tf-table__actions-cell tf-button')].map((b) => b.dataset.act);
  assert.deepEqual(actsOf(), ['locate', 'smart', 'wipe', 'details'], 'a used disk offers "clear disk"');

  const seen = [];
  const real = Screen.wipeDisk;
  Screen.wipeDisk = (d) => { seen.push(`${d.diskId}:${d.role}`); };
  try {
    diskId = 'sdz';
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();
    click(tbody.querySelector('[data-act="wipe"]'));
    assert.deepEqual(seen, ['sdz:used'], 'the handler carries the disk from the LATEST poll');

    // A free disk is offered the pool wizard instead: there is nothing on it
    // to clear, and a destructive action with nothing to destroy is noise.
    role = 'free';
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();
    assert.deepEqual(actsOf(), ['locate', 'smart', 'use', 'details']);

    // And a disk with a real owner keeps neither: the action on it is on its
    // pool or its array, not on the device.
    role = 'pool_member';
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();
    assert.deepEqual(actsOf(), ['locate', 'smart', 'details']);
  } finally {
    Screen.wipeDisk = real;
    Screen.unmount();
  }
});

test('the KPI tiles are the same elements after a poll, with only their numbers moved', async () => {
  let readBps = 1048576;
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: () => ({
      ...fixtures.tentaNasDisksListRequest,
      disks: [disk({ io: { readBps, writeBps: 0, readIops: 10, writeIops: 0, awaitMs: 2.5, utilPct: 3 } })],
    }),
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const tiles = [...root.querySelectorAll('#nas-ov-kpi tf-stat-card')];
  assert.equal(tiles.length, 4);
  assert.equal(tiles[3].getAttribute('value'), '1.0');

  readBps = 4 * 1048576;
  await Screen.refreshOverview(root.querySelector('#nas-tab-body'));
  await flush();
  const after = [...root.querySelectorAll('#nas-ov-kpi tf-stat-card')];
  assert.equal(after.length, 4);
  after.forEach((el, i) => assert.equal(same(el, tiles[i]), true, `tile ${i} is the same element`));
  assert.equal(after[3].getAttribute('value'), '4.0', 'the throughput moved without the tile being rebuilt');
  assert.match(after[3].querySelector('.tf-stat-card-value').textContent, /^4\.0/, 'and the component rendered it');
  // A tile whose numbers did not move is not touched at all: the poll writes
  // no attribute, so the component does not re-render its insides either.
  const labelEl = tiles[0].querySelector('.tf-stat-card-label');
  await Screen.refreshOverview(root.querySelector('#nas-tab-body'));
  await flush();
  assert.equal(same(tiles[0].querySelector('.tf-stat-card-label'), labelEl), true, 'an unchanged tile is left alone');
  Screen.unmount();
});

test('the fleet view is patched by its poll, never redrawn', async () => {
  stubTransport(fixtures);
  const root = await mountScreen();
  await flush();
  await flush();
  const grid = root.querySelector('#nas-node-grid');
  const cards = [...root.querySelectorAll('.kpi tf-stat-card')];
  const alertsTable = root.querySelector('#nas-fleet-alerts');
  assert.equal(cards.length, 4);

  await Screen.refreshFleet();
  await flush();
  assert.equal(same(root.querySelector('#nas-node-grid'), grid), true, 'the node grid is not rebuilt');
  assert.equal(same(root.querySelector('#nas-fleet-alerts'), alertsTable), true, 'nor the alert table');
  [...root.querySelectorAll('.kpi tf-stat-card')].forEach((el, i) => assert.equal(same(el, cards[i]), true, `fleet tile ${i} survives`));
  Screen.unmount();
});

// fleet.rs counts warnings and failures apart. A node that had LOST a disk
// used to arrive here inside `disksWarning`, so the fleet said "warning"
// about a node whose `health` on the same row already said 'critical'.
test('a dead disk reads as a failure on the fleet chip, the health tile and its drill-down', async () => {
  stubTransport({ ...fixtures, tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [
    node({ health: 'warning', disksWarning: 2 }),
    node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, health: 'critical', disksWarning: 0, disksCritical: 1, features: [] }),
  ] } });
  const root = await mountScreen();
  await flush();
  const chip = root.querySelector('#nas-fleet-chips tf-chip');
  assert.equal(chip.getAttribute('status'), 'err', 'a failure is not amber');
  assert.equal(chip.getAttribute('label'), '1 awaria (vega)');

  const health = [...root.querySelectorAll('.kpi tf-stat-card')][1];
  assert.equal(health.getAttribute('value'), '1', 'the failure count leads, not the two warnings');
  assert.equal(health.getAttribute('suffix'), 'awaria');
  assert.equal(health.getAttribute('accent'), 'danger');
  assert.match(health.getAttribute('delta'), /vega/);
  assert.match(health.getAttribute('delta'), /orion/, 'the warned node stays reachable in the delta');

  click(health);
  await flush();
  assert.equal(Screen.nodeId, REMOTE, 'the tile opens the node that failed, not the one that warns');
  assert.equal(Screen.diskFilter, 'problems');
  Screen.unmount();
});

test('a fleet with warnings only keeps the warning wording', async () => {
  stubTransport({ ...fixtures, tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [
    node({ health: 'warning', disksWarning: 2 }),
  ] } });
  const root = await mountScreen();
  await flush();
  const chip = root.querySelector('#nas-fleet-chips tf-chip');
  assert.equal(chip.getAttribute('status'), 'warn');
  assert.match(chip.getAttribute('label'), /^2 ostrzeżenia/);
  const health = [...root.querySelectorAll('.kpi tf-stat-card')][1];
  assert.equal(health.getAttribute('value'), '2');
  assert.equal(health.getAttribute('accent'), 'warning');
  Screen.unmount();
});

// `poolsTotal` is ZFS pools alone (fleet.rs), so a node whose only storage is
// an Elastic Array reported zero pools and read as a share client.
test('a node whose only storage is an Elastic Array is a fleet NAS and says how many arrays it has', async () => {
  stubTransport({ ...fixtures, tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [
    node({ poolsTotal: 0, arraysTotal: 2, capacityBytes: 12e12, usedBytes: 3e12 }),
  ] } });
  const root = await mountScreen();
  await flush();
  const card = root.querySelector('.node-card');
  assert.match(card.querySelector('.nc-foot').lastElementChild.textContent, /NAS floty/);
  const stats = [...card.querySelectorAll('.nc-stats .kv-inline')];
  const arrays = stats.find((kv) => kv.querySelector('.k').textContent === 'Elastic Array');
  assert.ok(arrays, `the array count is a stat of its own: ${stats.map((kv) => kv.querySelector('.k').textContent)}`);
  assert.equal(arrays.querySelector('.v').textContent, '2');
  assert.equal(stats.find((kv) => kv.querySelector('.k').textContent === 'Pule').querySelector('.v').textContent, '0',
    'and it is not folded into the pool count');

  const badges = [...root.querySelectorAll('#nas-fleet-badges tf-chip')].map((c) => c.getAttribute('label'));
  assert.match(badges[0], /1× NAS: orion/, 'an array-only node counts as a NAS of the fleet');
  assert.match(badges[2], /^2 pule · 11 TiB/, 'the capacity badge counts the arrays it is the capacity of');
  Screen.unmount();
});

// An array the node could not measure is in NEITHER byte figure (fleet.rs),
// so the total is knowingly short of its disks and both the tile and the
// card's fill bar have to say so instead of looking complete.
test('an unmeasured array is named beside the capacity, not folded into it', async () => {
  stubTransport({ ...fixtures, tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [
    node({ poolsTotal: 1, arraysTotal: 2, arraysUnmeasured: 1 }),
  ] } });
  const root = await mountScreen();
  await flush();
  const capacity = [...root.querySelectorAll('.kpi tf-stat-card')][0];
  assert.match(capacity.getAttribute('delta'), /1 bez pomiaru pojemności/);
  assert.match(root.querySelector('.node-card .split-bar').getAttribute('title'), /1 bez pomiaru pojemności/);
  Screen.unmount();
});

test('a fleet that measured every array says nothing about unmeasured ones', async () => {
  stubTransport({ ...fixtures, tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [
    node({ poolsTotal: 1, arraysTotal: 1, arraysUnmeasured: 0 }),
  ] } });
  const root = await mountScreen();
  await flush();
  const capacity = [...root.querySelectorAll('.kpi tf-stat-card')][0];
  assert.ok(!/bez pomiaru/.test(capacity.getAttribute('delta')), capacity.getAttribute('delta'));
  assert.equal(root.querySelector('.node-card .split-bar').getAttribute('title'), '25%');
  Screen.unmount();
});

test('the node header chip calls a dead disk a failure, not a warning', async () => {
  stubTransport({ ...fixtures, tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [
    node({ health: 'critical', disksCritical: 1, disksWarning: 0 }),
  ] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const chip = root.querySelector('#nas-head-chips tf-chip');
  assert.equal(chip.getAttribute('status'), 'err');
  assert.equal(chip.getAttribute('label'), '1 awaria');
  Screen.unmount();
});

// The grid used to be compared as ONE joined string, so any single node's
// uptime or used-bytes ticking rebuilt every card on the fleet every 10 s —
// the largest blink surface left in TentaNas. Each card is now compared
// against its own previous markup, keyed by node id.
test('one node moving rebuilds only that node card, not the whole grid', async () => {
  const vega = () => node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, poolsTotal: 0, features: [] });
  const mini = () => node({ nodeId: MAC, nodeName: 'mini', isLocal: false, instanceStatus: 'unsupported', osName: 'macOS', disksTotal: 0, poolsTotal: 0, features: [] });
  let nodes = [node({}), vega(), mini()];
  stubTransport({ ...fixtures, tentaNasNodesListRequest: () => ({ localNodeId: LOCAL, nodes }) });
  const root = await mountScreen();
  await flush();
  await flush();
  const cards = [...root.querySelectorAll('.node-card')];
  assert.equal(cards.length, 3);

  // Only orion's uptime ticks; the other two carry identical values.
  nodes = [node({ uptimeSecs: 7200 }), vega(), mini()];
  await Screen.refreshFleet();
  await flush();
  const after = [...root.querySelectorAll('.node-card')];
  assert.equal(after.length, 3);
  assert.match(after[0].querySelector('.nc-sub').textContent, /uptime/, 'orion now reports an uptime');
  assert.equal(same(after[0], cards[0]), false, 'the node whose value moved is rebuilt');
  assert.equal(same(after[1], cards[1]), true, 'the node that did not move keeps its element');
  assert.equal(same(after[2], cards[2]), true, 'and so does the third');
  Screen.unmount();
});

test('a node leaving and rejoining the fleet leaves the other cards standing', async () => {
  const vega = () => node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, poolsTotal: 0, features: [] });
  const mini = () => node({ nodeId: MAC, nodeName: 'mini', isLocal: false, instanceStatus: 'unsupported', osName: 'macOS', disksTotal: 0, poolsTotal: 0, features: [] });
  let nodes = [node({}), vega()];
  stubTransport({ ...fixtures, tentaNasNodesListRequest: () => ({ localNodeId: LOCAL, nodes }) });
  const root = await mountScreen();
  await flush();
  await flush();
  const orion = root.querySelectorAll('.node-card')[0];
  assert.ok(orion, 'the local node has a card');

  // vega drops out of the fleet: its card has to go with it.
  nodes = [node({})];
  await Screen.refreshFleet();
  await flush();
  let grid = [...root.querySelectorAll('.node-card')];
  assert.equal(grid.length, 1, 'the stale card is removed, not left behind');
  assert.equal(same(grid[0], orion), true, 'the surviving node keeps its element');

  // …and rejoins, with a third node after it.
  nodes = [node({}), vega(), mini()];
  await Screen.refreshFleet();
  await flush();
  grid = [...root.querySelectorAll('.node-card')];
  assert.equal(grid.length, 3, 'no duplicate card for the node that rejoined');
  assert.equal(same(grid[0], orion), true, 'orion is the same element throughout');
  assert.deepEqual(grid.map((c) => c.dataset.node), [LOCAL, REMOTE, MAC], 'and the grid is in fleet order');
  Screen.unmount();
});

// `partial` is what the wire sends when SMART was read for most disks and
// failed for the ones `detail` names (disks.rs:1145). Titling that "SMART
// niedostępny" overstates it; an unknown state still deserves the blunt title.
test('a partial SMART read says so, and an unknown state keeps the generic title', async () => {
  const partial = {
    sampledAt: '2026-09-02 10:00:00', smartReadAt: '2026-09-02 09:59:00',
    smartState: 'partial', detail: '/dev/sdb: smartctl failed (4): no output',
  };
  stubTransport({ ...fixtures, tentaNasDisksListRequest: { ...fixtures.tentaNasDisksListRequest, telemetry: partial } });
  let root = await mountScreen({ node: LOCAL });
  await flush();
  const alert = root.querySelector('#nas-ov-telemetry tf-alert');
  assert.equal(alert === null, false, 'the banner is on screen');
  assert.equal(alert.getAttribute('title'), 'SMART częściowo niedostępny');
  Screen.unmount();

  // A state this screen has not been taught keeps the blunt wording rather than
  // silently claiming the failure is partial.
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      ...fixtures.tentaNasDisksListRequest,
      telemetry: { ...partial, smartState: 'something-the-wire-grew-later' },
    },
  });
  root = await mountScreen({ node: LOCAL });
  await flush();
  assert.equal(root.querySelector('#nas-ov-telemetry tf-alert').getAttribute('title'), 'SMART niedostępny');
  Screen.unmount();
});

test('the unavailable banner offers an install only when a package is genuinely missing', async () => {
  const telemetry = { sampledAt: '2026-09-02 10:00:00', smartReadAt: null, smartState: 'partial', detail: '/dev/sdb: smartctl failed (4): no output' };
  // Nothing is missing here: smartctl is installed and merely failed on some
  // disks. Offering "Doinstaluj" would name a cause that does not exist.
  stubTransport({ ...fixtures, tentaNasDisksListRequest: { ...fixtures.tentaNasDisksListRequest, telemetry } });
  let root = await mountScreen({ node: LOCAL });
  await flush();
  assert.ok(root.querySelector('#nas-ov-telemetry tf-alert'), 'the banner is there');
  assert.equal(absent(root, '#nas-ov-telemetry tf-button'), true, 'but no install action over a working smartctl');
  Screen.unmount();

  // The node's own probe reports the package absent → the banner routes to the
  // very install flow the Environment tab uses.
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: { ...fixtures.tentaNasDisksListRequest, telemetry },
    tentaNasEnvironmentRequest: {
      environment: {
        ...environment,
        features: [
          ...environment.features,
          { id: 'smartmontools', status: 'missing_package', version: null, requiredVersion: null, binaries: ['smartctl'], kernelModule: null, packages: ['smartmontools'], detail: '', optional: false },
        ],
      },
    },
  });
  root = await mountScreen({ node: LOCAL });
  await flush();
  const btn = root.querySelector('#nas-ov-telemetry tf-button');
  assert.ok(btn, 'the install action is offered');
  assert.equal(btn.textContent, 'Doinstaluj (sudo)…', 'and it reuses the Environment tab wording');
  click(btn);
  await flush();
  const win = document.querySelector('tf-window');
  assert.ok(win, 'the install confirmation opens');
  assert.match(win.textContent, /smartmontools/, 'and it names the package');
  win.remove();
  Screen.unmount();
});

test('disk detail draws the history charts over the window the backend reports', async () => {
  const sample = (at, temperatureC, readBps, reallocatedSectors) => ({ at, temperatureC, reallocatedSectors, pendingSectors: 0, readBps, writeBps: 0, awaitMs: 1 });
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: {
      disk: disk({}),
      attributes: [],
      selfTests: [],
      history: [sample('2026-09-02 08:00:00', 33, 1e6, 0), sample('2026-09-02 09:00:00', 35, 2e6, null), sample('2026-09-02 10:00:00', 34, 5e5, null)],
      alerts: [],
      historyDays: 30,
    },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sda' });
  await flush();
  await flush();
  assert.match(root.querySelector('#nas-disk-temp-chart').previousElementSibling.textContent, /Temperatura — 30 dni/);
  assert.ok(root.querySelector('#nas-disk-temp-chart tf-line-chart'), 'temperature chart mounted');
  assert.equal(absent(root, '#nas-disk-io-chart'), true, 'the extra 24 h transfer chart is gone');
  assert.equal(root.querySelector('#nas-disk-temp-chart polyline.tf-chart__series-line').getAttribute('points').trim().split(' ').length, 3, 'three temperature samples plotted');
  // One reallocation sample only → no chart, the empty note instead.
  assert.equal(absent(root, '#nas-disk-realloc-chart tf-line-chart'), true);
  assert.ok(root.querySelector('#nas-disk-realloc-chart .muted'));
  assert.match(root.querySelector('.id-badge').textContent, /hdd/);
  assert.ok(root.querySelector('[data-act="copy-serial"]'), 'the serial can be copied');
  Screen.unmount();
});

test('a pooled disk shows the vdev error counters and opens the replace wizard', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: {
      disk: disk({ diskId: 'sdd', name: 'sdd', serial: 'ZR9AB12K', role: 'pool', memberOf: 'tank', health: 'warning', healthReason: '3 nowe realokowane sektory w 7 dni', vdevRole: 'data', vdevKind: 'raidz2' }),
      attributes: [], selfTests: [], history: [], alerts: [], historyDays: 30,
    },
    tentaNasPoolGetRequest: { pool, properties: [], datasets: [], alerts: [], history: [] },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sdd' });
  await flush();
  await flush();
  assert.equal(kinds('tentaNasPoolGetRequest')[0].payload.name, 'tank');
  // n04:175 — the identification chip carries the status AND its reason.
  assert.equal(root.querySelector('.section-card-head tf-chip').getAttribute('label'), 'Uwaga: 3 nowe realokowane sektory w 7 dni');
  assert.match(root.textContent, /Dlaczego status „Uwaga”\?/);
  const roleField = [...root.querySelectorAll('.id-fields .f')].find((f) => f.querySelector('.k').textContent === 'Rola');
  assert.equal(roleField.querySelector('.v').textContent, 'tank · RAIDZ2');
  const card = [...root.querySelectorAll('.section-card')].find((c) => /Błędy z warstwy puli \(tank\)/.test(c.textContent));
  assert.ok(card, 'the pool error block is rendered');
  const rows = [...card.querySelectorAll('.sr')].map((r) => r.textContent.replace(/\s+/g, ' ').trim());
  assert.equal(rows[0], 'READ1');
  assert.equal(rows[1], 'WRITE0');
  assert.equal(rows[2], 'CKSUM2');
  assert.match(rows[3], /^Stan w vdev/);
  assert.match(rows[4], /Ostatni scrub.*· 0 błędów/);
  assert.match(card.textContent, /zanim SMART cokolwiek pokaże/);
  assert.equal(card.querySelector('[data-act="open-pool"]').textContent, 'Zobacz pulę tank');

  const replace = root.querySelector('[data-act="replace"]');
  assert.equal(replace.textContent, 'Wymień dysk…');
  assert.equal(replace.getAttribute('variant'), 'danger');
  click(replace);
  await flush();
  await flush();
  const win = document.querySelector('tf-window.nas-modal');
  assert.equal(windowTitle(win), 'Wymień dysk sdd (tank · RAIDZ2)');
  win.remove();
  Screen.unmount();
});

test('the disk-detail breadcrumb walks back to the disk list and to the fleet', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: { disk: disk({}), attributes: [], selfTests: [], history: [], alerts: [], historyDays: 30 },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sda' });
  await flush();
  await flush();
  // The header crumb says "TentaNas › orion"; the tab body adds "Dyski › sda",
  // the same tail shape the pool detail uses.
  const crumbs = [...root.querySelectorAll('.nas-crumbs')];
  assert.equal(crumbs.length, 2, 'header crumb plus the tab-local tail');
  assert.deepEqual([...crumbs[0].querySelectorAll('.tf-breadcrumb-item')].map((a) => a.textContent), ['TentaNas', 'orion']);
  const tail = [...crumbs[1].querySelectorAll('.tf-breadcrumb-item')];
  assert.deepEqual(tail.map((a) => a.textContent), ['Dyski', 'sda']);
  assert.equal(crumbs[1].querySelector('a.tf-breadcrumb-item').getAttribute('href'), `#/tentanas?node=${LOCAL}&tab=disks`);

  click(crumbs[1].querySelector('a.tf-breadcrumb-item'));
  await flush();
  assert.equal(Screen.diskId, null, 'the "Dyski" crumb returns to the disk list');

  click(crumbs[0].querySelector('a.tf-breadcrumb-item'));
  await flush();
  assert.equal(Screen.nodeId, null, 'the "TentaNas" crumb returns to the fleet');
  Screen.unmount();
});

// --- n04 refreshes itself, and refreshes the way this screen refreshes -------
// The disk detail is the screen the admin watches SMART and temperature on, so
// it polls like every other live view: `drawDiskDetail` builds it once and
// `refreshDiskDetail` patches the values in and re-arms itself. A redraw on a
// timer would be the opposite of a fix — every node on the screen destroyed and
// rebuilt every five seconds, the charts restarted and the scroll thrown away.
//
// `sdd` is the subject of all four: it is IN a pool, so the pool's own error
// counters are part of what the poll has to keep fresh.
const detailState = {
  health: 'warning',
  healthReason: '3 nowe realokowane sektory w 7 dni',
  reallocated: 3,
  cksum: 2,
  history: [
    { at: '2026-09-02 08:00:00', temperatureC: 33, reallocatedSectors: 3 },
    { at: '2026-09-02 09:00:00', temperatureC: 35, reallocatedSectors: 3 },
    { at: '2026-09-02 10:00:00', temperatureC: 34, reallocatedSectors: 3 },
  ],
  diskFails: false,
  poolFails: false,
};

function diskDetailFixtures() {
  return {
    ...fixtures,
    tentaNasDiskGetRequest: () => {
      if (detailState.diskFails) throw new Error('node nie odpowiada');
      return {
        disk: disk({
          diskId: 'sdd', name: 'sdd', serial: 'ZR9AB12K', role: 'pool', memberOf: 'tank',
          health: detailState.health, healthReason: detailState.healthReason,
          reallocatedSectors: detailState.reallocated, vdevRole: 'data', vdevKind: 'raidz2',
        }),
        attributes: [{ id: 5, name: 'Reallocated_Sector_Ct', value: 98, raw: detailState.reallocated, rawText: String(detailState.reallocated), rawWeekAgo: 0, status: 'warning' }],
        selfTests: [{ startedAt: '2026-09-01 01:00:00', kind: 'Short offline', status: 'passed', detail: '', lifetimeHours: 99 }],
        history: detailState.history,
        alerts: [],
        historyDays: 30,
      };
    },
    tentaNasPoolGetRequest: () => {
      if (detailState.poolFails) throw new Error('zpool status failed');
      const vdev = pool.vdevs[0];
      return {
        pool: {
          ...pool,
          vdevs: [{ ...vdev, disks: vdev.disks.map((x) => (x.name === 'sdd' ? { ...x, cksumErrors: detailState.cksum } : x)) }],
        },
        properties: [], datasets: [], alerts: [], history: [],
      };
    },
  };
}

async function openDiskDetail() {
  detailState.health = 'warning';
  detailState.healthReason = '3 nowe realokowane sektory w 7 dni';
  detailState.reallocated = 3;
  detailState.cksum = 2;
  detailState.history = [
    { at: '2026-09-02 08:00:00', temperatureC: 33, reallocatedSectors: 3 },
    { at: '2026-09-02 09:00:00', temperatureC: 35, reallocatedSectors: 3 },
    { at: '2026-09-02 10:00:00', temperatureC: 34, reallocatedSectors: 3 },
  ];
  detailState.diskFails = false;
  detailState.poolFails = false;
  stubTransport(diskDetailFixtures());
  const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sdd' });
  await flush();
  await flush();
  return root;
}

test('a disk-detail poll that brings the same disk replaces no node of the screen', async () => {
  const root = await openDiskDetail();
  const body = root.querySelector('#nas-tab-body');
  const chip = root.querySelector('#nas-dd-health');
  const grid = root.querySelector('.id-grid');
  const reallocated = root.querySelector('[data-f="reallocated"]');
  const rows = root.querySelector('#nas-dd-pool .stat-rows');
  const cksum = root.querySelector('#nas-dd-pool [data-c="cksum"]');
  const attrTable = root.querySelector('#nas-attr-table');
  const stTable = root.querySelector('#nas-st-table');
  const chart = root.querySelector('#nas-disk-temp-chart tf-line-chart');
  assert.ok(chip && reallocated && rows && attrTable && stTable && chart, 'the screen is fully drawn before the poll');
  assert.equal(chip.getAttribute('label'), 'Uwaga: 3 nowe realokowane sektory w 7 dni');
  assert.equal(cksum.textContent, '2');

  // Records are COLLECTED IN THE CALLBACK for the reason the fleet tests
  // document: the awaits below drain happy-dom's queue, so a trailing
  // takeRecords() would read an empty list and pass vacuously.
  Screen.clearTimers();
  const records = [];
  const obs = new window.MutationObserver((recs) => { records.push(...recs); });
  obs.observe(grid, { childList: true, subtree: true, attributes: true, characterData: true });
  try {
    await Screen.refreshDiskDetail(body);
    await flush();
    records.push(...obs.takeRecords());
  } finally {
    obs.disconnect();
  }
  assert.equal(records.length, 0, `an unchanged poll must touch no node of the identification card, saw ${records.map((r) => `${r.type}@${r.target.nodeName}`).join(', ')}`);
  assert.equal(same(root.querySelector('#nas-dd-health'), chip), true, 'the health chip is the very same element');
  assert.equal(same(root.querySelector('.id-grid'), grid), true, 'so is the identification grid');
  assert.equal(same(root.querySelector('[data-f="reallocated"]'), reallocated), true, 'and the counter cell inside it');
  assert.equal(same(root.querySelector('#nas-dd-pool .stat-rows'), rows), true, 'the pool error rows are not rebuilt');
  assert.equal(same(root.querySelector('#nas-attr-table'), attrTable), true, 'nor the SMART attribute table');
  assert.equal(same(root.querySelector('#nas-st-table'), stTable), true, 'nor the self-test log');
  // The history charts are a server-sent series, not a live stream: the same
  // series must leave the chart element alone, or its line restarts its draw
  // animation on every tick.
  assert.equal(same(root.querySelector('#nas-disk-temp-chart tf-line-chart'), chart), true, 'the temperature chart keeps its element');
  assert.equal(Screen.timers.size, 1, 'and the poll re-armed itself — without this the screen never refreshes again');
  Screen.unmount();
});

test('a disk-detail poll moves the values that changed and leaves their surroundings standing', async () => {
  const root = await openDiskDetail();
  const body = root.querySelector('#nas-tab-body');
  const chip = root.querySelector('#nas-dd-health');
  const grid = root.querySelector('.id-grid');
  const reallocated = root.querySelector('[data-f="reallocated"]');
  const rows = root.querySelector('#nas-dd-pool .stat-rows');
  const scrub = root.querySelector('#nas-dd-pool [data-c="scrub"]');
  const chartHost = root.querySelector('#nas-disk-temp-chart');
  const why = root.querySelector('#nas-dd-why');
  assert.equal(reallocated.textContent, '3');
  assert.equal(root.querySelector('#nas-disk-temp-chart polyline.tf-chart__series-line').getAttribute('points').trim().split(' ').length, 3);

  detailState.health = 'critical';
  detailState.healthReason = '8 realokowanych sektorów, rośnie';
  detailState.reallocated = 8;
  detailState.cksum = 5;
  detailState.history = [...detailState.history, { at: '2026-09-02 11:00:00', temperatureC: 41, reallocatedSectors: 8 }];
  await Screen.refreshDiskDetail(body);
  await flush();

  assert.equal(chip.getAttribute('label'), 'Awaria: 8 realokowanych sektorów, rośnie', 'the health chip carries the new status and reason');
  assert.equal(chip.getAttribute('status'), 'err');
  assert.equal(chip.textContent, 'Awaria: 8 realokowanych sektorów, rośnie', 'and the component rendered it');
  assert.equal(root.querySelector('[data-f="reallocated"]').textContent, '8', 'the reallocated counter moved');
  assert.equal(root.querySelector('#nas-dd-pool [data-c="cksum"]').textContent, '5', 'and so did the pool checksum counter');
  assert.equal(root.querySelector('#nas-dd-pool [data-c="cksum"]').getAttribute('class'), 'v num-err');
  assert.equal(why.textContent, '8 realokowanych sektorów, rośnie', 'the explanation box follows the reason');
  // The surroundings survive: same chip, same grid, same counter cell, same
  // rows — only their contents differ.
  assert.equal(same(root.querySelector('#nas-dd-health'), chip), true, 'the chip was patched, not replaced');
  assert.equal(same(root.querySelector('.id-grid'), grid), true, 'the identification grid stands');
  assert.equal(same(root.querySelector('[data-f="reallocated"]'), reallocated), true, 'the counter cell stands');
  assert.equal(same(root.querySelector('#nas-dd-pool .stat-rows'), rows), true, 'a moving counter does not rebuild the error rows');
  assert.equal(same(root.querySelector('#nas-dd-pool [data-c="scrub"]'), scrub), true, 'nor the scrub line next to it');
  assert.equal(same(root.querySelector('#nas-disk-temp-chart'), chartHost), true, 'the chart card is not rebuilt');
  assert.equal(root.querySelector('#nas-disk-temp-chart polyline.tf-chart__series-line').getAttribute('points').trim().split(' ').length, 4, 'the new sample is plotted');
  Screen.unmount();
});

test('a failed disk-detail poll keeps the last good screen and keeps asking', async () => {
  const root = await openDiskDetail();
  const body = root.querySelector('#nas-tab-body');
  const chip = root.querySelector('#nas-dd-health');

  detailState.diskFails = true;
  Screen.clearTimers();
  await Screen.refreshDiskDetail(body);
  await flush();
  assert.match(root.querySelector('#nas-dd-error tf-alert').getAttribute('message'), /node nie odpowiada/, 'the failure is stated');
  assert.equal(chip.getAttribute('label'), 'Uwaga: 3 nowe realokowane sektory w 7 dni', 'the last good health stays on screen');
  assert.equal(root.querySelector('[data-f="reallocated"]').textContent, '3', 'and the last good counter');
  assert.equal(root.querySelector('#nas-dd-pool [data-c="cksum"]').textContent, '2', 'and the last good pool counters');
  assert.ok(root.querySelector('#nas-attr-table'), 'the SMART table is not blanked');
  assert.equal(Screen.timers.size, 1, 'and the screen keeps asking');

  // The pool read is the secondary one: a node that answers about the disk but
  // not about its pool must not be read as "this disk is in no pool" — that
  // would announce a lost membership nothing has reported.
  detailState.diskFails = false;
  detailState.poolFails = true;
  Screen.clearTimers();
  await Screen.refreshDiskDetail(body);
  await flush();
  assert.equal(absent(root, '#nas-dd-error tf-alert'), true, 'the answered poll retracts the banner');
  assert.equal(root.querySelector('#nas-dd-pool [data-c="cksum"]').textContent, '2', 'the last good pool counters stand');
  assert.match(root.querySelector('#nas-dd-pool-title').textContent, /tank/);
  assert.equal(Screen.timers.size, 1, 'and it is still asking');
  Screen.unmount();
});

test('leaving the disk detail stops its poll', async () => {
  const root = await openDiskDetail();
  const body = root.querySelector('#nas-tab-body');
  const detached = document.createElement('div');
  detached.innerHTML = body.innerHTML;

  // A body that is no longer in the document: nothing to patch, nothing to
  // re-arm — and no request either, because the guard runs before the read.
  const beforeDetached = kinds('tentaNasDiskGetRequest').length;
  Screen.clearTimers();
  await Screen.refreshDiskDetail(detached);
  await flush();
  assert.equal(kinds('tentaNasDiskGetRequest').length, beforeDetached, 'a disconnected body asks nothing');
  assert.equal(Screen.timers.size, 0, 'and arms nothing');

  Screen.unmount();
  const beforeDisposed = kinds('tentaNasDiskGetRequest').length;
  await Screen.refreshDiskDetail(body);
  await flush();
  assert.equal(kinds('tentaNasDiskGetRequest').length, beforeDisposed, 'an unmounted screen asks nothing');
  assert.equal(Screen.timers.size, 0, 'and the polling chain is over');
});

test('the environment tab carries the ksmbd row with the kernel version and the EXPERIMENTAL note', async () => {
  // n16 gains one probe row per §5.4b. It reports the KERNEL version, because
  // ksmbd is in-tree: ksmbd-tools' own version says nothing about the server
  // that actually serves.
  const ksmbd = {
    id: 'ksmbd', status: 'ok', version: '6.12.4-arch1-1', requiredVersion: null,
    binaries: ['ksmbd.mountd', 'ksmbd.control', 'ksmbd.adduser'], kernelModule: 'ksmbd', packages: ['ksmbd-tools'],
    detail: 'enp1s0f0np0 10.10.0.5 · EXPERIMENTAL (kernel docs) · ksmbd loaded', optional: true,
  };
  // The probe row arrives the way the node sends it. It used to be injected by
  // assigning `Screen.environment` and re-mounting, which only worked while the
  // tab body raced the header probe and won.
  stubTransport({
    ...fixtures,
    tentaNasEnvironmentRequest: { environment: { ...environment, features: [...environment.features, ksmbd] } },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'environment' });
  await flush();
  await flush();

  const rows = root.querySelector('#nas-feature-table').rows;
  const row = rows.find((r) => r._feature.id === 'ksmbd');
  assert.ok(row, 'the ksmbd probe row is listed');
  assert.match(row.name, /ksmbd \(SMB Direct przez RDMA\)/);
  assert.match(row.name, /mod:ksmbd/);
  assert.match(row.version, /6\.12\.4-arch1-1/, 'the kernel release is the version of this backend');
  assert.match(row.version, /EXPERIMENTAL \(kernel docs\)/);
  assert.equal(row.status.status, 'ok');
});

test('a ksmbd row refused by the exposure guard reads as a warning, not as a missing package', async () => {
  const exposed = {
    id: 'ksmbd', status: 'exposed', version: '6.12.4-arch1-1', requiredVersion: null,
    binaries: ['ksmbd.mountd', 'ksmbd.control', 'ksmbd.adduser'], kernelModule: 'ksmbd', packages: ['ksmbd-tools'],
    detail: 'enp3s0 192.168.1.20 also carries the default gateway — SMB Direct needs a dedicated storage network · EXPERIMENTAL (kernel docs)',
    optional: true,
  };
  stubTransport({
    ...fixtures,
    tentaNasEnvironmentRequest: { environment: { ...environment, features: [...environment.features, exposed] } },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'environment' });
  await flush();
  await flush();

  const row = root.querySelector('#nas-feature-table').rows.find((r) => r._feature.id === 'ksmbd');
  // Installing a package would not change a routing table, so this must not
  // read like an optional feature nobody got round to installing.
  assert.equal(row.status.status, 'warn');
  assert.equal(row.status.label, 'interfejs z bramą domyślną');
  assert.match(row.version, /default gateway/);
});

// -----------------------------------------------------------------------------
// The forced setup step (n16)
//
// WHY these exist: an instance installed through the catalog came up with
// `elevation.mode: "unset"` — the spelling the wire uses — while every
// comparison in this screen tested `"unarmed"`. The panel therefore read a
// node that could not run a single privileged command as a working one: it
// painted a dashboard, the header badge said ok, and `withSudo` decided no
// password was needed. The first Elastic click then failed on the server.
// -----------------------------------------------------------------------------

const unconfigured = (over = {}) => ({
  ...environment,
  elevation: { ...environment.elevation, mode: 'unset', helperState: 'absent', coreCompatible: false, ...over },
});

test('a node whose privilege channel is not configured opens on the setup step instead of a dashboard', async () => {
  stubTransport({ ...fixtures, tentaNasEnvironmentRequest: { environment: unconfigured() } });
  const root = await mountScreen({ node: LOCAL, tab: 'overview' });
  await flush();

  assert.ok(root.querySelector('#nas-setup'), 'the setup step is what the panel shows');
  assert.equal(absent(root, '#nas-ov-kpi'), true, 'no dashboard rendered behind it');
  assert.match(root.querySelector('tf-alert').getAttribute('message'), /Tryb kanału nie został jeszcze wybrany/);
  // n16 promises the admin sees exactly what mode A would run.
  assert.match(root.querySelector('#nas-setup-plan').textContent, /install -m 0755 \/opt\/tentaflow\/tentanas-helper/);
  assert.ok(root.querySelector('[data-act="setup-mode-a"]'), 'mode A offered');
  assert.ok(root.querySelector('[data-act="setup-mode-b"]'), 'mode B offered');
  // The dashboard stays gated on every tab, not just the one that was asked for.
  Screen.tab = 'pools';
  Screen.drawTab();
  await flush();
  assert.ok(root.querySelector('#nas-setup'), 'the pools tab is gated too');
  assert.equal(absent(root, '#nas-pools-list'), true);
  Screen.unmount();
});

test('cancelling the channel wizard leaves the not-configured state and arms nothing', async () => {
  stubTransport({ ...fixtures, tentaNasEnvironmentRequest: { environment: unconfigured() } });
  const root = await mountScreen({ node: LOCAL, tab: 'overview' });
  await flush();

  click(root.querySelector('[data-act="setup-mode-b"]'));
  await flush();
  const win = document.querySelector('tf-window.nas-modal');
  assert.ok(win, 'the existing channel wizard opened — no second password dialog');
  click(win.querySelector('[data-wizard-cancel]'));
  await flush();

  assert.equal(kinds('tentaNasElevationArmRequest').length, 0, 'a cancelled wizard arms nothing');
  assert.equal(kinds('tentaNasElevationProvisionRequest').length, 0, 'and provisions nothing');
  assert.ok(root.querySelector('#nas-setup'), 'the panel stays on the honest not-configured state');
  assert.equal(absent(root, '#nas-ov-kpi'), true, 'and never falls through to a dashboard');
  win.remove();
  Screen.unmount();
});

test('mode A is not offered when the helper binary is missing next to the core', async () => {
  // Measured on the installed node: the core looks for `tentanas-helper` beside
  // its own binary and it was not there, so provisioning would die on its first
  // command.
  const plan = { ...fixtures.tentaNasElevationPlanRequest.plan, helperSourcePresent: false };
  stubTransport({
    ...fixtures,
    tentaNasEnvironmentRequest: { environment: unconfigured() },
    tentaNasElevationPlanRequest: { plan },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'overview' });
  await flush();

  assert.equal(absent(root, '[data-act="setup-mode-a"]'), true, 'no button that cannot work');
  const blocked = root.querySelector('#nas-setup-mode-a-blocked').textContent;
  assert.match(blocked, /brak binarium helpera obok core/, 'it says specifically what is missing');
  assert.match(blocked, /\/opt\/tentaflow\/tentanas-helper/, 'and where it was looked for');
  assert.ok(root.querySelector('[data-act="setup-mode-b"]'), 'mode B stays available');
  Screen.unmount();
});

// -----------------------------------------------------------------------------
// Straight after the install (the owner's actual requirement), the viewer's
// state, and the two states around the environment probe
// -----------------------------------------------------------------------------

test('completing the wizard retires the forced post-install step instead of leaving a stale alert', async () => {
  // `forceSetup` was cleared by nothing but the dismiss button, and the
  // wizard's post-run redraw was conditional on the environment tab — while
  // the post-install route lands on `overview`. An admin who typed the sudo
  // password correctly was still shown "channel not configured", at exactly
  // the moment the feature is meant to prove it worked.
  //
  // The environment answers `unset` until the channel is armed and healthy
  // afterwards, because the step must retire on the REFRESHED state, not on
  // the wizard reporting success: a wizard can succeed while the channel
  // still fails `channelUnusable()`, and then the step has to stay.
  let envCalls = 0;
  stubTransport({
    ...fixtures,
    tentaNasEnvironmentRequest: () => {
      envCalls += 1;
      return { environment: envCalls === 1 ? unconfigured() : environment };
    },
    tentaNasElevationArmRequest: {
      elevation: { ...environment.elevation, mode: 'interactive', armedUntil: '2026-09-13 12:00:00' },
    },
  });
  const root = await mountScreen({ setup: '1' });
  try {
    assert.equal(Screen.forceSetup, true, 'the install forced the step');
    assert.ok(root.querySelector('#nas-setup'), 'and the step is what is on screen');

    click(root.querySelector('[data-act="setup-mode-b"]'));
    await flush();
    const win = document.querySelector('tf-window.nas-modal');
    assert.ok(win, 'the existing channel wizard opened — no second password dialog');

    click(win.querySelector('[data-wizard-next]'));
    await flush();
    const pass = win.querySelector('#nas-wz-pass');
    assert.ok(pass, 'the wizard asks for the sudo password');
    // The field binds through `input`; assigning .value alone would leave
    // `state.password` empty and send a blank secret, passing for the wrong
    // reason.
    pass.value = 'sudo-secret';
    pass.dispatchEvent(new window.Event('input', { bubbles: true }));

    click(win.querySelector('[data-wizard-next]'));
    for (let i = 0; i < 6; i += 1) await flush();

    const armed = kinds('tentaNasElevationArmRequest');
    assert.equal(armed.length, 1, 'the wizard armed the channel');
    assert.equal(armed[0].payload.sudoPassword, 'sudo-secret', 'carrying the password that was typed');

    assert.equal(Screen.forceSetup, false, 'the forced step retires once the channel reads as usable');
    assert.ok(root.querySelector('#nas-ov-kpi'), 'and the dashboard is what the admin now sees');
    assert.equal(absent(root, '#nas-setup'), true, 'not the stale not-configured panel');
  } finally {
    document.querySelector('tf-window.nas-modal')?.remove();
    Screen.unmount();
  }
});

test('the post-install route forces the setup step on this node even when the channel already works', async () => {
  // The requirement is a forced elevation step IMMEDIATELY AFTER INSTALL, not
  // a gate that waits for somebody to open an unconfigured node later. The
  // install path routes here with `setup=1`; these fixtures describe a node
  // whose helper channel is perfectly healthy, so only the forcing can put
  // the step on screen.
  stubTransport(fixtures);
  const root = await mountScreen({ setup: '1' });
  // `finally`, because the screen arms polling timers on mount: a failing
  // assertion that skipped `unmount` would leave them running and the whole
  // FILE would hang to the runner's timeout instead of reporting this test.
  try {
    assert.ok(root.querySelector('#nas-setup'), 'the step is forced right after the install');
    assert.equal(absent(root, '#nas-ov-kpi'), true, 'and not the dashboard');
    // The route named no node: the screen opened the one the admin is on.
    assert.equal(Screen.nodeId, LOCAL);
    // Install is fleet-wide, the channel is per node — the step has to say so
    // rather than let the admin believe the fleet is now armed.
    const scope = root.querySelector('#nas-setup-scope').textContent;
    assert.match(scope, /orion/, 'it names the node being configured');
    assert.match(scope, /tylko dla niego/, 'and says it configures only that node');
    assert.match(scope, /osobno na każdym węźle/, 'and that every other node is still its own job');
    // A healthy channel must not be described as a broken one.
    assert.match(root.querySelector('tf-alert').getAttribute('message'), /jest już skonfigurowany/);
    // The route itself collects no secret: the wizard does, after the install.
    assert.equal(kinds('tentaNasElevationArmRequest').length, 0);
    assert.equal(kinds('tentaNasElevationProvisionRequest').length, 0);
  } finally {
    Screen.unmount();
  }
});

test('the forced step can be dismissed, and an unconfigured node still reads as unconfigured', async () => {
  stubTransport({ ...fixtures, tentaNasEnvironmentRequest: { environment: unconfigured() } });
  const root = await mountScreen({ setup: '1' });
  try {
    assert.ok(root.querySelector('[data-act="setup-dismiss"]'), 'mode B is a deliberate downgrade, so dismissing is possible');

    click(root.querySelector('[data-act="setup-dismiss"]'));
    await flush();
    assert.equal(Screen.forceSetup, false, 'the post-install forcing is dropped');
    assert.ok(root.querySelector('#nas-setup'), 'but a node with no channel is still held on the step');
    assert.equal(absent(root, '#nas-ov-kpi'), true, 'and never falls through to a dashboard');
  } finally {
    Screen.unmount();
  }
});

test('dismissing the forced step on a configured node returns to the dashboard', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ setup: '1' });
  try {
    click(root.querySelector('[data-act="setup-dismiss"]'));
    await flush();
    assert.equal(absent(root, '#nas-setup'), true, 'nothing traps a working node on the step');
    assert.ok(root.querySelector('#nas-ov-kpi'), 'a configured node shows its dashboard');
  } finally {
    Screen.unmount();
  }
});

test('a viewer is told what is wrong and who must act, not left on a spinner that never resolves', async () => {
  // The plan box rendered "Pobieranie planu…" unconditionally while the fetch
  // behind it was admin-only, so a non-admin sat under a heading promising
  // commands that were never going to arrive.
  stubTransport({
    ...fixtures,
    authMeRequest: { role: 'user' },
    tentaNasEnvironmentRequest: { environment: unconfigured() },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'overview' });
  try {
    assert.ok(root.querySelector('#nas-setup'), 'the viewer gets the honest state too');
    assert.equal(absent(root, '#nas-setup-plan'), true, 'and no plan box that will never fill');
    assert.equal(kinds('tentaNasElevationPlanRequest').length, 0, 'nor a request they are not allowed to make');
    const viewer = root.querySelector('#nas-setup-viewer').textContent;
    assert.match(viewer, /nie jest skonfigurowany/, 'it says what is wrong');
    assert.match(viewer, /administrator/i, 'and who has to act');
    assert.equal(absent(root, '[data-act="setup-mode-a"]'), true, 'no action a viewer cannot take');
    assert.equal(absent(root, '[data-act="setup-mode-b"]'), true);
  } finally {
    Screen.unmount();
  }
});

test('a failed environment probe shows the failure and a retry, never a tile-by-tile dashboard', async () => {
  // The catch only toasted: `environment` stayed undefined, `channelUnusable()`
  // answered false, and the whole dashboard rendered anyway — every tile then
  // failing separately against the same silent node.
  let attempt = 0;
  stubTransport({
    ...fixtures,
    tentaNasEnvironmentRequest: () => {
      attempt += 1;
      if (attempt === 1) throw new Error('probe timed out');
      return { environment };
    },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'overview' });
  try {
    assert.ok(root.querySelector('#nas-probe-failed'), 'the failure is stated');
    assert.equal(absent(root, '#nas-ov-kpi'), true, 'no dashboard over a node that cannot answer');
    assert.equal(absent(root, '#nas-setup'), true, '"unknown" is not reported as "not configured"');
    assert.match(root.querySelector('tf-alert').getAttribute('message'), /probe timed out/, 'and it names the failure');

    click(root.querySelector('[data-act="probe-retry"]'));
    await flush();
    await flush();
    assert.equal(absent(root, '#nas-probe-failed'), true, 'the retry clears the error state');
    assert.ok(root.querySelector('#nas-ov-kpi'), 'and a node that answers gets its dashboard');
  } finally {
    Screen.unmount();
  }
});

// The disk bar is repainted by every 5 s poll (applyDiskRows → paintDiskFilters)
// because the counts live in the chip labels. The pool selector next to it was
// already guarded by `diskPoolSig`; the chips were not, so the button under the
// cursor was destroyed and recreated twelve times a minute.
test('the disk filter chips survive a poll, so a click never lands on a detached button', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  await flush();
  const bar = root.querySelector('#nas-disk-filters');
  const buttons = [...bar.querySelectorAll('.tf-filter-chip')];
  assert.equal(buttons.length, 5, 'all / hdd / flash / problems / free');
  assert.match(buttons[3].textContent, /\d/, 'the label carries the live count');

  await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
  await flush();
  const after = [...bar.querySelectorAll('.tf-filter-chip')];
  assert.equal(after.length, 5);
  after.forEach((el, i) => assert.equal(el === buttons[i], true, `filter ${i} is the same element after the poll`));
  Screen.unmount();
});

// One transient failure used to do two things at once: replace the whole body
// with an alert (destroying the charts and their accumulated history) and
// return without re-arming the timer, so the overview never refreshed again
// until the user changed tabs.
test('a failed overview poll patches the error in and keeps the loop alive', async () => {
  let failing = false;
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: () => {
      if (failing) throw new Error('node busy');
      return fixtures.tentaNasDisksListRequest;
    },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'overview' });
  await flush();
  const body = root.querySelector('#nas-tab-body');
  const kpi = root.querySelector('#nas-ov-kpi');
  const chart = root.querySelector('#nas-ov-io');
  assert.equal(root.querySelectorAll('#nas-ov-kpi tf-stat-card').length, 4);

  const scheduled = [];
  const realLater = Screen.later;
  Screen.later = (fn, ms) => { scheduled.push(ms); };
  try {
    failing = true;
    await Screen.refreshOverview(body);
    await flush();
    assert.equal(root.querySelector('#nas-ov-kpi') === kpi, true, 'the dashboard is not thrown away');
    assert.equal(root.querySelector('#nas-ov-io') === chart, true, 'and the live chart keeps its history');
    assert.match(root.querySelector('#nas-ov-error tf-alert').getAttribute('message'), /node busy/, 'the failure is stated');
    assert.deepEqual(scheduled, [5000], 'the poll loop re-armed itself');

    // And it recovers on its own: the next poll retracts the banner.
    failing = false;
    scheduled.length = 0;
    await Screen.refreshOverview(body);
    await flush();
    assert.equal(absent(root, '#nas-ov-error tf-alert'), true, 'the banner is retracted');
    assert.equal(root.querySelector('#nas-ov-kpi') === kpi, true, 'over the very same tiles');
  } finally {
    Screen.later = realLater;
    Screen.unmount();
  }
});

// `set rows` and `set rowActions` each call the table's _render(). Painting
// both on every poll therefore ran the whole table twice per 10 s tick; the
// actions are a function OF THE ROW, so they belong in drawFleet, once.
test('a fleet poll runs one render pass per table, not two', async () => {
  stubTransport(fixtures);
  const root = await mountScreen();
  await flush();
  await flush();
  const table = root.querySelector('#nas-fleet-alerts');
  assert.equal(typeof table.rowActions, 'function', 'row actions are wired at draw time');
  let renders = 0;
  const real = table._render;
  table._render = function counted() { renders += 1; return real.call(this); };
  try {
    await Screen.refreshFleet();
    await flush();
    assert.equal(renders, 1, `one render pass per poll, got ${renders}`);
  } finally {
    table._render = real;
    Screen.unmount();
  }
});

// `setAttribute` with an IDENTICAL value still runs attributeChangedCallback,
// and tf-chip._update() does `span.textContent = ''` and rebuilds the insides —
// the chip's text node does not survive. Two raw setAttribute calls sat two
// lines under already-converted code, so this counter was thrown away and
// rebuilt every 5 s for a number that had not moved.
test('the overview alert counter is not rewritten when the count did not change', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL, tab: 'overview' });
  await flush();
  const body = root.querySelector('#nas-tab-body');
  const chip = root.querySelector('#nas-ov-alerts-count');
  const text = chip.querySelector('span').firstChild;
  assert.equal(text.nodeType, 3, 'the count is a text node inside the chip span');
  assert.equal(chip.getAttribute('label'), '0', 'no alerts in the fixtures');
  try {
    await Screen.refreshOverview(body);
    await flush();
    assert.equal(chip.querySelector('span').firstChild === text, true, 'the very same text node after a 5 s poll');

    // A count that really moves still lands: the guard skips the write, not
    // the update.
    const raised = { alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'sda', title: 'sda', detail: '', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null };
    stubTransport({ ...fixtures, tentaNasAlertsListRequest: { alerts: [raised] } });
    await Screen.refreshOverview(body);
    await flush();
    assert.equal(chip.getAttribute('label'), '1', 'a changed count is written');
    assert.equal(chip.getAttribute('status'), 'err', 'and so is the status it carries');
  } finally {
    Screen.unmount();
  }
});

// Same mechanism one tab strip over: TfTab.observedAttributes includes `count`,
// and its attributeChangedCallback runs _update() → `this._btn.innerHTML = …`
// unconditionally. setJobsBadge is called from refreshOverview, so the tab's
// label and count pill were destroyed and recreated twelve times a minute.
test('the jobs tab badge is not rewritten when the running count did not change', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL, tab: 'overview' });
  await flush();
  const body = root.querySelector('#nas-tab-body');
  const tab = root.querySelector('#nas-tabs tf-tab#jobs');
  assert.equal(tab.getAttribute('count'), '1', 'one running job in the fixtures');
  const label = tab.querySelector('.tf-tab-label');
  const pill = tab.querySelector('.tf-tab-count');
  assert.equal(label === null, false, 'the tab has a label span');
  assert.equal(pill === null, false, 'and a count pill');
  try {
    await Screen.refreshOverview(body);
    await flush();
    assert.equal(tab.querySelector('.tf-tab-label') === label, true, 'the tab label survived the 5 s poll');
    assert.equal(tab.querySelector('.tf-tab-count') === pill, true, 'and so did the count pill');

    // A count that really moves still lands.
    stubTransport({ ...fixtures, tentaNasJobsListRequest: { jobs: [] } });
    await Screen.refreshOverview(body);
    await flush();
    assert.equal(tab.hasAttribute('count'), false, 'nothing running, no badge');
  } finally {
    Screen.unmount();
  }
});

// paintSmartBulkButton runs on every disks poll (applyDiskRows) and used to
// write both the label and `disabled` unconditionally. `textContent =` replaces
// the text node, and an identical setAttribute still fires
// attributeChangedCallback, so tf-button rebuilt its insides every 5 s —
// measured live as the toolbar label under 6 node identities in 21 s.
test('the bulk SMART button is not rewritten when the selection did not change', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  await flush();
  const body = root.querySelector('#nas-tab-body');
  const btn = root.querySelector('[data-act="smart-bulk"]');
  assert.equal(btn === null, false, 'the toolbar has the bulk button');
  const inner = btn.querySelector('button');
  assert.equal(inner === null, false, 'tf-button rendered its insides');
  try {
    await Screen.refreshDisks(body);
    await flush();
    assert.equal(btn.querySelector('button') === inner, true, 'the same element survived the 5 s poll');
    assert.equal(btn.hasAttribute('disabled'), true, 'nothing selected, so still disabled');

    // A selection that really changes still lands — without this half the test
    // would also pass if the button stopped updating altogether.
    Screen.diskSelection.add('any-disk-id');
    Screen.paintSmartBulkButton();
    await flush();
    assert.equal(btn.hasAttribute('disabled'), false, 'one selected disk enables it');
    assert.match(btn.textContent, /1/, 'and the count reaches the label');
  } finally {
    Screen.diskSelection.clear();
    Screen.unmount();
  }
});

// The disks table and the fleet card both call a failing disk "Awaria", while
// this tile folded critical into the warning count and announced it in warning
// tone — so the screen the admin lands on understated a dying disk, and two
// screens contradicted each other about the same device.
test('a failing disk makes the health tile a failure, not a warning', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      ...fixtures.tentaNasDisksListRequest,
      disks: [
        disk({ health: 'critical', healthReason: '835 media errors' }),
        disk({ diskId: 'sdz', name: 'sdz', path: '/dev/sdz', health: 'warning', healthReason: '51°C' }),
      ],
    },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  try {
    const tile = root.querySelector('#nas-ov-kpi [data-kpi="disks"]');
    assert.equal(tile === null, false, 'the disks tile is on screen');
    assert.equal(tile.getAttribute('accent'), 'danger', 'a failure is not painted as a warning');
    assert.equal(tile.getAttribute('value'), '1', 'the failure count leads the tile');
    assert.match(tile.getAttribute('suffix'), /awari/i, 'and it is named a failure');
    // Failures come first in the detail line, so the dying disk is the one read.
    assert.match(tile.getAttribute('delta'), /^[^·]*835 media errors/);
  } finally {
    Screen.unmount();
  }
});

// The other half: without this, a change that painted every non-ok state red
// would pass, and one wrong tone would simply replace the other.
test('with warnings only, the health tile stays a warning', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      ...fixtures.tentaNasDisksListRequest,
      disks: [disk({ health: 'warning', healthReason: '51°C' })],
    },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  try {
    const tile = root.querySelector('#nas-ov-kpi [data-kpi="disks"]');
    assert.equal(tile.getAttribute('accent'), 'warning', 'a warning is still a warning');
    assert.match(tile.getAttribute('suffix'), /ostrzeż/i);
  } finally {
    Screen.unmount();
  }
});

// n02's "Tiering i cache zapisu". Every figure it shows was already measured
// and already on the wire; `cacheUnprotectedBytes` in particular is the
// canonical "18 GiB na cache bez parity" that the spec asks for in several
// places and that no screen had ever read.
const tieredArray = (overrides) => elasticArray({
  cacheSizeBytes: 1 * TIB, cacheUsedBytes: 0.25 * TIB,
  protection: { status: 'window_open', cacheUnprotectedBytes: 18 * 1024 ** 3 },
  mover: { history: [{ startedAt: '2026-09-15 14:00:00', outcome: 'ok', movedBytes: 42 * 1024 ** 3, movedFiles: 118 }] },
  ...overrides,
});

test('the tiering card names both tiers, what waits on the cache unprotected, and the last move', async () => {
  stubTransport({ ...fixtures, tentaNasElasticArraysListRequest: { arrays: [tieredArray()] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();

  const card = root.querySelector('#nas-ov-tier-card');
  assert.equal(card.hidden, false, 'a node with a cache tier has tiering to show');
  assert.equal(root.querySelector('#nas-ov-arc-row').getAttribute('data-single'), null,
    'the row is two columns when both cards are there');

  const block = root.querySelector('#nas-ov-tier .tier-block[data-array="produkt"]');
  assert.ok(block, 'one block per tiered array');
  const text = block.textContent;
  assert.match(text, /Cache \(szybka warstwa\)/);
  assert.match(text, /Dyski danych/);
  assert.match(text, /18(\.0)? GiB/, 'the bytes waiting on the cache are finally on a screen');
  assert.match(text, /Na cache, jeszcze bez ochrony/);
  assert.match(text, /Ostatnie przeniesienie/);
  // The recorded runs live in `mover.history`; a card that looked elsewhere
  // said "no runs" beside an array that had moved 42 GiB.
  assert.match(text, /118 plików/, 'the last move carries its file count');
  assert.doesNotMatch(text, /brak zapisanych przebiegów/);
  assert.match(text, /automatycznie przenoszone/, 'it says the files move by themselves');
  assert.match(text, /nie są chronione parzystością/, 'and why those bytes are at risk until then');
  assert.doesNotMatch(text, /mover/i, 'no process name an admin has to know');
  // Pending bytes are the normal state of a cache, not a warning.
  const waitingRow = [...block.querySelectorAll('.sr')].find((r) => /jeszcze bez ochrony/.test(r.textContent));
  assert.equal(waitingRow.querySelector('.v').classList.contains('num-warn'), false);
  assert.ok(block.querySelector('.split-bar'), 'both sides measured, so the bar is drawn');
  Screen.unmount();
});

test('a node with no cache tier hides the tiering card and gives ARC the whole row', async () => {
  stubTransport({ ...fixtures, tentaNasElasticArraysListRequest: { arrays: [elasticArray()] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  assert.equal(root.querySelector('#nas-ov-tier-card').hidden, true,
    'an array without a cache disk has no tiering to describe');
  assert.equal(root.querySelector('#nas-ov-arc-row').getAttribute('data-single'), '1');
  assert.equal(root.querySelector('#nas-ov-tier .tier-block'), null);
  Screen.unmount();
});

test('the tiering bar is omitted when only one side was measured', async () => {
  stubTransport({ ...fixtures,
    tentaNasElasticArraysListRequest: { arrays: [tieredArray({ cacheUsedBytes: null })] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const block = root.querySelector('#nas-ov-tier .tier-block');
  assert.ok(block, 'the rows are still there');
  assert.equal(block.querySelector('.split-bar'), null,
    'half a split drawn as a bar would read as a measurement');
  Screen.unmount();
});
