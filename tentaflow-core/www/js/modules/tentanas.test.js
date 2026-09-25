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
import { existsSync, readFileSync, readdirSync } from 'node:fs';

const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { default: Screen } = await import('./tentanas.js');
// The shared job-row implementation (BLOCKER 2, n01-n10 critic 2026-09-21):
// tentanas.js imports these from tasks.js rather than keeping its own
// `jobRowHtml`/`wireJobRows` copy, so the tests build rows the same way.
const { jobRowSkeleton, paintJobRow } = await import('./tentanas/tasks.js');

// Renders one job row through the shared skeleton + painter, the way both
// tentanas.js (n02) and tasks.js (n15) do it, and returns the row element.
function buildJobRow(container, j) {
  container.insertAdjacentHTML('beforeend', jobRowSkeleton(j));
  const row = container.lastElementChild;
  paintJobRow(row, j);
  return row;
}

const LOCAL = 'nodeaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const REMOTE = 'nodebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
const MAC = 'nodeccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc';
// One health reason as the node sends it (`NasHealthReason`): a code and its
// parameters, numbers in decimal strings.
function R(code, params = {}) {
  return { code, params };
}

// `perOrgCounted` follows `isLocal` unless a test sets it: the list handler
// counts the asking organisation's figures on the local row only.
function node(overrides) {
  const n = {
    nodeId: LOCAL, nodeName: 'orion', isLocal: true, online: true, instanceStatus: 'ready', health: 'ok',
    osName: 'Debian 12', zfsVersion: '2.2.4', elevationMode: 'helper', disksTotal: 2, disksWarning: 0,
    poolsTotal: 1, sharesTotal: 1, alertsActive: 0, capacityBytes: 4e12, usedBytes: 1e12, updatedAt: '2026-09-02 10:00:00',
    features: ['OpenZFS 2.2.4', 'SMB'],
    ...overrides,
  };
  if (!('perOrgCounted' in overrides)) n.perOrgCounted = n.isLocal;
  return n;
}

function disk(overrides) {
  return {
    diskId: 'sda', name: 'sda', path: '/dev/sda', kind: 'hdd', model: 'WD Red', serial: 'WD-1', wwn: null, sizeBytes: 2e12,
    transport: 'sata', rotational: true, removable: false, firmware: null, role: 'free', memberOf: null, health: 'ok', healthReason: '', healthReasons: [],
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
  name: 'tank', guid: '1', kind: 'zfs', state: 'online', health: 'ok', healthReason: '', healthReasons: [],
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
    disks: [disk({}), disk({ diskId: 'nvme0n1', name: 'nvme0n1', path: '/dev/nvme0n1', kind: 'nvme', model: 'Samsung 980', serial: 'S-1', health: 'warning', healthReason: '2 pending sectors', healthReasons: [R('pending_sectors', { count: '2' })], wearPct: 12, rotational: false })],
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
  // The array pane names its place through the shell's breadcrumb
  // (`screen.setCrumbTail`), like the pool detail — not a crumb of its own.
  click([...root.querySelectorAll('#nas-crumbs a.tf-breadcrumb-item')].find((a) => a.textContent.trim() === 'Pule'));
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
  jobs.forEach((job) => buildJobRow(host, job));
  assert.equal(host.querySelectorAll('[data-act="cancel"]').length, 1);
  assert.equal(host.querySelectorAll('[data-act="log"]').length, 3);
  assert.match(host.textContent, /Tworzenie Elastic Array/);
  assert.match(host.textContent, /Przywracanie montowania Elastic Array/);
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
  assert.equal(subs[0], 'Debian 12 · OpenZFS 2.2.4 · 128 GiB RAM · uptime 41 d 4 h · ten node');
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
  assert.match(sub, /^flota · 3 nody · 2 wspierane/);
  assert.match(sub, /TentaNas 1\.4\.0/);
  assert.match(sub, /ostatnie odświeżenie/);
  const badges = [...head.querySelectorAll('.d-badges tf-chip')].map((c) => c.getAttribute('label'));
  assert.match(badges[0], /1× NAS: orion/);
  assert.match(badges[1], /Kanały uprawnień: 1× tryb A · 1× nieuzbrojony/);
  assert.match(badges[3], /mesh: 3 nody/);
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
      return { alerts: [{
        alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'sda', title: 'Disk sda: warning', detail: '3 reallocated sectors',
        code: 'disk_health', params: { health: 'warning', name: 'sda', name_source: 'live' }, reasons: [{ code: 'reallocated', params: { count: '3' } }],
        raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null,
      }] };
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
  // Worded from the codes the way the mockup writes it (n01:298, "sdd: 3 nowe
  // realokowane sektory w 7 dni"): the disk's name and its reason as a
  // sentence, never the n03 chip abbreviation; the node's English is the
  // cell's tooltip only.
  assert.match(alerts[0].alert, /<div class="l1">sda: 3 realokowane sektory<\/div><div class="l2"><\/div>/);
  assert.match(alerts[0].alert, /title="Disk sda: warning — 3 reallocated sectors"/);
  assert.equal(alerts[0].alert.replace(/title="[^"]*"/g, '').includes('reallocated'), false, 'no English outside the tooltip');
  // n01 spells the level as an alert severity, never as the disk-health word.
  assert.match(alerts[0].level, /label="ostrzeżenie"/);
  assert.ok(!/Uwaga/.test(alerts[0].level), 'the disk-health wording stays on n03/n04');
  assert.match(alerts[1].level, /offline/);
  assert.match(alerts[1].alert, /mesh timeout/);

  const res = root.querySelector('#nas-fleet-res-table').rows;
  assert.equal(res.length, 2, 'one share plus the unreachable node');
  assert.match(res[0].resource, /projekty/);
  assert.match(res[0].mounts, /mount-dots/);
  // The dot's tooltip is the node and the state in words, never the code.
  assert.match(res[0].mounts, /title="orion: źródło"/);
  assert.doesNotMatch(res[0].mounts, /: source"/);
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

// n03's own bulk SMART used to abort on the first refusal of ANY kind, which
// made it behave differently from the "SMART all disks" schedule action in
// tasks.js (MINOR 5 of the round-1 review). Both now share `runDiskBatch`
// (format.js): a disk-specific refusal does not stop the batch.
test('the bulk SMART button continues past a disk-specific refusal in the middle', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      disks: [disk({}), disk({ diskId: 'sdb', name: 'sdb' }), disk({ diskId: 'sdc', name: 'sdc' })],
      telemetry: fixtures.tentaNasDisksListRequest.telemetry,
    },
    tentaNasDiskSmartTestRequest: (payload) => (payload.diskId === 'sdb'
      ? Promise.reject(new Error('dysk zajęty'))
      : Promise.resolve({ job: { jobId: `j-${payload.diskId}`, kind: 'smart_test', subject: payload.diskId, status: 'queued', log: [] } })),
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  const table = root.querySelector('#nas-disk-table');
  for (const row of table.rows) {
    table.dispatchEvent(new window.CustomEvent('row-select', { detail: { row, index: 0, selected: true } }));
  }
  // Every toast, recorded at the append: utils.js keeps its container in a
  // module variable that an earlier test's teardown may have detached.
  const toasts = [];
  const append = window.Node.prototype.appendChild;
  window.Node.prototype.appendChild = function (child) {
    const kind = /(?:^|\s)toast-(\w+)/.exec(child?.className || '')?.[1];
    if (kind && kind !== 'container') toasts.push({ kind, text: child.textContent });
    return append.call(this, child);
  };
  try {
    await Screen.startSmartTestBulk();
    await flush();
  } finally {
    window.Node.prototype.appendChild = append;
  }
  const sent = kinds('tentaNasDiskSmartTestRequest');
  assert.deepEqual(sent.map((c) => c.payload.diskId), ['sda', 'sdb', 'sdc'], 'sdc is still tried after sdb refuses — the batch does not stop');
  // The refusal is a translated sentence around the node's per-disk reason.
  assert.match(toasts.filter((t) => t.kind === 'warning').map((t) => t.text).join('\n'), /Test SMART nie ruszył na 1 dysku: sdb: dysk zajęty/);
  Screen.unmount();
});

// M1: a privilege/credential error must stop the WHOLE batch at once — the
// same rejected password (or the same unarmed channel) would otherwise be
// replayed against sudo once per remaining disk.
test('the bulk SMART button stops at once on a privilege/credential error and sends exactly one request', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      disks: [disk({}), disk({ diskId: 'sdb', name: 'sdb' }), disk({ diskId: 'sdc', name: 'sdc' })],
      telemetry: fixtures.tentaNasDisksListRequest.telemetry,
    },
    tentaNasDiskSmartTestRequest: () => Promise.reject(Object.assign(
      new Error('Kanał uprawnień systemowych nie jest dostępny (sudo rejected the password)'), { code: 'NotAvailable' },
    )),
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  const table = root.querySelector('#nas-disk-table');
  for (const row of table.rows) {
    table.dispatchEvent(new window.CustomEvent('row-select', { detail: { row, index: 0, selected: true } }));
  }
  await Screen.startSmartTestBulk();
  await flush();
  const sent = kinds('tentaNasDiskSmartTestRequest');
  assert.equal(sent.length, 1, 'sda fails with a credential error — sdb and sdc are never sent');
  assert.equal(sent[0].payload.diskId, 'sda');
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

// A remote node with no known hostname: the prompt must not say "do węzła
// Węzeł bez nazwy" or "tentaflow@Węzeł bez nazwy".
test('the sudo prompt for a nameless remote node reads naturally', async () => {
  stubTransport(fixtures);
  await mountScreen({ node: LOCAL });
  await flush();
  const pending = Screen.promptSudo('x', { nodeId: REMOTE, nodeName: '', isLocal: false });
  await flush();
  const prompt = document.querySelector('tf-window.nas-modal');
  try {
    assert.match(prompt.querySelector('.explain-box').textContent, /^Hasło trafi do tego noda bez nazwy /);
    assert.equal(prompt.querySelector('#nas-sudo-pass').getAttribute('label'), 'Hasło sudo użytkownika tentaflow na nodzie bez nazwy');
    assert.doesNotMatch(prompt.innerHTML, /Node bez nazwy|bbbbbbbbbbbb/);
  } finally {
    // Closed even when an assertion fails, so a pending prompt never keeps
    // the test process alive.
    prompt.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'cancel' } }));
    await pending;
    prompt.remove();
    Screen.unmount();
  }
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
// On a REMOTE row the arrays are not counted at all (fleet.rs publishes 0 and
// scopes only the local row), so there the count is "—", and the Shares count
// is the one the node's own share list answered, never the published 0/3.
test('zakładka Pule liczy pule ZFS RAZEM z macierzami, pozostałe badge pozostają pomiarami noda', async () => {
  stubTransport({ ...fixtures, tentaNasNodesListRequest: { localNodeId: LOCAL,
    nodes: [node({ poolsTotal: 0, arraysTotal: 2 }), node({ nodeId: REMOTE, isLocal: false, poolsTotal: 7, disksTotal: 9, sharesTotal: 3 })] } });
  const root = await mountScreen({ node: LOCAL, tab: 'pools' });
  try {
    assert.equal(root.querySelector('tf-tab#pools').getAttribute('count'), '2',
      'no ZFS pool, two arrays — the node is not storage-less');
    assert.equal(root.querySelector('tf-tab#pools').hasAttribute('title'), false);
    assert.equal(root.querySelector('tf-tab#disks').getAttribute('count'), '2');
    assert.equal(root.querySelector('tf-tab#shares').getAttribute('count'), '1');
    assert.equal(root.querySelector('tf-tab#jobs').getAttribute('count'), '1');
    Screen.selectNode(REMOTE, 'pools');
    await flush();
    await flush();
    const pools = root.querySelector('tf-tab#pools');
    assert.equal(pools.getAttribute('count'), '—', 'the ZFS half is not passed off as pools + arrays');
    assert.match(pools.getAttribute('title'), /nie są tu liczone/);
    assert.equal(root.querySelector('tf-tab#disks').getAttribute('count'), '9');
    assert.ok(kinds('tentaNasSharesListRequest').some((c) => c.options.targetNodeId === REMOTE), 'the remote node was asked for its shares');
    assert.equal(root.querySelector('tf-tab#shares').getAttribute('count'), '1', 'what the node itself answered, not the published figure');
    assert.equal(root.querySelector('tf-tab#shares').hasAttribute('title'), false);
  } finally { Screen.unmount(); }
});

// Backlog N1 minor: a remote node's own screen read "—" on its Pools tab even
// after the node had answered both lists for this organisation. Its overview
// now sets the count from those answers — and only when both halves came
// back, so a failed array list never passes the ZFS pools off as the whole.
test('a remote node view counts its Pools tab from the lists the node itself answered', async () => {
  const remote = { localNodeId: LOCAL, nodes: [node({}), node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, poolsTotal: 5, arraysTotal: 0 })] };
  stubTransport({ ...fixtures, tentaNasNodesListRequest: remote,
    tentaNasElasticArraysListRequest: { arrays: [elasticArray()] } });
  let root = await mountScreen({ node: REMOTE });
  try {
    await flush();
    const pools = root.querySelector('tf-tab#pools');
    assert.equal(pools.getAttribute('count'), '2', 'one ZFS pool and one array, as the node answered');
    assert.equal(pools.hasAttribute('title'), false);
  } finally { Screen.unmount(); }

  stubTransport({ ...fixtures, tentaNasNodesListRequest: remote,
    tentaNasElasticArraysListRequest: () => { throw new Error('array probe failed'); } });
  root = await mountScreen({ node: REMOTE });
  try {
    await flush();
    assert.equal(root.querySelector('tf-tab#pools').getAttribute('count'), '—', 'half an answer is no count');
  } finally { Screen.unmount(); }
});

test('a remote node view whose share list fails shows "—" on the Shares tab, not the published figure', async () => {
  stubTransport({ ...fixtures,
    tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [node({}), node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, sharesTotal: 0 })] },
    tentaNasSharesListRequest: (payload, options) => {
      if (options.targetNodeId === REMOTE) throw new Error('peer unreachable');
      return fixtures.tentaNasSharesListRequest;
    } });
  const root = await mountScreen({ node: REMOTE });
  try {
    await flush();
    const tab = root.querySelector('tf-tab#shares');
    assert.equal(tab.getAttribute('count'), '—');
    assert.match(tab.getAttribute('title'), /Tu nie liczone/);
  } finally { Screen.unmount(); }
});

test('the node header carries the disk-warning and service chips plus the mockup badges', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const chips = [...root.querySelectorAll('#nas-head-chips tf-chip')].map((c) => c.getAttribute('label'));
  // The fixture node runs smbd (the shares list's service row), and that —
  // not which packages the probe found — is what the chip reports.
  assert.deepEqual(chips, ['Dyski OK', 'Usługi aktywne']);
  const badges = [...root.querySelectorAll('#nas-head-badges tf-chip')].map((c) => c.getAttribute('label'));
  assert.equal(badges[0], 'OpenZFS 2.2.4');
  assert.equal(badges[1], 'Kanał uprawnień: tryb A');
  assert.equal(badges[2], 'mesh: 3 nody');
  const sub = root.querySelector('#nas-head-sub').textContent;
  assert.match(sub, /^node orion · uptime 1 h 0 min · TentaNas 1\.4\.0 · ostatnie odświeżenie/);
  assert.ok(!/6\.1/.test(sub), 'the kernel is not in the header sub');
  Screen.unmount();
});

// The node view's header subline used to wrap EVERY node — named or not — in
// "node.head_sub_node" ("węzeł {name}"). With a nameless node that doubled the
// word: "node.unnamed" is itself "Węzeł bez nazwy" ("node without a name"),
// so the sentence read "węzeł Węzeł bez nazwy". `nodeHeadSub` skips the
// template for the nameless case instead of nesting one translation inside
// another.
test('the node header subline reads naturally for a node with no known hostname', async () => {
  stubTransport({
    ...fixtures,
    tentaNasNodesListRequest: {
      ...fixtures.tentaNasNodesListRequest,
      nodes: fixtures.tentaNasNodesListRequest.nodes.map((n) => (n.nodeId === LOCAL ? { ...n, nodeName: '' } : n)),
    },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const sub = root.querySelector('#nas-head-sub').textContent;
  assert.match(sub, /^Node bez nazwy · uptime/);
  assert.doesNotMatch(sub, /node Node bez nazwy/i, 'the word "node" is not doubled');
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

// n02:299 — an array with bytes waiting on its cache outside parity leads its
// mini row with that figure (the node's `cacheUnprotectedBytes`), in warning.
test('the overview mini row of an Elastic Array with data waiting on its cache names the bytes', async () => {
  const waiting = elasticArray({
    cacheDisks: [{ diskId: 'nv0', name: 'c1', diskName: 'nvme0n1', role: 'cache', sizeBytes: TIB }],
    protection: { status: 'window_open', protectedAsOf: '2026-09-02 09:00:00', cacheUnprotectedBytes: 18 * 1024 ** 3 },
  });
  stubTransport({ ...fixtures, tentaNasElasticArraysListRequest: { arrays: [waiting] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  try {
    const chip = root.querySelector('#nas-ov-pools .pool-mini[data-array="produkt"] [data-role="protection"]');
    assert.match(chip.getAttribute('label'), /^18(\.0)? GiB na cache bez parity$/);
    assert.equal(chip.getAttribute('status'), 'warn');
  } finally {
    Screen.unmount();
  }
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

// The owner's hard rule, end to end on n02: a poll that moves the ARC numbers,
// a job's progress and an alert's age must not rebuild the ARC card, the job
// row, its Cancel button, the alert row or its buttons — only the values
// painted inside them move (BLOCKER 2, n01-n10 critic 2026-09-21). Before the
// fix, `paintArcCard` and `renderAlertList` each compared one joined string
// for the WHOLE card, and the running-jobs card used `patchHtml` over the
// whole list — any one of these three changing rebuilt everything alongside
// it, including buttons the fixed-node assertions below catch as `!==`.
test('n02: a poll with changed ARC numbers, job progress and a newer alert time keeps every card, row and button the same node', async () => {
  // A `pool_scrub` job: the one kind `jobCanCancel` (format.js) offers Cancel
  // for. With a job that has no Cancel the identity check below would compare
  // nothing to nothing and pass vacuously.
  const runningV1 = [{ jobId: 'j1', kind: 'pool_scrub', subject: 'tank', status: 'running', progressPct: 10, startedBy: 'admin', startedAt: '2026-09-02 09:58:00', finishedAt: null, error: null, log: ['started'] }];
  const runningV2 = [{ jobId: 'j1', kind: 'pool_scrub', subject: 'tank', status: 'running', progressPct: 55, startedBy: 'admin', startedAt: '2026-09-02 09:58:00', finishedAt: null, error: null, log: ['started', 'halfway'] }];
  const alertV1 = { alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'nvme0n1', title: 'nvme0n1: pending sectors', detail: 'w 7 dni', raisedAt: '2026-08-01 10:00:00', ackedAt: null, resolvedAt: null };
  const alertV2 = { ...alertV1, raisedAt: '2026-09-20 10:00:00' };
  const arcV1 = { ...arc };
  const arcV2 = { ...arc, sizeBytes: 2.5e9, hitRatio: 88.1, mruBytes: 9e8, mfuBytes: 1.3e9 };
  let poll = 0;
  stubTransport({
    ...fixtures,
    tentaNasJobsListRequest: () => ({ jobs: poll === 0 ? runningV1 : runningV2 }),
    tentaNasAlertsListRequest: () => ({ alerts: [poll === 0 ? alertV1 : alertV2] }),
    tentaNasArcStatsRequest: () => ({ arc: poll === 0 ? arcV1 : arcV2 }),
  });
  try {
    const root = await mountScreen({ node: LOCAL });
    await flush();
    const body = root.querySelector('#nas-tab-body');

    const arcHost = root.querySelector('#nas-ov-arc');
    const donut1 = arcHost.querySelector('.donut');
    const dnVal1 = arcHost.querySelector('.dn-val');
    const jobRow1 = root.querySelector('#nas-ov-jobs .job-row');
    const cancelBtn1 = jobRow1.querySelector('[data-act="cancel"]');
    assert.ok(cancelBtn1 !== null, 'the running scrub offers Cancel — the identity check below is not vacuous');
    const bar1 = jobRow1.querySelector('tf-progress-bar');
    const alertRow1 = root.querySelector('#nas-ov-alerts .alert-row');
    const gotoBtn1 = alertRow1.querySelector('[data-goto]');
    const ackBtn1 = alertRow1.querySelector('[data-ack]');
    const agoText1 = alertRow1.querySelector('.a-sub').textContent;
    assert.equal(bar1.getAttribute('value'), '10');
    assert.match(dnVal1.textContent, /94\.2%/);

    poll = 1;
    await Screen.refreshOverview(body);
    await flush();

    // ARC: the very same card AND the very same donut ring inside it — a full
    // `host.innerHTML =` on every poll (the pre-fix behaviour) keeps the outer
    // container's identity but destroys everything inside it, which the two
    // checks below catch that a container-only check would miss.
    assert.ok(root.querySelector('#nas-ov-arc') === arcHost, 'the ARC card container is the same node');
    assert.ok(arcHost.querySelector('.donut') === donut1, 'the donut ring is the same node');
    assert.ok(arcHost.querySelector('.dn-val') === dnVal1, 'its value element is the same node');
    assert.match(dnVal1.textContent, /88\.1%/, 'and its hit ratio moved');

    // The running job: the very same row and Cancel button, its progress moved.
    const jobRow2 = root.querySelector('#nas-ov-jobs .job-row');
    assert.ok(jobRow2 === jobRow1, 'the job row survives the poll');
    assert.ok(jobRow2.querySelector('[data-act="cancel"]') === cancelBtn1, 'its Cancel button is the same node');
    const bar2 = jobRow2.querySelector('tf-progress-bar');
    assert.ok(bar2 === bar1, 'the progress bar is the same node');
    assert.equal(bar2.getAttribute('value'), '55', 'and its value moved');

    // The alert: the very same row and buttons, its age moved.
    const alertRow2 = root.querySelector('#nas-ov-alerts .alert-row');
    assert.ok(alertRow2 === alertRow1, 'the alert row survives the poll');
    assert.ok(alertRow2.querySelector('[data-goto]') === gotoBtn1, 'its "Szczegóły" button is the same node');
    assert.ok(alertRow2.querySelector('[data-ack]') === ackBtn1, 'its "Potwierdź" button is the same node');
    assert.notEqual(alertRow2.querySelector('.a-sub').textContent, agoText1, 'and the relative time moved');
  } finally {
    Screen.unmount();
  }
});

// MINOR 6 (critic-round2-wave1-2026-09-22.md): `alertRowSkeleton`'s comment
// claimed "everything here is fixed for the life of an alert", but
// `raise_coded_alert` refreshes the code, its parameters and the English on
// the node, and a cache-stuck alert's wait time changes every minute below
// 1 h and every hour above. Before the fix, title and detail were baked into
// the row's markup, so `patchKeyedList` rebuilt the whole row — Ack and
// "Szczegóły" buttons included — every time either one ticked.
test('n02: an alert whose title/detail change keeps its row and its Ack/"Szczegóły" buttons (MINOR 6)', async () => {
  const alertV1 = {
    alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'nvme0n1', title: 'Disk nvme0n1: warning', detail: '2 pending sectors',
    code: 'disk_health', params: { health: 'warning', name: 'nvme0n1', name_source: 'live' }, reasons: [{ code: 'pending_sectors', params: { count: '2' } }],
    raisedAt: '2026-08-01 10:00:00', ackedAt: null, resolvedAt: null,
  };
  // The disk left the inventory and its pending count grew.
  const alertV2 = {
    ...alertV1, title: 'Disk last seen as nvme0n1: warning', detail: '5 pending sectors',
    params: { ...alertV1.params, name_source: 'last_known' }, reasons: [{ code: 'pending_sectors', params: { count: '5' } }],
  };
  let poll = 0;
  stubTransport({
    ...fixtures,
    tentaNasAlertsListRequest: () => ({ alerts: [poll === 0 ? alertV1 : alertV2] }),
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const body = root.querySelector('#nas-tab-body');
  try {
    const alertRow1 = root.querySelector('#nas-ov-alerts .alert-row');
    const gotoBtn1 = alertRow1.querySelector('[data-goto]');
    const ackBtn1 = alertRow1.querySelector('[data-ack]');
    assert.equal(alertRow1.querySelector('.a-title').textContent, 'nvme0n1: 2 sektory czekają na realokację');
    assert.equal(alertRow1.querySelector('.a-title').getAttribute('title'), 'Disk nvme0n1: warning — 2 pending sectors');

    poll = 1;
    await Screen.refreshOverview(body);
    await flush();

    const alertRow2 = root.querySelector('#nas-ov-alerts .alert-row');
    assert.ok(alertRow2 === alertRow1, 'the alert row is the SAME node after title/detail changed');
    assert.ok(alertRow2.querySelector('[data-goto]') === gotoBtn1, 'its "Szczegóły" button is the SAME node');
    assert.ok(alertRow2.querySelector('[data-ack]') === ackBtn1, 'its "Potwierdź" button is the SAME node');
    assert.equal(alertRow2.querySelector('.a-title').textContent, 'Dysk ostatnio widziany jako nvme0n1: 5 sektorów czeka na realokację', 'the title actually updated');
    assert.equal(alertRow2.querySelector('.a-title').getAttribute('title'), 'Disk last seen as nvme0n1: warning — 5 pending sectors', 'and its tooltip');
  } finally {
    Screen.unmount();
  }
});

// Wave-4 round-2 critic minor 1: a disk alert with one reason has it in the
// title and an EMPTY detail, and the sub-line read "· dysk · 2 dni temu".
// The detail's separator is shown with the detail, patched in place.
test('n02: the alert sub-line joins only the parts it has, in place across polls', async () => {
  const oneReason = {
    alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'wwn-0x5000c500a1b2c3d4', title: 'Disk sdd: warning', detail: '3 reallocated sectors',
    code: 'disk_health', params: { health: 'warning', name: 'sdd', name_source: 'live' }, reasons: [{ code: 'reallocated', params: { count: '3' } }],
    raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null,
  };
  const twoReasons = { ...oneReason, reasons: [...oneReason.reasons, { code: 'crc_errors', params: { count: '1' } }] };
  let poll = 0;
  stubTransport({ ...fixtures, tentaNasAlertsListRequest: () => ({ alerts: [poll === 0 ? oneReason : twoReasons] }) });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const body = root.querySelector('#nas-tab-body');
  try {
    const sub = root.querySelector('#nas-ov-alerts .a-sub');
    assert.equal(root.querySelector('#nas-ov-alerts .a-title').textContent, 'sdd: 3 realokowane sektory');
    assert.match(sub.textContent, /^dysk · /, 'no leading separator before an empty detail');
    assert.doesNotMatch(sub.textContent, /wwn-/);
    assert.equal(sub.getAttribute('title'), null, 'and the disk id is not the tooltip');

    poll = 1;
    await Screen.refreshOverview(body);
    await flush();
    const sub2 = root.querySelector('#nas-ov-alerts .a-sub');
    assert.ok(sub2 === sub, 'the same sub-line, patched');
    assert.match(sub2.textContent, /^1 błąd CRC \(kabel lub backplane\) · dysk · /, 'a detail gets its separator');

    poll = 0;
    await Screen.refreshOverview(body);
    await flush();
    assert.match(root.querySelector('#nas-ov-alerts .a-sub').textContent, /^dysk · /, 'and loses it with the detail');
  } finally {
    Screen.unmount();
  }
});

// n02 names a disk alert's pool on its sub-line ("tank · raidz2", n02:316)
// and n01 ends the alert with "— zaplanuj wymianę dysku" (n01:298) — each
// only from what the node put on the alert (disks.rs `HealthAlertPlace`):
// no pool param, no pool; no advice param, no suffix.
test('n01/n02: a disk alert carries its pool and the node\'s replacement advice only when the node sent them', async () => {
  const advised = {
    alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'wwn-0x5000c500a1b2c3d4', title: 'Disk sdd: warning', detail: '',
    code: 'disk_health', params: { health: 'warning', name: 'sdd', name_source: 'live', pool: 'tank', layout: 'raidz2', advice: 'replace' },
    reasons: [{ code: 'reallocated_growing', params: { from: '5', to: '8' } }],
    raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null,
  };
  const plain = { ...advised, alertId: 'a2', subjectId: 'wwn-0x5000c500a1b2c3d5', params: { health: 'warning', name: 'sdf', name_source: 'live' } };
  let alerts = [advised, plain];
  stubTransport({ ...fixtures, tentaNasAlertsListRequest: () => ({ alerts }) });
  let root = await mountScreen({ node: LOCAL });
  await flush();
  try {
    const row = (id) => root.querySelector(`#nas-ov-alerts .alert-row[data-alert="${id}"]`);
    const sub = row('a1').querySelector('.a-sub');
    assert.match(sub.textContent, /dysk · tank · RAIDZ2 · /, 'the pool and its layout, from the alert');
    assert.doesNotMatch(row('a1').textContent, /zaplanuj/, 'n02 keeps the title as the mockup writes it');
    assert.doesNotMatch(row('a2').querySelector('.a-sub').textContent, /tank|RAIDZ/, 'no pool param, no pool');
    // A re-raise that moves the disk out of the pool patches the same line.
    alerts = [{ ...advised, params: { health: 'warning', name: 'sdd', name_source: 'live' } }, plain];
    await Screen.refreshOverview(root.querySelector('#nas-tab-body'));
    await flush();
    assert.ok(row('a1').querySelector('.a-sub') === sub, 'patched in place');
    assert.doesNotMatch(sub.textContent, /tank/);
  } finally {
    Screen.unmount();
  }

  alerts = [advised, plain];
  root = await mountScreen();
  await flush();
  await flush();
  try {
    const cells = root.querySelector('#nas-fleet-alerts').rows.filter((r) => r._row.alert).map((r) => r.alert);
    const cell = (name) => cells.find((c) => c.includes(`>${name}:`));
    assert.match(cell('sdd'), />sdd: [^<]* — zaplanuj wymianę dysku</, 'the node advised it');
    assert.doesNotMatch(cell('sdf'), /zaplanuj/, 'the node did not');
  } finally {
    Screen.unmount();
  }
});

// A conflict's quarantined copy is named after its operation's uuid, so the
// row never shows where it is — and the admin had no way to find it. Each
// file gets a deliberate "copy path" control: labelled by the file's own
// path, it puts the node's path of the other version on the clipboard on a
// click, and that path never enters the DOM, not even as a tooltip.
test('n02: a conflict row copies the other version\'s path on request and never renders it', async () => {
  const uuid = '01a0cf8c-5a61-7283-8410-924a0fceb01f';
  const kept = `/mnt/tentanas-branches/media/cache/nvme2n1/.tentanas-quarantine-${uuid}-3`;
  const alert = {
    alertId: 'c1', severity: 'warning', subjectKind: 'elastic-array', subjectId: 'media', title: 'Pliki zachowane w dwóch wersjach: 2', detail: '',
    code: 'elastic_conflict', params: { array: 'media', count: '2' },
    reasons: [
      { code: 'conflict_file', params: { path: 'docs/a.odt', visible: '/mnt/media/docs/a.odt', kept_kind: 'quarantine', kept_disk: 'nvme2n1', kept_path: kept } },
      { code: 'conflict_file', params: { path: 'docs/b.odt', visible: '/mnt/media/docs/b.odt', kept_kind: 'branch' } },
    ],
    raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null,
  };
  const copied = [];
  const clipboard = Object.getOwnPropertyDescriptor(globalThis.navigator, 'clipboard');
  Object.defineProperty(globalThis.navigator, 'clipboard', { configurable: true, value: { writeText: async (t) => { copied.push(t); } } });
  stubTransport({ ...fixtures, tentaNasAlertsListRequest: () => ({ alerts: [alert] }) });
  const rec = recordToasts();
  const root = await mountScreen({ node: LOCAL });
  await flush();
  try {
    const row = root.querySelector('#nas-ov-alerts .alert-row[data-alert="c1"]');
    const buttons = [...row.querySelectorAll('[data-copy]')];
    assert.equal(buttons.length, 1, 'only a copy inside this array can be copied');
    assert.equal(buttons[0].textContent.trim(), 'Kopiuj ścieżkę drugiej wersji: docs/a.odt');
    assert.doesNotMatch(root.innerHTML, new RegExp(uuid), 'the path is nowhere in the DOM');
    await Screen.refreshOverview(root.querySelector('#nas-tab-body'));
    await flush();
    assert.ok(row.querySelector('[data-copy]') === buttons[0], 'the control survives a poll');
    click(buttons[0]);
    await flush();
    assert.deepEqual(copied, [kept], 'the click copies the other version\'s path');
    assert.match(rec.toasts.map((t) => t.text).join('\n'), /Skopiowano ścieżkę drugiej wersji pliku docs\/a\.odt/);
    assert.doesNotMatch(rec.toasts.map((t) => t.text).join('\n'), new RegExp(uuid));
  } finally {
    rec.restore();
    if (clipboard) Object.defineProperty(globalThis.navigator, 'clipboard', clipboard);
    else delete globalThis.navigator.clipboard;
    Screen.unmount();
  }
});

// Wave-4 round-2 critic minor 4: the "Treść węzła" section and the tooltip
// are the node's own text, and ids in it are taken out — a node id the fleet
// knows becomes that node's name, anything else a neutral word. The same on
// the n01 fleet cell.
test('n01/n02: the node text section and the alert tooltips carry no id', async () => {
  const HEX = 'c'.repeat(64);
  const uuid = '0191f2c0-4b1e-7c3a-9f2d-8ac41b5e9d70';
  const alert = {
    alertId: 'a1', severity: 'warning', subjectKind: 'elastic-array', subjectId: 'media', title: 'Macierz wymaga interwencji',
    detail: `forwarded from ${HEX}`,
    code: 'elastic_needs_attention', params: { array: 'media', helper_detail: `rename stuck on .tentanas-transfer-${uuid}-2` }, reasons: [],
    raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null,
  };
  const nodes = {
    ...fixtures.tentaNasNodesListRequest,
    nodes: [...fixtures.tentaNasNodesListRequest.nodes, node({ nodeId: HEX, nodeName: 'atlas', isLocal: false, online: false })],
  };
  stubTransport({
    ...fixtures,
    tentaNasNodesListRequest: nodes,
    tentaNasAlertsListRequest: (payload, options) => {
      // An unreachable node's error is node text too.
      if (options?.targetNodeId === REMOTE) throw new Error(`forward via ${HEX} timed out`);
      return { alerts: [alert] };
    },
  });
  let root = await mountScreen({ node: LOCAL });
  await flush();
  try {
    const row = root.querySelector('#nas-ov-alerts .alert-row');
    const nodeText = row.querySelector('[data-role="node-text"]').textContent;
    assert.equal(row.querySelector('[data-role="node"]').hidden, false);
    assert.match(nodeText, /forwarded from atlas/, 'a known node id reads as its name');
    assert.match(nodeText, /\.tentanas-transfer-\[identyfikator\]-2/, 'an operation uuid as the neutral word');
    for (const el of [row, ...row.querySelectorAll('*')]) {
      assert.doesNotMatch(`${el.getAttribute('title') || ''} ${el.textContent}`, /0191f2c0|c{32,}/, el.tagName);
    }
  } finally {
    Screen.unmount();
  }

  root = await mountScreen();
  await flush();
  await flush();
  try {
    const rows = root.querySelector('#nas-fleet-alerts').rows;
    const cells = rows.filter((r) => r._row.alert).map((r) => `${r.node}${r.alert}`);
    assert.ok(cells.length > 0);
    for (const cell of cells) {
      assert.doesNotMatch(cell, /0191f2c0|c{32,}/, cell);
      assert.match(cell, /forwarded from atlas/);
    }
    const offline = rows.find((r) => r._row.error);
    assert.match(offline.alert, /forward via atlas timed out/);
    assert.doesNotMatch(`${offline.node}${offline.alert}`, /c{32,}/);
  } finally {
    Screen.unmount();
  }
});

// M1: an alert raised by an older node (no code) or with a code this build
// does not know is shown on n02 as a translated generic alert, the node's own
// text only as the tooltip (the n01 cell goes through the same `alertText`,
// see the fleet alert test).
test('n02: an uncoded or unknown alert reads as a generic translated alert with the node text as its tooltip', async () => {
  const old = { alertId: 'a1', severity: 'warning', subjectKind: 'elastic-array', subjectId: 'produkt', title: 'Macierz wymaga interwencji', detail: 'rezerwacje zachowane', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null };
  const future = { ...old, alertId: 'a2', code: 'array_on_fire', params: { array: 'produkt' }, reasons: [], title: 'Array produkt: on fire', detail: 'call someone' };
  stubTransport({ ...fixtures, tentaNasAlertsListRequest: { alerts: [old, future] } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  try {
    const rows = [...root.querySelectorAll('#nas-ov-alerts .alert-row')];
    assert.equal(rows.length, 2);
    for (const [row, alert] of [[rows[0], old], [rows[1], future]]) {
      const title = row.querySelector('.a-title');
      assert.equal(title.textContent, 'Alert węzła');
      assert.equal(title.getAttribute('title'), `${alert.title} — ${alert.detail}`);
      assert.match(row.querySelector('[data-role="detail"]').textContent, /w treści węzła/);
      // Wave-4 critic minor 13: a tooltip cannot be reached on a phone, so the
      // same node text sits one tap away — collapsed, and never the row's
      // own title or detail line.
      const node = row.querySelector('[data-role="node"]');
      assert.equal(node.tagName, 'DETAILS');
      assert.equal(node.hidden, false, 'offered where the detail points at it');
      assert.equal(node.open, false, 'collapsed until tapped');
      assert.equal(node.querySelector('[data-role="node-text"]').textContent, `${alert.title} — ${alert.detail}`);
      const outside = row.cloneNode(true);
      outside.querySelector('[data-role="node"]').remove();
      assert.doesNotMatch(outside.textContent, /on fire|call someone|interwencji/, 'the node text is never the row text');
    }
  } finally {
    Screen.unmount();
  }
});

// MINOR 7, part 1 (critic-round2-wave1-2026-09-22.md): `paintArcCard` used to
// add its delegated click listener on `host` inside the `!host.__tfArcBuilt`
// branch, and ARC going unavailable reset that very flag — so each
// unavailable→available cycle wired one MORE `click` listener onto the same
// `host` node, and one l2arc click called `openPool` once per cycle it had
// survived.
test('n02: ARC unavailable→available twice still fires the l2arc click exactly once per click (MINOR 7)', async () => {
  const arcAvailable = { ...arc, l2arcPools: [] };
  let available = true;
  stubTransport({
    ...fixtures,
    tentaNasArcStatsRequest: () => ({ arc: available ? arcAvailable : null }),
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const body = root.querySelector('#nas-tab-body');

  let opens = 0;
  const originalOpenPool = Screen.openPool;
  Screen.openPool = () => { opens += 1; };
  try {
    const clickL2arc = () => {
      const link = root.querySelector('#nas-ov-arc [data-act="arc-l2arc"]');
      assert.ok(link, 'the l2arc add-link is offered while ARC is available');
      click(link);
    };

    clickL2arc();
    assert.equal(opens, 1, 'the first click opens the pool exactly once');

    for (let i = 0; i < 2; i++) {
      available = false;
      await Screen.refreshOverview(body);
      await flush();
      assert.equal(root.querySelector('#nas-ov-arc [data-act="arc-l2arc"]'), null, 'ARC unavailable hides the link');
      available = true;
      await Screen.refreshOverview(body);
      await flush();
    }

    opens = 0;
    clickL2arc();
    assert.equal(opens, 1, 'after two unavailable/available cycles, one click still opens the pool exactly once');
  } finally {
    Screen.openPool = originalOpenPool;
    Screen.unmount();
  }
});

// MINOR 7, part 2: the settings button's label used to be written with
// `setText` on the `tf-button` host, which replaces its light-DOM content —
// exactly what `tf-button` treats as a caller overwriting its insides, so it
// rebuilds its inner `<button>` from scratch (tf-button.js). `setAttr` on
// `label` goes through the attribute the component re-renders from in place.
test('n02: the ARC settings button keeps its node when its label changes (MINOR 7)', async () => {
  const arcA = { ...arc, maxBytes: 2e9, ramBytes: 8e9 }; // ramPct 25
  const arcB = { ...arc, maxBytes: 4e9, ramBytes: 8e9 }; // ramPct 50
  let poll = 0;
  stubTransport({
    ...fixtures,
    tentaNasArcStatsRequest: () => ({ arc: poll === 0 ? arcA : arcB }),
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const body = root.querySelector('#nas-tab-body');
  try {
    const limitBtn = root.querySelector('#nas-ov-arc-actions [data-act="arc-limit"]');
    assert.ok(limitBtn, 'the settings button is offered');
    const labelBefore = limitBtn.getAttribute('label');
    assert.match(labelBefore, /25/);

    poll = 1;
    await Screen.refreshOverview(body);
    await flush();

    assert.ok(root.querySelector('#nas-ov-arc-actions [data-act="arc-limit"]') === limitBtn, 'the settings button is the SAME node after its label changed');
    assert.notEqual(limitBtn.getAttribute('label'), labelBefore, 'and the label actually updated');
    assert.match(limitBtn.getAttribute('label'), /50/);
  } finally {
    Screen.unmount();
  }
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
  // not said, and the mockups show no identifier in an alert row at all. Not
  // as a tooltip either (owner's rule: no ids anywhere in the GUI; wave-4
  // round-2 critic minor 5).
  stubTransport({
    ...fixtures,
    tentaNasAlertsListRequest: {
      alerts: [
        { alertId: 'a1', severity: 'warning', subjectKind: 'disk', subjectId: 'wwn-5000cca27dc7a4c6', title: 'Disk sdg: warning', detail: '1 UDMA CRC errors', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
        { alertId: 'a2', severity: 'warning', subjectKind: 'approval', subjectId: '0191f2c0-4b1e-7c3a-9f2d-8ac41b5e9d70', title: 'Operacja czeka na drugiego admina', detail: 'pool tank', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
        { alertId: 'a3', severity: 'warning', subjectKind: 'elastic-array', subjectId: 'produkt', title: 'Macierz oczekuje na przywrócenie', detail: 'wymagane jawne Przywróć', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
        // The third disk-id shape `disks.rs` builds: a virtual disk with no
        // WWN and no serial is keyed `dev-<kernel name>`.
        { alertId: 'a4', severity: 'warning', subjectKind: 'disk', subjectId: 'dev-vdb', title: 'Disk vdb: warning', detail: 'SMART', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
      ],
    },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const rows = [...root.querySelectorAll('#nas-ov-alerts .alert-row')];
  assert.equal(rows.length, 4);
  assert.doesNotMatch(rows[3].querySelector('.a-sub').textContent, /dev-vdb/);

  const disk = rows[0].querySelector('.a-sub');
  assert.doesNotMatch(disk.textContent, /wwn-/);

  assert.doesNotMatch(rows[1].querySelector('.a-sub').textContent, /0191f2c0/);
  // No id in any attribute of any row: not the sub-line's tooltip, not the
  // title's, not a node-text section.
  for (const row of rows) {
    for (const el of [row, ...row.querySelectorAll('*')]) {
      if (el.getAttribute('title')) assert.doesNotMatch(el.getAttribute('title'), /wwn-|dev-vdb|0191f2c0/, el.outerHTML);
    }
  }

  // …and a real name still shows, next to the translated kind — the raw
  // `elastic-array` enum never reaches the Polish sentence.
  assert.match(rows[2].querySelector('.a-sub').textContent, /macierz Elastic produkt/);
  assert.doesNotMatch(rows[2].querySelector('.a-sub').textContent, /elastic-array/);
  Screen.unmount();
});

// A pool/target/array/dataset alert's `subjectId` is a NAME the node chose
// (`spec.name`, `row.name`) — never a disk id, even when that name happens to
// take the same shape as one. Routing every kind through the disk rule (the
// old single `isMachineId`) hid a pool named `usb-backup`, a target
// `pci-store`, a share `dev-backups` or a dataset `2024`/`sn-archive`/
// `mmc-media` as if it were an id; only `subjectKind === 'disk'` may use it.
test('a pool/target name is never hidden as an id, even in a disk-id shape', async () => {
  const names = ['dev-backups', 'usb-backup', 'pci-store', '2024', 'sn-archive', 'mmc-media'];
  const alerts = names.map((name, i) => ({
    alertId: `p${i}`, severity: 'warning', subjectKind: 'target', subjectId: name,
    title: `target ${name}`, detail: 'x', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null,
  }));
  stubTransport({ ...fixtures, tentaNasAlertsListRequest: { alerts } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const rows = [...root.querySelectorAll('#nas-ov-alerts .alert-row')];
  assert.equal(rows.length, names.length);
  names.forEach((name, i) => {
    const sub = rows[i].querySelector('.a-sub');
    assert.match(sub.textContent, new RegExp(name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')), `${name} must stay visible`);
  });
  Screen.unmount();
});

// The exact same `wwn-…` string is a disk id on a `disk` alert (hidden —
// already covered above) but a plain name on any other
// subject kind, because only a disk alert's subject can BE a disk id.
test('a wwn- shaped subject is hidden only for a disk alert, shown for other kinds', async () => {
  const wwn = 'wwn-0x5000c500a1b2c3d4';
  const alerts = [
    { alertId: 'd1', severity: 'warning', subjectKind: 'disk', subjectId: wwn, title: 'Disk warning', detail: 'x', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
    { alertId: 'p1', severity: 'warning', subjectKind: 'target', subjectId: wwn, title: 'target warning', detail: 'x', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null },
  ];
  stubTransport({ ...fixtures, tentaNasAlertsListRequest: { alerts } });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  const rows = [...root.querySelectorAll('#nas-ov-alerts .alert-row')];
  assert.doesNotMatch(rows[0].querySelector('.a-sub').textContent, /wwn-/, 'hidden for the disk alert');
  assert.equal(rows[0].querySelector('.a-sub').getAttribute('title'), null, 'and not the tooltip either');
  assert.match(rows[1].querySelector('.a-sub').textContent, /wwn-/, 'shown for the target alert');
  Screen.unmount();
});

// An Elastic Array alert asks for something done on that array's own pane — a
// restore, a repair, a settled mover. Without its own branch the button led to
// the Overview the admin was already on.
test('an Elastic Array alert opens that array, from the node and from the fleet', async () => {
  const alert = { alertId: 'a1', severity: 'warning', subjectKind: 'elastic-array', subjectId: 'produkt', title: 'Macierz wymaga interwencji', detail: 'x', raisedAt: '2026-09-01 10:00:00', ackedAt: null, resolvedAt: null };
  stubTransport({ ...fixtures, tentaNasAlertsListRequest: { alerts: [alert] } });
  let root = await mountScreen({ node: LOCAL });
  await flush();
  const go = root.querySelector('#nas-ov-alerts [data-goto]');
  assert.equal(go.textContent.trim(), 'Macierz');
  click(go);
  await flush();
  assert.equal(Screen.tab, 'pools');
  assert.equal(Screen.array, 'produkt');
  Screen.unmount();

  root = await mountScreen();
  await flush();
  await flush();
  const table = root.querySelector('#nas-fleet-alerts');
  const row = table.rows.find((r) => r._row.alert?.subjectKind === 'elastic-array');
  const actions = table.rowActions(row, 0, () => row);
  assert.equal(actions.querySelector('[data-act="go"]').textContent.trim(), 'Macierz');
  click(actions.querySelector('[data-act="go"]'));
  await flush();
  assert.equal(Screen.tab, 'pools');
  assert.equal(Screen.array, 'produkt');
  Screen.unmount();
});

// The node sends an EMPTY name when neither the peer store nor the sync
// registry knows one; it used to send the 64-hex node id, and every fleet
// surface printed that as the name. One helper names such a node, and the id
// is not even a tooltip (owner's rule: no ids anywhere in the GUI).
test('a node with no known hostname is "Node bez nazwy" everywhere, never its id', async () => {
  const nameless = {
    ...fixtures,
    tentaNasNodesListRequest: {
      ...fixtures.tentaNasNodesListRequest,
      nodes: fixtures.tentaNasNodesListRequest.nodes.map((n) => (n.nodeId === REMOTE ? { ...n, nodeName: '' } : n)),
    },
    tentaNasAlertsListRequest: (payload, options) => {
      if (options.targetNodeId === REMOTE) throw new Error('mesh timeout');
      return { alerts: [] };
    },
  };
  stubTransport(nameless);
  const root = await mountScreen();
  await flush();
  await flush();
  const name = root.querySelector(`.node-card[data-node="${REMOTE}"] .nc-name`);
  assert.equal(name.textContent.trim(), 'Node bez nazwy');
  assert.equal(name.getAttribute('title'), null, 'the id is not the tooltip');
  assert.doesNotMatch(root.textContent, /bbbbbbbbbbbb/, 'no visible text carries the node id');
  const offline = root.querySelector('#nas-fleet-alerts').rows.find((r) => r._row.node?.nodeId === REMOTE);
  assert.match(offline.node, />Node bez nazwy</);
  assert.doesNotMatch(offline.node, new RegExp(REMOTE), 'nor the offline row\'s tooltip');
  Screen.unmount();
});

// A job's author is a person by display name, or one of the node's own two
// system authors in words. An id the node could not resolve to an account is
// not a name: it is the tooltip.
test('a job author is a name, a system author in words, or an unknown account with no id anywhere', async () => {
  stubTransport(fixtures);
  await mountScreen({ node: LOCAL, tab: 'pools' });
  const host = document.createElement('div');
  document.body.append(host);
  const uuid = '0191f2c0-4b1e-7c3a-9f2d-8ac41b5e9d70';
  const job = (startedBy) => ({ jobId: startedBy, kind: 'pool_scrub', subject: 'tank', status: 'succeeded', startedBy, startedAt: '2026-09-01 10:00:00', log: [] });
  ['Anna', 'scheduler', 'startup', uuid].forEach((by) => buildJobRow(host, job(by)));
  const subs = [...host.querySelectorAll('.job-sub')];
  assert.match(subs[0].textContent, /^Anna · /);
  assert.match(subs[1].textContent, /^harmonogram · /);
  assert.match(subs[2].textContent, /^start noda · /);
  assert.match(subs[3].textContent, /^nieznane konto · /);
  assert.doesNotMatch(host.textContent, /0191f2c0|scheduler|startup/);
  for (const el of host.querySelectorAll('[title]')) assert.doesNotMatch(el.getAttribute('title'), /0191f2c0/, 'not even as a tooltip');
  host.remove();
  Screen.unmount();
});

// A SMART job's subject is a disk id the node swaps for a name. A name it only
// remembers is marked as last-known; an id it could not swap reads "nieznany dysk".
// The overview card and the job-log header render the same way.
test('a job row marks a last-known subject and keeps a disk id out of the text', async () => {
  stubTransport(fixtures);
  await mountScreen({ node: LOCAL, tab: 'pools' });
  const host = document.createElement('div');
  document.body.append(host);
  const wwn = 'wwn-0x5000cca27dc7a4c6';
  const job = (jobId, subject, extra = {}) => ({ jobId, kind: 'smart_test', subject, status: 'succeeded', startedBy: 'Anna', startedAt: '2026-09-01 10:00:00', log: [], ...extra });
  [job('a', 'sdq', { subjectLastKnown: true }), job('b', wwn), job('c', 'sdd')].forEach((j) => buildJobRow(host, j));
  const subject = (id) => host.querySelector(`.job-row[data-job="${id}"] .job-name .mono`);
  assert.equal(subject('a').textContent, 'ostatnio widziany jako sdq');
  assert.equal(subject('b').textContent, 'nieznany dysk');
  assert.equal(subject('c').textContent, 'sdd');
  assert.doesNotMatch(host.innerHTML, /wwn-/, 'not even as a tooltip');
  host.remove();
  Screen.unmount();
});

test('the fleet node table names a node instead of printing its node id', async () => {
  // n16 prints `atlas` and `orion`. The 64-hex node id is not a name, and
  // not even the tooltip (owner's rule: no ids anywhere in the GUI).
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
    assert.doesNotMatch(row.name, new RegExp(row._node.nodeId), 'the id is not even the tooltip');
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

// Per-organisation figures are published as 0 by every node and filled in
// only on the LOCAL row (fleet.rs `scope_local_shares` / `scope_local_arrays`),
// so a remote row's 0 shares / 0 arrays means "not counted". The card used to
// print it as a fact: "Share 0", an array-only node labelled a client, left out
// of the NAS count, and its pools-only capacity shown as complete.
function remoteFleet(remote, sharesAnswer) {
  stubTransport({ ...fixtures,
    tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [
      node({}),
      node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, poolsTotal: 0, arraysTotal: 0, sharesTotal: 0, features: [], ...remote }),
    ] },
    tentaNasSharesListRequest: (payload, options) => {
      if (options.targetNodeId !== REMOTE) return fixtures.tentaNasSharesListRequest;
      if (sharesAnswer instanceof Error) throw sharesAnswer;
      return sharesAnswer;
    } });
}

function cardStat(card, key) {
  return [...card.querySelectorAll('.nc-stats .kv-inline')].find((kv) => kv.querySelector('.k').textContent === key) || null;
}

test('a remote node card shows "—" for what it does not count, says why, and is not called a client', async () => {
  remoteFleet({}, new Error('peer unreachable'));
  const root = await mountScreen();
  try {
    await flush();
    const card = root.querySelector(`.node-card[data-node="${REMOTE}"]`);
    const shares = cardStat(card, 'Share');
    assert.equal(shares.querySelector('.v').textContent, '—', 'not "0"');
    // The node did not give its share list: that is the reason, not the
    // per-organisation rule (backlog N1 minor).
    assert.match(shares.querySelector('[data-not-counted]').getAttribute('title'), /nie udało się odczytać listy udziałów/);
    const arrays = cardStat(card, 'Elastic Array');
    assert.ok(arrays, 'the arrays stat is shown to say it was not counted');
    assert.equal(arrays.querySelector('.v').textContent, '—');
    assert.match(arrays.querySelector('[data-not-counted]').getAttribute('title'), /Macierze Elastic Array nie są tu liczone/);
    const capacity = cardStat(card, 'Pojemność łączna');
    assert.equal(capacity.querySelector('[data-pools-only]').textContent, 'tylko pule');
    assert.match(card.querySelector('.split-bar').getAttribute('title'), /tylko pule/);
    const role = card.querySelector('.nc-foot').lastElementChild.textContent;
    assert.equal(role, 'brak puli ZFS · macierze niepoliczone', 'a zero that means "not counted" makes no client');

    const badges = [...root.querySelectorAll('#nas-fleet-badges tf-chip')];
    assert.equal(badges[0].getAttribute('label'), '1× NAS: orion · macierze niepoliczone: vega');
    assert.match(badges[0].getAttribute('title'), /nie są tu liczone/);
    assert.match(badges[2].getAttribute('label'), /tylko pule na 1 nodzie$/);
    const capacityTile = [...root.querySelectorAll('.kpi tf-stat-card')][0];
    assert.match(capacityTile.getAttribute('delta'), /tylko pule na 1 nodzie/);

    // The local card keeps its real, scoped figures.
    const local = root.querySelector(`.node-card[data-node="${LOCAL}"]`);
    assert.equal(cardStat(local, 'Share').querySelector('.v').textContent, '1');
    assert.ok(absent(local, '[data-pools-only]'), 'the local capacity includes its arrays');
    assert.ok(absent(local, '[data-not-counted]'), 'the local figures are counted');
  } finally { Screen.unmount(); }
});

// `isLocal` is not "counted": the list handler gives up on a failed scoped
// read (or a caller with no organisation) and leaves the published 0, which
// only `perOrgCounted` tells apart from a real 0. The share list the fleet
// poll asks for fails too, so nothing else counts the shares either.
function localCountedFleet(perOrgCounted) {
  stubTransport({ ...fixtures,
    tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [
      node({ poolsTotal: 1, arraysTotal: 2, sharesTotal: 3, perOrgCounted }),
    ] },
    tentaNasSharesListRequest: () => { throw new Error('database is locked'); } });
}

test('the local node card shows "—" for figures its scoped read did not count', async () => {
  localCountedFleet(false);
  const root = await mountScreen();
  try {
    await flush();
    const card = root.querySelector(`.node-card[data-node="${LOCAL}"]`);
    const shares = cardStat(card, 'Share');
    assert.equal(shares.querySelector('.v').textContent, '—', 'a failed read is not "3" nor "0"');
    assert.match(shares.querySelector('[data-not-counted]').getAttribute('title'), /nie udało się odczytać listy udziałów/);
    const arrays = cardStat(card, 'Elastic Array');
    assert.equal(arrays.querySelector('.v').textContent, '—');
    assert.ok(card.querySelector('[data-pools-only]'), 'the capacity is the pools\' alone');
  } finally { Screen.unmount(); }
});

test('the local node card shows its counted figures when the scoped read succeeded', async () => {
  localCountedFleet(true);
  const root = await mountScreen();
  try {
    await flush();
    const card = root.querySelector(`.node-card[data-node="${LOCAL}"]`);
    assert.equal(cardStat(card, 'Share').querySelector('.v').textContent, '3');
    assert.equal(cardStat(card, 'Elastic Array').querySelector('.v').textContent, '2');
    assert.ok(absent(card, '[data-not-counted]'), 'nothing reads "not counted"');
    assert.ok(absent(card, '[data-pools-only]'));
  } finally { Screen.unmount(); }
});

test('a remote node card counts the shares the node answered for this organisation, and its broken share warns', async () => {
  remoteFleet({}, { ...fixtures.tentaNasSharesListRequest, shares: [share, { ...share, shareId: 's2', name: 'b', state: 'error' }] });
  const root = await mountScreen();
  try {
    await flush();
    const card = root.querySelector(`.node-card[data-node="${REMOTE}"]`);
    assert.equal(cardStat(card, 'Share').querySelector('.v').textContent, '2', 'the node\'s own answer, not the published 0');
    assert.equal(card.querySelector('.nc-head tf-chip').getAttribute('status'), 'warn', 'its broken share makes it a warning');
    assert.ok(card.querySelector('.health-dot').classList.contains('warn'), card.querySelector('.health-dot').className);
  } finally { Screen.unmount(); }
});

test('a remote node with a ZFS pool is a fleet NAS on what it published; the local row keeps "client"', async () => {
  stubTransport({ ...fixtures, tentaNasNodesListRequest: { localNodeId: LOCAL, nodes: [
    node({ poolsTotal: 0, arraysTotal: 0 }),
    node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, poolsTotal: 2, features: [] }),
  ] } });
  const root = await mountScreen();
  try {
    await flush();
    const remote = root.querySelector(`.node-card[data-node="${REMOTE}"]`);
    assert.match(remote.querySelector('.nc-foot').lastElementChild.textContent, /NAS floty/);
    const local = root.querySelector(`.node-card[data-node="${LOCAL}"]`);
    assert.match(local.querySelector('.nc-foot').lastElementChild.textContent, /klient/, 'the local zero IS counted');
    const badge = root.querySelector('#nas-fleet-badges tf-chip').getAttribute('label');
    assert.equal(badge, '1× NAS: vega', 'no "not counted" note when every remote node has a pool');
  } finally { Screen.unmount(); }
});

test('the not-counted words are translated in every locale, with the same placeholders', () => {
  const root = new URL('../../', import.meta.url);
  const all = {};
  for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
    all[lang] = JSON.parse(readFileSync(new URL(`i18n/${lang}.json`, root), 'utf8')).tentanas;
  }
  const keys = [['fleet', 'not_counted_hint'], ['fleet', 'not_answered_hint'], ['fleet', 'arrays_not_counted_hint'], ['fleet', 'role_uncounted'],
    ['fleet', 'pools_only'], ['fleet', 'badge_nas_uncounted'], ['kpi', 'capacity_pools_only']];
  for (const [group, key] of keys) {
    const values = Object.values(all).map((t) => t[group][key]);
    assert.ok(values.every((v) => typeof v === 'string' && v.trim()), `${group}.${key} exists everywhere`);
    assert.equal(new Set(values).size, values.length, `${group}.${key} is really translated, not copied: ${values}`);
  }
  for (const [lang, t] of Object.entries(all)) {
    assert.match(t.fleet.badge_nas_uncounted, /\{nodes\}/, lang);
    assert.match(t.kpi.capacity_pools_only, /\{n\}.*\{n\|/, lang);
  }
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
      disk: disk({ diskId: 'sdd', name: 'sdd', serial: 'ZR9AB12K', role: 'pool', memberOf: 'tank', health: 'warning', healthReason: 'reallocated sectors growing (0 → 3 in 7 days)', healthReasons: [R('reallocated_growing', { from: '0', to: '3' })], vdevRole: 'data', vdevKind: 'raidz2' }),
      attributes: [], selfTests: [], history: [], alerts: [], historyDays: 30,
    },
    tentaNasPoolGetRequest: { pool, properties: [], datasets: [], alerts: [], history: [] },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sdd' });
  await flush();
  await flush();
  assert.equal(kinds('tentaNasPoolGetRequest')[0].payload.name, 'tank');
  // n04:175 — the identification chip carries the status AND its reason.
  assert.equal(root.querySelector('.section-card-head tf-chip').getAttribute('label'), 'Uwaga: realok. 0 → 3');
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
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sda' });
    await flush();
    await flush();
    // m26 / n04:125-134: ONE bar, "TentaNas › orion › Dyski › sda" — the
    // shell's own, with the detail's tail in it, never a second bar under it.
    const crumbs = [...root.querySelectorAll('tf-breadcrumb')];
    assert.equal(crumbs.length, 1, 'a single breadcrumb on the screen');
    const bar = crumbs[0];
    assert.deepEqual([...bar.querySelectorAll('.tf-breadcrumb-item')].map((a) => a.textContent), ['TentaNas', 'orion', 'Dyski', 'sda']);
    const links = () => [...bar.querySelectorAll('a.tf-breadcrumb-item')];
    assert.equal(links()[2].getAttribute('href'), `#/tentanas?node=${LOCAL}&tab=disks`);

    click(links()[2]);
    await flush();
    assert.equal(Screen.diskId, null, 'the "Dyski" crumb returns to the disk list');
    assert.ok(root.querySelector('tf-breadcrumb') === bar, 'the same bar stays on screen');
    assert.deepEqual([...bar.querySelectorAll('.tf-breadcrumb-item')].map((a) => a.textContent), ['TentaNas', 'orion'], 'and loses the tail with the detail');

    click(links()[0]);
    await flush();
    assert.equal(Screen.nodeId, null, 'the "TentaNas" crumb returns to the fleet');
  } finally {
    // A failed assertion above must not leave the disk-detail poll running:
    // the test process would never exit.
    Screen.unmount();
  }
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
  healthReason: 'reallocated sectors growing (0 → 3 in 7 days)',
  healthReasons: [R('reallocated_growing', { from: '0', to: '3' })],
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
          health: detailState.health, healthReason: detailState.healthReason, healthReasons: detailState.healthReasons,
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
  detailState.healthReason = 'reallocated sectors growing (0 → 3 in 7 days)';
  detailState.healthReasons = [R('reallocated_growing', { from: '0', to: '3' })];
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
  assert.equal(chip.getAttribute('label'), 'Uwaga: realok. 0 → 3');
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
  detailState.healthReason = 'reallocated sectors growing (3 → 8 in 7 days)';
  detailState.healthReasons = [R('reallocated_growing', { from: '3', to: '8' })];
  detailState.reallocated = 8;
  detailState.cksum = 5;
  detailState.history = [...detailState.history, { at: '2026-09-02 11:00:00', temperatureC: 41, reallocatedSectors: 8 }];
  await Screen.refreshDiskDetail(body);
  await flush();

  assert.equal(chip.getAttribute('label'), 'Awaria: realok. 3 → 8', 'the health chip carries the new status and reason');
  assert.equal(chip.getAttribute('status'), 'err');
  assert.equal(chip.textContent, 'Awaria: realok. 3 → 8', 'and the component rendered it');
  assert.equal(root.querySelector('[data-f="reallocated"]').textContent, '8', 'the reallocated counter moved');
  assert.equal(root.querySelector('#nas-dd-pool [data-c="cksum"]').textContent, '5', 'and so did the pool checksum counter');
  assert.equal(root.querySelector('#nas-dd-pool [data-c="cksum"]').getAttribute('class'), 'v num-err');
  assert.equal(why.textContent, 'realok. 3 → 8', 'the explanation box follows the reason, in the reader\'s language');
  assert.equal(why.getAttribute('title'), 'reallocated sectors growing (3 → 8 in 7 days)', 'the node\'s sentence is its tooltip');
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
  assert.equal(chip.getAttribute('label'), 'Uwaga: realok. 0 → 3', 'the last good health stays on screen');
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

// n16 MAJOR 16: the ARC card read the node once and never again. It is
// re-read on the overview cadence now, and a read writes only the values
// into the nodes already on screen — and never moves a slider the admin has
// moved but not applied.
test('n16: the ARC card follows the node without rebuilding and keeps an unsaved slider', async () => {
  let current = arc;
  stubTransport({ ...fixtures, tentaNasArcStatsRequest: () => ({ arc: current }) });
  const root = await mountScreen({ node: LOCAL, tab: 'environment' });
  await flush();
  await flush();
  try {
    const host = root.querySelector('#nas-env-arc');
    const size = host.querySelector('[data-f="arc-size"]');
    const slider = host.querySelector('#nas-arc-slider');
    assert.ok(size.textContent.trim(), 'painted');
    const before = size.textContent;
    assert.equal(slider.getAttribute('value'), '25');
    current = { ...arc, sizeBytes: 5e9, hitRatio: 71.5, maxBytes: 4e9 };
    await Screen.paintArcSettings(root.querySelector('#nas-tab-body'));
    await flush();
    assert.ok(host.querySelector('[data-f="arc-size"]') === size, 'the same node, patched');
    assert.notEqual(size.textContent, before, 'the live size moved');
    assert.equal(host.querySelector('[data-f="arc-hit"]').textContent, '71.5%');
    assert.equal(slider.getAttribute('value'), '50', 'the slider follows the node\'s cap');
    // The admin moves it; the next read must not take it back.
    slider.dispatchEvent(new CustomEvent('input', { detail: { value: 30 } }));
    current = { ...arc, maxBytes: 6e9 };
    await Screen.paintArcSettings(root.querySelector('#nas-tab-body'));
    await flush();
    assert.equal(slider.getAttribute('value'), '50', 'an unsaved choice stays');
    assert.match(host.querySelector('#nas-arc-val').textContent, /30%/);
  } finally {
    Screen.unmount();
  }
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
    assert.match(scope, /osobno na każdym nodzie/, 'and that every other node is still its own job');
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
    // An unchanged poll now hands the alert table no rows at all
    // (`setRowsIfChanged`), and never more than one pass.
    assert.ok(renders <= 1, `at most one render pass per poll, got ${renders}`);
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
        disk({ health: 'critical', healthReason: '835 media errors', healthReasons: [R('media_errors', { count: '835' })] }),
        disk({ diskId: 'sdz', name: 'sdz', path: '/dev/sdz', health: 'warning', healthReason: '51°C', healthReasons: [R('temperature_high', { celsius: '51' })] }),
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
    assert.match(tile.getAttribute('delta'), /^[^·]*835 bł\. nośnika/);
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
      disks: [disk({ health: 'warning', healthReason: '51°C', healthReasons: [R('temperature_high', { celsius: '51' })] })],
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

// The job-log window used to do `pre.textContent = (j.log || []).join('\n')`
// on every 1.5 s poll (POLL_JOB_MODAL_MS) even though the log only ever
// grows while a job runs — `dom-patch.js`'s append-only `paintJobLog` exists
// for exactly this. A full rewrite replaces the `<pre>`'s text node with a
// brand new one on every tick; a reader partway through an earlier line has
// their selection and scroll position quietly reset under them each time.
test('the job-log window appends the growing tail instead of rewriting the whole log', async () => {
  let calls = 0;
  const logByCall = [['line one'], ['line one', 'line two']];
  stubTransport({
    ...fixtures,
    tentaNasJobGetRequest: () => {
      const log = logByCall[Math.min(calls, logByCall.length - 1)];
      calls += 1;
      return {
        job: {
          jobId: 'j1', kind: 'smart_test', subject: 'sda', status: 'running', progressPct: 40,
          startedBy: 'admin', startedAt: '2026-09-02 09:58:00', finishedAt: null, error: null, log,
        },
      };
    },
  });
  await mountScreen({ node: LOCAL });
  try {
    Screen.openJobLog('j1');
    await flush();
    const pre = document.getElementById('nas-joblog');
    assert.ok(pre, 'the job-log window is open');
    assert.equal(pre.textContent, 'line one');
    const preAfterTick1 = pre;
    const firstTextNodeAfterTick1 = pre.firstChild;
    assert.ok(firstTextNodeAfterTick1, 'the first tick painted a text node');

    // POLL_JOB_MODAL_MS is 1500 ms; wait past it for the second real tick.
    await new Promise((r) => setTimeout(r, 1700));
    await flush();
    assert.equal(pre.textContent, 'line one\nline two', 'the tail grew');
    const preAfterTick2 = document.getElementById('nas-joblog');
    assert.ok(preAfterTick2 === preAfterTick1, 'the <pre> is the same node across ticks');
    assert.ok(pre.firstChild === firstTextNodeAfterTick1, 'the first text node is untouched — only the tail was appended');
  } finally {
    // `Screen.unmount()` marks the screen disposed, so the job log's own
    // poll loop (`isCurrent()`) stops rescheduling itself the next time its
    // pending timer fires — this MUST run even when an assertion above
    // throws, or that timer keeps firing every 1.5 s forever and the test
    // process never exits.
    Screen.unmount();
  }
});

// --- Real functionality critic 2026-09-21 + mockups n01-n10 critic ----------

// Every toast, recorded at the append (see the bulk SMART test for why the
// container itself cannot be read). Returns the list and a restore function.
function recordToasts() {
  const toasts = [];
  const append = window.Node.prototype.appendChild;
  window.Node.prototype.appendChild = function (child) {
    const kind = /(?:^|\s)toast-(\w+)/.exec(child?.className || '')?.[1];
    if (kind && kind !== 'container') toasts.push({ kind, text: child.textContent });
    return append.call(this, child);
  };
  return { toasts, restore: () => { window.Node.prototype.appendChild = append; } };
}

const diskGet = (d, extra = {}) => ({ disk: d, attributes: [], selfTests: [], history: [], alerts: [], historyDays: 30, ...extra });

// A1 / M5: an Elastic Array member's `memberOf` is the ARRAY name. n04 asked
// for a ZFS pool of that name on every poll (always failing), offered
// "Wymień dysk…" (withdrawn for arrays; it ended in a "no vdev" toast) and
// titled a card "Błędy z warstwy puli" over "Dysk nie należy do żadnej puli".
test('n04 of an Elastic Array member asks for no pool, offers no replace and names its array', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: diskGet(disk({ diskId: 'sdk', name: 'sdk', role: 'array_member', memberOf: 'produkt', arrayRole: 'data' })),
    tentaNasPoolGetRequest: () => { throw new Error('no such pool'); },
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sdk' });
    await flush();
    await flush();
    await Screen.refreshDiskDetail(root.querySelector('#nas-tab-body'));
    await flush();
    assert.equal(kinds('tentaNasPoolGetRequest').length, 0, 'no pool read, not on the draw and not on a poll');
    assert.equal(absent(root, '[data-act="replace"]'), true, 'no replace button for an array disk');
    assert.equal(root.querySelector('#nas-dd-pool-title').textContent, 'Macierz Elastic (produkt)');
    const card = root.querySelector('#nas-dd-pool');
    assert.doesNotMatch(card.textContent, /nie należy do żadnej puli/, 'the card no longer contradicts its title');
    assert.match(card.textContent, /Wymiana dysku macierzy nie jest udostępniona w tej wersji/);
    assert.deepEqual([...card.querySelectorAll('.sr .v')].map((v) => v.textContent), ['produkt', 'Dane']);
    const open = card.querySelector('[data-act="open-array"]');
    assert.equal(open.textContent, 'Zobacz macierz produkt');
    click(open);
    await flush();
    assert.equal(Screen.array, 'produkt', 'the card opens the array');
  } finally {
    Screen.unmount();
  }
});

test('n04 of another organisation\'s array disk says only that, with no name, no pool read and no replace', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: diskGet(disk({ diskId: 'sdm', name: 'sdm', role: 'other_org_array', memberOf: null, arrayRole: '' })),
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sdm' });
    await flush();
    await flush();
    assert.equal(kinds('tentaNasPoolGetRequest').length, 0);
    assert.equal(absent(root, '[data-act="replace"]'), true);
    assert.equal(root.querySelector('#nas-dd-pool-title').textContent, 'Macierz innej organizacji');
    assert.match(root.querySelector('#nas-dd-pool').textContent, /należy do macierzy Elastic innej organizacji/);
    assert.equal(absent(root, '#nas-dd-pool [data-act="open-array"]'), true, 'nothing to open: the name is not ours to know');
  } finally {
    Screen.unmount();
  }
});

// B1: a failed `ledctl` answers `active:false` with its stderr in `detail`.
test('a failed locate LED is an error toast with the node\'s reason, never a green "LED off"', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDiskLocateRequest: { method: 'ledctl', active: false, detail: 'ledctl: no enclosure for /dev/sda' },
  });
  const rec = recordToasts();
  try {
    await mountScreen({ node: LOCAL, tab: 'disks' });
    await Screen.locateDisk(disk({}), true);
    await flush();
  } finally {
    rec.restore();
    Screen.unmount();
  }
  assert.equal(rec.toasts.filter((t) => t.kind === 'success').length, 0, 'no success toast');
  assert.match(rec.toasts.filter((t) => t.kind === 'error').map((t) => t.text).join('\n'), /Dioda sda: polecenie nie powiodło się — ledctl: no enclosure/);
  assert.equal(Boolean(Screen.locateState.sda), false, 'the LED is not recorded as blinking');
});

test('a locate that worked still says so', async () => {
  stubTransport({ ...fixtures, tentaNasDiskLocateRequest: { method: 'ledctl', active: true, detail: '' } });
  const rec = recordToasts();
  try {
    await mountScreen({ node: LOCAL, tab: 'disks' });
    await Screen.locateDisk(disk({}), true);
    await flush();
  } finally {
    rec.restore();
    Screen.unmount();
  }
  assert.match(rec.toasts.filter((t) => t.kind === 'success').map((t) => t.text).join('\n'), /Dioda sda miga/);
  assert.equal(Screen.locateState.sda, true);
});

// M4: n04 shows the SMART test in flight with the shared job row and refuses
// a second one while it runs.
test('n04 shows the running SMART job with the shared job row and greys both test buttons', async () => {
  let jobs = fixtures.tentaNasJobsListRequest.jobs; // a running smart_test on sda
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: diskGet(disk({})),
    tentaNasJobsListRequest: () => ({ jobs }),
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sda' });
    await flush();
    await flush();
    const body = root.querySelector('#nas-tab-body');
    const rows = root.querySelectorAll('#nas-dd-jobs .job-row');
    assert.equal(rows.length, 1, 'the running test is listed');
    assert.equal(rows[0].dataset.job, 'j1');
    assert.match(rows[0].querySelector('.job-name').textContent, /sda/);
    assert.equal(rows[0].querySelector('tf-progress-bar').getAttribute('value'), '40', 'painted by the shared paintJobRow');
    const short = root.querySelector('[data-act="smart-short"]');
    const long = root.querySelector('[data-act="smart-long"]');
    assert.equal(short.hasAttribute('disabled') && long.hasAttribute('disabled'), true, 'no second test while one runs');
    assert.match(long.getAttribute('title'), /już trwa/);

    // Progress moves: the same row, patched.
    const row = rows[0];
    jobs = [{ ...jobs[0], progressPct: 70 }];
    Screen.clearTimers();
    await Screen.refreshDiskDetail(body);
    await flush();
    assert.ok(root.querySelector('#nas-dd-jobs .job-row') === row, 'the job row is kept across the poll');
    assert.equal(row.querySelector('tf-progress-bar').getAttribute('value'), '70');

    // The test ends: the row goes and the buttons come back.
    jobs = [];
    Screen.clearTimers();
    await Screen.refreshDiskDetail(body);
    await flush();
    assert.equal(root.querySelectorAll('#nas-dd-jobs .job-row').length, 0);
    assert.equal(root.querySelector('[data-act="smart-long"]').hasAttribute('disabled'), false, 'a new test can start again');
    // A job for ANOTHER disk does not grey this one's buttons.
    jobs = [{ ...fixtures.tentaNasJobsListRequest.jobs[0], jobId: 'j9', subject: 'sdb' }];
    Screen.clearTimers();
    await Screen.refreshDiskDetail(body);
    await flush();
    assert.equal(root.querySelector('[data-act="smart-long"]').hasAttribute('disabled'), false);
  } finally {
    Screen.unmount();
  }
});

test('n04 greys the test buttons while the drive itself reports a running self-test', async () => {
  stubTransport({
    ...fixtures,
    tentaNasJobsListRequest: { jobs: [] },
    tentaNasDiskGetRequest: diskGet(disk({}), { selfTests: [{ startedAt: null, kind: 'Extended offline', status: 'running', detail: '40% remaining', lifetimeHours: 100 }] }),
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sda' });
    await flush();
    await flush();
    assert.equal(root.querySelector('[data-act="smart-long"]').hasAttribute('disabled'), true);
  } finally {
    Screen.unmount();
  }
});

test('starting a SMART test on n04 greys the buttons at once, so a double click cannot start two', async () => {
  let release;
  const gate = new Promise((r) => { release = r; });
  stubTransport({
    ...fixtures,
    tentaNasJobsListRequest: { jobs: [] },
    tentaNasDiskGetRequest: diskGet(disk({})),
    tentaNasDiskSmartTestRequest: () => gate.then(() => ({ job: { jobId: 'j2', kind: 'smart_test', subject: 'sda', status: 'queued', log: [] } })),
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sda' });
    await flush();
    await flush();
    click(root.querySelector('[data-act="smart-long"]'));
    await flush();
    const long = root.querySelector('[data-act="smart-long"]');
    assert.equal(long.hasAttribute('disabled'), true, 'grey while the request is out');
    click(long);
    click(root.querySelector('[data-act="smart-short"]'));
    release();
    await flush();
    await flush();
    assert.equal(kinds('tentaNasDiskSmartTestRequest').length, 1, 'exactly one test was asked for');
    assert.equal(root.querySelector('[data-act="smart-long"]').hasAttribute('disabled'), true, 'and it stays grey until a poll shows the job');
  } finally {
    Screen.unmount();
  }
});

// M13: n03 problem rows carry a tone the shadow root can style, and a chip
// with the short reason ("3 realok." / "54°C").
test('n03 problem rows carry their tone class and a short reason chip', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      ...fixtures.tentaNasDisksListRequest,
      disks: [
        disk({}),
        disk({ diskId: 'sdd', name: 'sdd', health: 'warning', healthReason: '3 reallocated sectors', healthReasons: [R('reallocated', { count: '3' })] }),
        disk({ diskId: 'sdf', name: 'sdf', health: 'warning', healthReason: '54°C; 1 UDMA CRC errors (cable/backplane)', healthReasons: [R('temperature_high', { celsius: '54' }), R('crc_errors', { count: '1' })] }),
        disk({ diskId: 'sdg', name: 'sdg', health: 'critical', healthReason: '2 pending sectors', healthReasons: [R('pending_sectors', { count: '2' })] }),
        disk({ diskId: 'sdh', name: 'sdh', health: 'unknown', healthReason: 'no SMART data', healthReasons: [R('no_smart_data')] }),
      ],
    },
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks' });
    await flush();
    const table = root.querySelector('#nas-disk-table');
    const byName = (n) => table.rows.find((r) => r._disk.name === n);
    assert.equal(byName('sda')._class, '');
    assert.equal(byName('sdd')._class, 'row-warn');
    assert.equal(byName('sdg')._class, 'row-danger');
    assert.equal(byName('sdh')._class, '', 'an unknown disk is not a problem row');
    const chip = (n) => table.rowActions(byName(n), 0, () => byName(n)).querySelector('[data-role="reason"]');
    assert.equal(chip('sdd').getAttribute('label'), '3 realok.');
    assert.equal(chip('sdd').getAttribute('status'), 'warn');
    assert.equal(chip('sdf').getAttribute('label'), '54°C', 'the first symptom is the reason');
    assert.equal(chip('sdf').getAttribute('title'), '54°C; 1 UDMA CRC errors (cable/backplane)', 'the whole reason stays in the tooltip');
    assert.equal(chip('sdg').getAttribute('label'), '2 oczek. sekt.');
    assert.equal(chip('sdg').getAttribute('status'), 'err');
    assert.equal(chip('sda'), null, 'a healthy row has no chip');
    assert.equal(chip('sdh'), null);
    // The class reaches the <tr> inside the shadow root.
    await flush();
    const trs = [...table.shadowRoot.querySelectorAll('tbody tr')];
    assert.ok(trs.some((tr) => tr.classList.contains('row-warn')), 'a warn row is rendered with its class');
    assert.ok(trs.some((tr) => tr.classList.contains('row-danger')));
  } finally {
    Screen.unmount();
  }
});

// critic-round2-wave2 MAJOR 1, and backlog M1: every reason `score_health`
// and `grade_disk_health` (tentanas/disks.rs) can put FIRST on a disk arrives
// as a code, and its chip word is composed from that code — never parsed out
// of the node's English, which stays whole in the tooltip.
const SERVER_REASONS = [
  ['critical', 'SMART overall status FAILED', R('smart_failed'), 'SMART: awaria'],
  ['critical', 'last self-test failed', R('self_test_failed'), 'self-test niezaliczony'],
  ['warning', '2 pending sectors', R('pending_sectors', { count: '2' }), '2 oczek. sekt.'],
  ['warning', '4 media errors', R('media_errors', { count: '4' }), '4 bł. nośnika'],
  ['warning', 'reallocated sectors growing (3 → 8 in 7 days)', R('reallocated_growing', { from: '3', to: '8' }), 'realok. 3 → 8'],
  ['warning', '3 reallocated sectors', R('reallocated', { count: '3' }), '3 realok.'],
  ['warning', '63°C (over the 60°C limit)', R('temperature_over_limit', { celsius: '63', limit: '60' }), '63°C, ponad limit 60°C'],
  ['warning', '54°C', R('temperature_high', { celsius: '54' }), '54°C'],
  ['warning', '1 UDMA CRC errors (cable/backplane)', R('crc_errors', { count: '1' }), '1 CRC'],
  ['warning', '87% worn', R('wear', { pct: '87' }), 'zużycie 87%'],
  ['critical', 'ZFS reports this disk FAULTED', R('zfs_faulted'), 'ZFS: awaria (FAULTED)'],
  ['critical', 'ZFS reports this disk UNAVAIL', R('zfs_unavail'), 'ZFS: niedostępny (UNAVAIL)'],
];

async function reasonChipOf(health, healthReason, healthReasons) {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: { ...fixtures.tentaNasDisksListRequest, disks: [disk({ diskId: 'sdq', name: 'sdq', health, healthReason, healthReasons })] },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks' });
  await flush();
  return root.querySelector('#nas-disk-table').shadowRoot.querySelector('.row-actions [data-role="reason"]');
}

for (const [health, sentence, reason, label] of SERVER_REASONS) {
  test(`the server reason code "${reason.code}" has a localized n03 chip`, async () => {
    try {
      const chip = await reasonChipOf(health, sentence, [reason]);
      assert.ok(chip, 'a problem row carries a reason chip');
      assert.equal(chip.getAttribute('label'), label);
      assert.equal(chip.getAttribute('status'), health === 'critical' ? 'err' : 'warn');
      assert.equal(chip.getAttribute('title'), sentence, 'the node\'s sentence stays in the tooltip');
    } finally {
      Screen.unmount();
    }
  });
}

test('a faulted disk that SMART also complains about leads with the ZFS state', async () => {
  try {
    const chip = await reasonChipOf('critical', 'ZFS reports this disk FAULTED; 3 reallocated sectors', [R('zfs_faulted'), R('reallocated', { count: '3' })]);
    assert.equal(chip.getAttribute('label'), 'ZFS: awaria (FAULTED)');
    assert.equal(chip.getAttribute('title'), 'ZFS reports this disk FAULTED; 3 reallocated sectors');
  } finally {
    Screen.unmount();
  }
});

// The chip reads the CODE, never the sentence: a sentence that looks known but
// comes with an unknown code (or with none, from an older node) gets the
// translated grade, with the sentence whole in the tooltip.
test('an unknown reason code falls back to the translated grade, the sentence only in the tooltip', async () => {
  for (const [health, grade] of [['warning', 'Uwaga'], ['critical', 'Awaria']]) {
    for (const codes of [[R('spindle_stall'), R('temperature_high', { celsius: '54' })], undefined]) {
      try {
        const chip = await reasonChipOf(health, '54°C; spindle motor stalled', codes);
        assert.equal(chip.getAttribute('label'), grade, `${health} ${JSON.stringify(codes)}`);
        assert.equal(chip.getAttribute('title'), '54°C; spindle motor stalled');
      } finally {
        Screen.unmount();
      }
    }
  }
});

// The n04 identification chip names the symptom through the same words, and
// falls back the same way.
test('the n04 health chip localizes the server reason and never prints an unknown one', async () => {
  for (const [reason, codes, label] of [
    ['ZFS reports this disk UNAVAIL; 2 pending sectors', [R('zfs_unavail'), R('pending_sectors', { count: '2' })], 'Awaria: ZFS: niedostępny (UNAVAIL)'],
    ['spindle motor stalled', [R('spindle_stall')], 'Awaria'],
  ]) {
    stubTransport({
      ...fixtures,
      tentaNasDiskGetRequest: {
        disk: disk({ health: 'critical', healthReason: reason, healthReasons: codes }),
        attributes: [], selfTests: [], history: [], alerts: [], historyDays: 30,
      },
    });
    try {
      const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sda' });
      await flush();
      await flush();
      const chip = root.querySelector('#nas-dd-health');
      assert.equal(chip.getAttribute('label'), label, reason);
      assert.equal(chip.getAttribute('title'), reason, 'the whole sentence is the tooltip');
    } finally {
      Screen.unmount();
    }
  }
});

test('the new reason words are translated in every locale, with the same placeholders', () => {
  const root = new URL('../../', import.meta.url);
  const words = {};
  for (const lang of ['pl', 'en', 'de', 'es', 'fr']) {
    words[lang] = JSON.parse(readFileSync(new URL(`i18n/${lang}.json`, root), 'utf8')).tentanas.disks;
  }
  for (const key of ['reason_temp_over', 'reason_zfs_faulted', 'reason_zfs_unavail']) {
    const values = Object.values(words).map((w) => w[key]);
    assert.ok(values.every((v) => typeof v === 'string' && v.trim()), `${key} exists everywhere`);
    assert.equal(new Set(values).size, values.length, `${key} is really translated, not copied: ${values}`);
  }
  for (const lang of Object.keys(words)) {
    assert.match(words[lang].reason_temp_over, /\{t\}.*\{limit\}/, lang);
  }
});

// The n04 "why" box listed the node's English sentences verbatim — the same
// defect as the chip (critic-round2-wave2 MAJOR 1). Every symptom now goes
// through the chip's words; one this build cannot name is left out of the
// text, the grade stands in when none is known, and the node's sentence is
// the tooltip. The box is patched, never rebuilt.
test('the n04 "why" box lists every symptom in the reader\'s language', async () => {
  let reason = 'ZFS reports this disk FAULTED; 3 reallocated sectors; 63°C (over the 60°C limit)';
  let codes = [R('zfs_faulted'), R('reallocated', { count: '3' }), R('temperature_over_limit', { celsius: '63', limit: '60' })];
  let health = 'critical';
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: () => ({
      disk: disk({ health, healthReason: reason, healthReasons: codes }),
      attributes: [], selfTests: [], history: [], alerts: [], historyDays: 30,
    }),
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sda' });
    await flush();
    await flush();
    const why = root.querySelector('#nas-dd-why');
    const body = root.querySelector('#nas-tab-body');
    assert.equal(why.textContent, 'ZFS: awaria (FAULTED); 3 realok.; 63°C, ponad limit 60°C');
    assert.equal(why.getAttribute('title'), reason);

    reason = 'spindle motor stalled';
    codes = [R('spindle_stall')];
    await Screen.refreshDiskDetail(body);
    await flush();
    assert.ok(root.querySelector('#nas-dd-why') === why, 'the box is patched, not rebuilt');
    assert.equal(why.textContent, 'Awaria', 'an unknown symptom falls back to the grade');
    assert.equal(why.getAttribute('title'), 'spindle motor stalled', 'and stays in the tooltip only');

    reason = 'spindle motor stalled; 2 pending sectors';
    codes = [R('spindle_stall'), R('pending_sectors', { count: '2' })];
    await Screen.refreshDiskDetail(body);
    await flush();
    assert.equal(why.textContent, '2 oczek. sekt.', 'a known symptom is named, the unknown one is not printed');

    health = 'unknown';
    reason = 'no SMART data';
    codes = [R('no_smart_data')];
    await Screen.refreshDiskDetail(body);
    await flush();
    assert.equal(why.textContent, 'brak danych SMART');

    health = 'ok';
    reason = '';
    codes = [];
    await Screen.refreshDiskDetail(body);
    await flush();
    assert.equal(why.textContent, 'Wszystkie źródła (SMART, liczniki puli, błędy I/O kernela) są czyste.');
    assert.equal(why.hasAttribute('title'), false);
  } finally {
    Screen.unmount();
  }
});

// The n02 disk-health tile named up to three problem disks with the node's
// English reasons. They read in the reader's language now; the node's
// sentences are the tile's tooltip, and an unknown one never reaches the text.
test('the n02 disk-health tile names its problem disks in the reader\'s language', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      ...fixtures.tentaNasDisksListRequest,
      disks: [
        disk({ health: 'critical', healthReason: 'ZFS reports this disk UNAVAIL; 835 media errors', healthReasons: [R('zfs_unavail'), R('media_errors', { count: '835' })] }),
        disk({ diskId: 'sdz', name: 'sdz', path: '/dev/sdz', health: 'warning', healthReason: 'spindle motor stalled', healthReasons: [R('spindle_stall')] }),
      ],
    },
  });
  const root = await mountScreen({ node: LOCAL });
  await flush();
  try {
    const tile = root.querySelector('#nas-ov-kpi [data-kpi="disks"]');
    assert.equal(tile.getAttribute('delta'), 'sda: ZFS: niedostępny (UNAVAIL); 835 bł. nośnika · sdz: Uwaga');
    assert.equal(tile.getAttribute('title'), 'sda: ZFS reports this disk UNAVAIL; 835 media errors · sdz: spindle motor stalled');
    assert.doesNotMatch(tile.getAttribute('delta'), /media errors|spindle|reports/);
  } finally {
    Screen.unmount();
  }
});

// critic-round2-wave2 MINOR 10: `rowActionsKey` used to carry the chip's label,
// so every 1 °C step of a warm disk destroyed and rebuilt its row buttons (and
// the focus on one of them). The key now covers what the buttons render and
// close over; the chip's words are patched onto the kept element.
test('a temperature step patches the reason chip and keeps the row buttons', async () => {
  let reason = '54°C';
  let codes = [R('temperature_high', { celsius: '54' })];
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: () => ({
      ...fixtures.tentaNasDisksListRequest,
      disks: [disk({ diskId: 'sdf', name: 'sdf', health: 'warning', healthReason: reason, healthReasons: codes })],
    }),
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks' });
    await flush();
    const shadow = root.querySelector('#nas-disk-table').shadowRoot;
    const wrap = shadow.querySelector('.row-actions');
    const button = wrap.querySelector('[data-act="details"]');
    const chip = wrap.querySelector('[data-role="reason"]');
    assert.equal(chip.getAttribute('label'), '54°C');

    reason = '55°C; 1 UDMA CRC errors (cable/backplane)';
    codes = [R('temperature_high', { celsius: '55' }), R('crc_errors', { count: '1' })];
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();
    assert.ok(shadow.querySelector('.row-actions') === wrap, 'the very same actions element');
    assert.ok(shadow.querySelector('[data-act="details"]') === button, 'and the same button under the cursor');
    assert.ok(shadow.querySelector('[data-role="reason"]') === chip, 'and the same chip');
    assert.equal(chip.getAttribute('label'), '55°C', 'whose words follow the poll');
    assert.equal(chip.getAttribute('title'), '55°C; 1 UDMA CRC errors (cable/backplane)');

    reason = '63°C (over the 60°C limit)';
    codes = [R('temperature_over_limit', { celsius: '63', limit: '60' })];
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();
    assert.ok(shadow.querySelector('.row-actions') === wrap);
    assert.equal(chip.getAttribute('label'), '63°C, ponad limit 60°C');
  } finally {
    Screen.unmount();
  }
});

// A `_class` lands on a <tr> inside tf-table's shadow root, which adopts only
// controls.css and the scoped cell sheets: a rule in tentanas.css can never
// reach it. `row-danger` was once emitted and styled nowhere at all.
test('every row class TentaNas puts on a table row is styled in the scoped cell sheet', () => {
  const root = new URL('../../', import.meta.url);
  const sources = [readFileSync(new URL('js/modules/tentanas.js', root), 'utf8')];
  const classes = new Set();
  for (const text of sources) {
    // The VALUES a `_class` expression can take: a literal right after the
    // key or after a `?` (the compared operands, `'critical'`, are not).
    for (const m of text.matchAll(/_class:([^\n]*)/g)) {
      for (const lit of `?${m[1]}`.matchAll(/\?\s*'([a-z][\w-]*)'/g)) classes.add(lit[1]);
    }
  }
  assert.ok(classes.has('row-warn') && classes.has('row-danger'), `parsed the row classes: ${[...classes]}`);
  const sheet = readFileSync(new URL('css/tentanas-cells.css', root), 'utf8');
  const unstyled = [...classes].filter((c) => !new RegExp(`tr\\.${c}(?![\\w-])`).test(sheet));
  assert.deepEqual(unstyled, [], 'css/tentanas-cells.css styles every row class on a <tr>');
});

// B2: an unreachable node's session count is unknown, not zero.
test('an unreachable node shows "—" sessions in the fleet resources, not 0', async () => {
  stubTransport({
    ...fixtures,
    tentaNasSharesListRequest: (payload, options) => {
      if (options.targetNodeId === REMOTE) throw new Error('mesh timeout');
      return fixtures.tentaNasSharesListRequest;
    },
  });
  try {
    const root = await mountScreen();
    await flush();
    await flush();
    const rows = root.querySelector('#nas-fleet-res-table').rows;
    assert.equal(rows[0].sessions, 14);
    assert.equal(rows[1].sessions, '—');
  } finally {
    Screen.unmount();
  }
});

// B3: "services running" holds only when every expected service runs.
test('the fleet services chip names each service that is down, and an unreachable node as unknown', async () => {
  const smbUp = { protocol: 'smb', installed: true, running: true, version: '4.21', configPath: '', detail: '' };
  const nfsDown = { protocol: 'nfs', installed: true, running: false, version: null, configPath: '', detail: '' };
  const nfsAbsent = { protocol: 'nfs', installed: false, running: false, version: null, configPath: '', detail: '' };
  let remote = { shares: [], services: [smbUp, nfsDown], users: [], mountRoot: '/mnt/tentanas' };
  stubTransport({
    ...fixtures,
    tentaNasSharesListRequest: (payload, options) => {
      if (options.targetNodeId === REMOTE) {
        if (!remote) throw new Error('mesh timeout');
        return remote;
      }
      return { ...fixtures.tentaNasSharesListRequest, services: [smbUp, nfsAbsent] };
    },
  });
  const chip = (root) => [...root.querySelectorAll('#nas-fleet-chips tf-chip')][1];
  try {
    let root = await mountScreen();
    await flush();
    await flush();
    assert.equal(chip(root).getAttribute('label'), 'Usługi nieaktywne: vega: NFS', 'one dead service is enough, and it is named');
    assert.equal(chip(root).getAttribute('status'), 'warn');
    Screen.unmount();

    // Everything installed runs; NFS that is neither installed nor used is not expected.
    remote = { shares: [], services: [smbUp, nfsAbsent], users: [], mountRoot: '/mnt/tentanas' };
    root = await mountScreen();
    await flush();
    await flush();
    assert.equal(chip(root).getAttribute('label'), 'Usługi aktywne');
    assert.equal(chip(root).getAttribute('status'), 'ok');
    Screen.unmount();

    remote = null;
    root = await mountScreen();
    await flush();
    await flush();
    assert.equal(chip(root).getAttribute('label'), 'Usługi: brak odpowiedzi z vega', 'a silent node is not "active"');
    assert.equal(chip(root).getAttribute('status'), 'warn');
  } finally {
    Screen.unmount();
  }
});

// B4: no pools and no arrays is "—", never the sum of raw disk sizes.
test('the capacity tile of a node with no pools says "—", not the sum of its raw disks', async () => {
  stubTransport({ ...fixtures, tentaNasPoolsListRequest: { pools: [], freeDisks: [] }, tentaNasElasticArraysListRequest: { arrays: [] } });
  try {
    const root = await mountScreen({ node: LOCAL });
    await flush();
    const tile = root.querySelector('#nas-ov-kpi [data-kpi="pools"]');
    assert.equal(tile.getAttribute('value'), '—');
    assert.equal(tile.getAttribute('delta'), 'brak pul i macierzy');
  } finally {
    Screen.unmount();
  }
});

// B6: the fleet header version is checked on every node, not read off the first.
test('the fleet header names each version and its nodes when the fleet runs more than one', async () => {
  stubTransport({
    ...fixtures,
    tentaNasEnvironmentRequest: (payload, options) => ({
      environment: { ...environment, elevation: { ...environment.elevation, coreVersion: options.targetNodeId === REMOTE ? '1.5.0' : '1.4.0' } },
    }),
  });
  try {
    const root = await mountScreen();
    await flush();
    await flush();
    const sub = root.querySelector('.tf-detail-header .d-sub').textContent;
    assert.match(sub, /TentaNas: 1\.4\.0 \(orion\) · 1\.5\.0 \(vega\)/);
    assert.equal(kinds('tentaNasEnvironmentRequest').length, 2, 'one probe per supported node');
  } finally {
    Screen.unmount();
  }
});

// m26: the pool detail's "Pule › tank" is the tail of the shell's one bar.
// Wave-4 critic minor 6: the shell cleared the tail for an ARRAY detail
// (only a ZFS pool was exempt), and the array detail wrote it back — the bar
// was rebuilt twice on every draw of the array.
test('the Elastic Array detail writes its breadcrumb once, and a redraw rewrites nothing', async () => {
  const array = { name: 'media', kind: 'elastic-array', state: 'active', enabled: true, filesystem: 'xfs', unionPath: '/mnt/media', dataDisks: [], parityDisks: [], protection: { status: 'unprotected' }, snapraid: {} };
  stubTransport({ ...fixtures, tentaNasElasticArrayGetRequest: { array } });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'pools', array: 'media' });
    await flush();
    const bar = root.querySelector('#nas-crumbs');
    assert.deepEqual([...bar.querySelectorAll('.tf-breadcrumb-item')].map((a) => a.textContent), ['TentaNas', 'orion', 'Pule', 'media']);
    const items = [...bar.querySelectorAll('tf-breadcrumb-item')];
    const tails = [];
    const real = Screen.setCrumbTail;
    Screen.setCrumbTail = function spy(tail) { tails.push(tail); return real.call(this, tail); };
    try {
      Screen.drawTab();
      await flush();
    } finally {
      Screen.setCrumbTail = real;
    }
    assert.ok(tails.every((t) => t.length > 0), `the tail is never cleared on the way: ${JSON.stringify(tails)}`);
    const after = [...bar.querySelectorAll('tf-breadcrumb-item')];
    assert.equal(after.length, items.length);
    assert.ok(after.every((el, i) => el === items[i]), 'the bar keeps its items');
  } finally {
    Screen.unmount();
  }
});

// Wave-4 critic minor 7: n19 has no heading and no back button of its own —
// the shell's one breadcrumb names the target under "Udostępnianie".
test('the block target detail names its tail in the shell breadcrumb, and "Udostępnianie" walks back', async () => {
  const target = {
    targetId: 't1', name: 'vm-store', protocol: 'iscsi', wwn: 'iqn.2026-09.local.tentaflow:orion.vm-store', enabled: true,
    luns: [], portals: [], auth: { method: 'none' }, initiators: [], portGroups: [], sessions: 0, sessionsKnown: true, state: 'active', stateDetail: '',
  };
  stubTransport({
    ...fixtures,
    tentaNasTargetGetRequest: { target, sessions: [], configPreview: '' },
    tentaNasTargetsListRequest: { targets: [target], services: [], capabilities: null },
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'shares', target: 't1' });
    await flush();
    await flush();
    const bar = root.querySelector('#nas-crumbs');
    assert.deepEqual([...bar.querySelectorAll('.tf-breadcrumb-item')].map((a) => a.textContent), ['TentaNas', 'orion', 'Udostępnianie', 'vm-store']);
    assert.equal(root.querySelector('.nas-target-detail [data-act="back"]'), null, 'no second way back');
    click([...bar.querySelectorAll('a.tf-breadcrumb-item')].find((a) => a.textContent.trim() === 'Udostępnianie'));
    await flush();
    assert.equal(Screen.targetId, null, '"Udostępnianie" returns to the list');
    assert.equal(Screen.tab, 'shares');
    assert.deepEqual([...bar.querySelectorAll('.tf-breadcrumb-item')].map((a) => a.textContent), ['TentaNas', 'orion']);
  } finally {
    Screen.unmount();
  }
});

test('the pool detail puts its tail into the one shell breadcrumb, and "Pule" walks back', async () => {
  stubTransport({ ...fixtures, tentaNasPoolGetRequest: { pool, properties: [], datasets: [], alerts: [], history: [] } });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'pools', pool: 'tank' });
    await flush();
    await flush();
    const bars = [...root.querySelectorAll('tf-breadcrumb')];
    assert.equal(bars.length, 1, 'one bar on the screen');
    assert.deepEqual([...bars[0].querySelectorAll('.tf-breadcrumb-item')].map((a) => a.textContent), ['TentaNas', 'orion', 'Pule', 'tank']);
    const links = [...bars[0].querySelectorAll('a.tf-breadcrumb-item')];
    assert.equal(links[1].getAttribute('href'), `#/tentanas?node=${LOCAL}`, 'the node level links to its dashboard');
    click(links[2]);
    await flush();
    assert.equal(Screen.pool, null, '"Pule" returns to the pool list');
    assert.equal(Screen.tab, 'pools');
    assert.deepEqual([...root.querySelector('tf-breadcrumb').querySelectorAll('.tf-breadcrumb-item')].map((a) => a.textContent), ['TentaNas', 'orion']);
  } finally {
    Screen.unmount();
  }
});

// Icons the TentaNas screens hand to components that read /img/icons.svg
// (tf-stat-card, tf-empty-state) must exist there, or they render blank —
// `layers` and `grid-2x2` did.
test('every icon TentaNas gives a stat card or an empty state exists in /img/icons.svg', () => {
  const root = new URL('../../', import.meta.url);
  const dir = new URL('js/modules/tentanas/', root);
  const files = [new URL('js/modules/tentanas.js', root)];
  for (const f of readdirSync(dir)) if (f.endsWith('.js') && !f.endsWith('.test.js') && !f.startsWith('_')) files.push(new URL(f, dir));
  const names = new Set();
  for (const f of files) {
    const text = readFileSync(f, 'utf8');
    for (const m of text.matchAll(/<tf-empty-state\b[^>]*\bicon="([\w-]+)"/g)) names.add(m[1]);
    // Stat-card attribute objects: `label:` … `icon: '…'` on one line.
    for (const line of text.split('\n')) {
      if (!/\blabel:/.test(line)) continue;
      for (const m of line.matchAll(/\bicon: '([\w-]+)'/g)) names.add(m[1]);
    }
  }
  assert.ok(names.has('layers') && names.has('grid-2x2'), `parsed the icon names: ${[...names]}`);
  const sprite = readFileSync(new URL('img/icons.svg', root), 'utf8');
  const missing = [...names].filter((n) => !sprite.includes(`id="icon-${n}"`)).sort();
  assert.deepEqual(missing, []);
});

// critic-round2-wave2-iter2 MAJOR A, and backlog M1: the replacement advice
// printed the node's `advice.reason` — English, with the disk's whole health
// reason in it (`replacement_advice`, tentanas/disks.rs). The node now sends
// the advice's reasons as codes (`advice.reasons`) and the text is worded
// from them (`replacementAdviceText`, format.js); the node's sentence is only
// the tooltip. The fixtures are what that function sends, code for code.
const FAULTED_DISK = { diskId: 'sde', name: 'sde', health: 'critical', healthReason: 'ZFS reports this disk FAULTED; 3 reallocated sectors', healthReasons: [R('zfs_faulted'), R('reallocated', { count: '3' })], memberOf: 'tank', role: 'data' };
const FAULTED_ADVICE = {
  diskId: 'sde', name: 'sde', severity: 'urgent',
  reason: 'critical for 3 days; ZFS reports this disk FAULTED; 3 reallocated sectors',
  reasons: [R('unhealthy_for_days', { health: 'critical', days: '3' }), R('zfs_faulted'), R('reallocated', { count: '3' })],
  warningDays: 3, reallocated: 3, reallocatedWeekAgo: 3, memberOf: 'tank', spareAvailable: false,
};
const GROWING_DISK = { diskId: 'sdd', name: 'sdd', health: 'warning', healthReason: 'reallocated sectors growing (3 → 8 in 7 days)', healthReasons: [R('reallocated_growing', { from: '3', to: '8' })], memberOf: 'tank', role: 'data', reallocatedSectors: 8 };
const GROWING_ADVICE = {
  diskId: 'sdd', name: 'sdd', severity: 'urgent',
  reason: 'reallocated sectors grew from 3 to 8 in the last 7 days; reallocated sectors growing (3 → 8 in 7 days)',
  reasons: [R('reallocated_grew', { from: '3', to: '8' })],
  warningDays: 0, reallocated: 8, reallocatedWeekAgo: 3, memberOf: 'tank', spareAvailable: true,
};
const OTHER_DISK = { diskId: 'sdf', name: 'sdf', health: 'warning', healthReason: '3 reallocated sectors', healthReasons: [R('reallocated', { count: '3' })], memberOf: 'tank', role: 'data' };
const OTHER_ADVICE = {
  diskId: 'sdf', name: 'sdf', severity: 'retire_soon',
  reason: 'firmware recall for this model', reasons: [R('reallocated', { count: '3' })],
  warningDays: 9, reallocated: 3, reallocatedWeekAgo: 3, memberOf: 'tank', spareAvailable: false,
};
const ENGLISH_ADVICE = /ZFS reports|reallocated sectors|critical for|grew from|firmware recall/;

test('n03 replacement advice reads in the reader\'s language, the node\'s sentence only in the tooltip', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: {
      ...fixtures.tentaNasDisksListRequest,
      disks: [disk({}), disk(FAULTED_DISK), disk(GROWING_DISK), disk(OTHER_DISK)],
      advice: [FAULTED_ADVICE, GROWING_ADVICE, OTHER_ADVICE],
    },
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks' });
    await flush();
    const row = (id) => root.querySelector(`#nas-disk-advice [data-advice="${id}"]`);
    const reason = (id) => row(id).querySelector('[data-role="advice-reason"]');
    const kind = (id) => row(id).querySelector('tf-chip').getAttribute('label');

    // Whole phrases, not the n03 chip abbreviations: the advice is a sentence.
    assert.equal(reason('sde').textContent, 'Awaria od 3 dni; ZFS wyłączył dysk jako uszkodzony (FAULTED); 3 realokowane sektory');
    assert.equal(reason('sde').getAttribute('title'), FAULTED_ADVICE.reason);
    assert.equal(kind('sde'), 'pilne');

    assert.equal(reason('sdd').textContent, 'realokacje wzrosły z 3 do 8 w 7 dni', 'the growth is said once');
    assert.equal(reason('sdd').getAttribute('title'), GROWING_ADVICE.reason);

    assert.equal(reason('sdf').textContent, 'węzeł zaleca wymianę tego dysku', 'an unknown advice kind reads as the generic recommendation');
    assert.equal(reason('sdf').getAttribute('title'), OTHER_ADVICE.reason);
    assert.equal(kind('sdf'), 'zalecenie', 'and its chip is a word, not a raw key');

    assert.ok(!ENGLISH_ADVICE.test(root.querySelector('#nas-disk-advice').textContent), 'no English advice is visible text');
  } finally {
    Screen.unmount();
  }
});

test('n04 replacement advice reads in the reader\'s language, the node\'s sentence only in the tooltip', async () => {
  let current = { disk: FAULTED_DISK, advice: FAULTED_ADVICE };
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: () => ({
      disk: disk(current.disk), advice: current.advice,
      attributes: [], selfTests: [], history: [], alerts: [], historyDays: 30,
    }),
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sde' });
    await flush();
    await flush();
    const box = () => root.querySelector('#nas-dd-advice .wizard-warning');
    assert.equal(box().textContent, 'Wymień ten dysk teraz: Awaria od 3 dni; ZFS wyłączył dysk jako uszkodzony (FAULTED); 3 realokowane sektory. brak spare w puli — przygotuj dysk zastępczy.');
    assert.doesNotMatch(box().textContent, /\.\./, 'no abbreviation runs into the full stop');
    assert.equal(box().getAttribute('title'), FAULTED_ADVICE.reason);
    assert.ok(box().classList.contains('danger'));

    const body = root.querySelector('#nas-tab-body');
    current = { disk: GROWING_DISK, advice: GROWING_ADVICE };
    await Screen.refreshDiskDetail(body);
    await flush();
    assert.equal(box().textContent, 'Wymień ten dysk teraz: realokacje wzrosły z 3 do 8 w 7 dni. pula ma spare gotowy do podmiany.');
    assert.equal(box().getAttribute('title'), GROWING_ADVICE.reason);

    current = { disk: OTHER_DISK, advice: OTHER_ADVICE };
    await Screen.refreshDiskDetail(body);
    await flush();
    assert.equal(box().textContent, 'Węzeł zaleca wymianę tego dysku. brak spare w puli — przygotuj dysk zastępczy.');
    assert.equal(box().getAttribute('title'), OTHER_ADVICE.reason);
    assert.ok(!ENGLISH_ADVICE.test(root.querySelector('#nas-dd-advice').textContent));
  } finally {
    Screen.unmount();
  }
});

// C3: an Elastic Array member gets `spareAvailable: false` from the node like
// a pool disk with no spare, and was told "Wymień ten dysk teraz … brak spare
// w puli — przygotuj dysk zastępczy" — but replacing an array disk does not
// exist in this version. It is told what applies instead, on n03 and n04.
const ARRAY_DISK = { diskId: 'sdq', name: 'sdq', health: 'critical', healthReason: '3 pending sectors', healthReasons: [R('pending_sectors', { count: '3' })], memberOf: 'media', role: 'array_member', arrayRole: 'data' };
const ARRAY_ADVICE = {
  diskId: 'sdq', name: 'sdq', severity: 'urgent',
  reason: 'critical for 3 days; 3 pending sectors',
  reasons: [R('unhealthy_for_days', { health: 'critical', days: '3' }), R('pending_sectors', { count: '3' })],
  warningDays: 3, reallocated: 0, reallocatedWeekAgo: 0, memberOf: 'media', spareAvailable: false,
};

test('C3: an Elastic Array disk is never told to be replaced or to get a spare (n03 and n04)', async () => {
  let listed = { disks: [disk(ARRAY_DISK)], advice: [ARRAY_ADVICE] };
  stubTransport({
    ...fixtures,
    tentaNasDisksListRequest: () => ({ ...fixtures.tentaNasDisksListRequest, ...listed }),
    tentaNasDiskGetRequest: () => ({
      disk: disk(ARRAY_DISK), advice: ARRAY_ADVICE, attributes: [], selfTests: [], history: [], alerts: [], historyDays: 30,
    }),
  });
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks' });
    await flush();
    const card = () => root.querySelector('#nas-disk-advice');
    const spare = (id) => card().querySelector(`[data-advice="${id}"] [data-role="advice-spare"]`).textContent;
    assert.equal(spare('sdq'), 'macierz Elastic — wymiana dysku niedostępna w tej wersji');
    assert.match(card().querySelector('.section-card-head .title').textContent, /Dyski macierzy Elastic wymagające uwagi/);
    assert.doesNotMatch(card().textContent, /spare|Zalecana wymiana|Wymień/, 'no replacement wording for an array disk');
    const arrayRow = card().querySelector('[data-advice="sdq"]');

    // Wave-4 critic minor 8: advice this build cannot word on an ARRAY row
    // is "a problem", never "the node recommends replacing this disk", and
    // its chip is not the "zaplanuj" of a disk one can replace.
    listed = { disks: [disk(ARRAY_DISK)], advice: [{ ...ARRAY_ADVICE, severity: 'advice', reasons: [R('spindle_stall')] }] };
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();
    const vague = card().querySelector('[data-advice="sdq"]');
    assert.equal(vague.querySelector('[data-role="advice-reason"]').textContent, 'węzeł zgłasza problem z tym dyskiem');
    assert.equal(vague.querySelector('tf-chip').getAttribute('label'), 'obserwuj');
    assert.doesNotMatch(card().textContent, /zaleca wymianę|zaplanuj/);
    assert.ok(vague !== arrayRow, 'the changed row is rebuilt');
    listed = { disks: [disk(ARRAY_DISK)], advice: [ARRAY_ADVICE] };
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();

    // Beside a pool disk the card is about replacement again, and only the
    // pool disk's row talks about a spare.
    listed = { disks: [disk(ARRAY_DISK), disk(FAULTED_DISK)], advice: [ARRAY_ADVICE, FAULTED_ADVICE] };
    const before = { card: card().firstElementChild, sdq: card().querySelector('[data-advice="sdq"]') };
    await Screen.refreshDisks(root.querySelector('#nas-tab-body'));
    await flush();
    assert.match(card().querySelector('.section-card-head .title').textContent, /Zalecana wymiana dysku/);
    assert.equal(spare('sdq'), 'macierz Elastic — wymiana dysku niedostępna w tej wersji');
    assert.equal(spare('sde'), 'brak spare w puli — przygotuj dysk zastępczy');
    // Wave-4 critic minor 14: a second disk joining the card patches the
    // card — its title and one new row — and rebuilds nothing else.
    assert.ok(card().firstElementChild === before.card, 'the card is the same node');
    assert.ok(card().querySelector('[data-advice="sdq"]') === before.sdq, 'the unchanged row is the same node');
    card().querySelector('[data-advice="sde"] [data-act="advice-open"]').click();
    assert.equal(Screen.diskId, 'sde', 'a row added later still opens its disk');
  } finally {
    Screen.unmount();
  }
  try {
    const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sdq' });
    await flush();
    await flush();
    const box = root.querySelector('#nas-dd-advice .wizard-warning');
    assert.equal(box.textContent, 'Ten dysk macierzy Elastic wymaga uwagi: Awaria od 3 dni; 3 sektory czekają na realokację. Wymiana dysku macierzy nie jest dostępna w tej wersji.');
    assert.doesNotMatch(box.textContent, /spare|Wymień/);
    assert.equal(box.getAttribute('title'), ARRAY_ADVICE.reason);
  } finally {
    Screen.unmount();
  }
});
