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
      node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, health: 'warning', disksWarning: 1, elevationMode: 'unarmed', poolsTotal: 0, features: [] }),
      node({ nodeId: MAC, nodeName: 'mini', isLocal: false, instanceStatus: 'unsupported', osName: 'macOS', disksTotal: 0, poolsTotal: 0, features: [] }),
    ],
  },
  tentaNasEnvironmentRequest: { environment },
  tentaNasElevationPlanRequest: { plan: { helperSource: '/opt/tentaflow/tentanas-helper', helperSourcePresent: true, helperPath: '/usr/local/libexec/tentanas-helper', sudoersPath: '/etc/sudoers.d/tentanas', sudoersLine: 'tentaflow ALL=(root) NOPASSWD: /usr/local/libexec/tentanas-helper', coreUser: 'tentaflow', coreVersion: '1.4.0', commands: [['install', '-m', '0755', '/opt/tentaflow/tentanas-helper', '/usr/local/libexec/tentanas-helper']] } },
  tentaNasDisksListRequest: {
    disks: [disk({}), disk({ diskId: 'nvme0n1', name: 'nvme0n1', path: '/dev/nvme0n1', kind: 'nvme', model: 'Samsung 980', serial: 'S-1', health: 'warning', healthReason: 'pending sectors', wearPct: 12, rotational: false })],
    telemetry: { sampledAt: '2026-09-02 10:00:00', smartReadAt: '2026-09-02 09:59:00', smartState: 'live', detail: '' },
  },
  tentaNasJobsListRequest: { jobs: [{ jobId: 'j1', kind: 'smart_test', subject: 'sda', status: 'running', progressPct: 40, startedBy: 'admin', startedAt: '2026-09-02 09:58:00', finishedAt: null, error: null, log: ['started'] }] },
  tentaNasAlertsListRequest: { alerts: [] },
  tentaNasPoolsListRequest: { pools: [pool], freeDisks: [disk({})] },
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

  const chips = root.querySelector('#nas-disk-filters').filters.map((f) => f.label);
  assert.deepEqual(chips, ['Wszystkie 2', 'HDD 1', 'SSD · NVMe 1', 'Problemy 1', 'Wolne 2']);
  assert.ok(root.querySelector('#nas-disk-table').hasAttribute('selectable'), 'rows are selectable');
  assert.match(root.querySelector('.legend-strip').textContent, /brak symptomów we wszystkich źródłach/);

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
  assert.equal(document.querySelector('tf-window.nas-modal'), null, 'no prompt opened');

  Screen.environment = { ...environment, elevation: { ...environment.elevation, mode: 'unarmed', helperState: 'absent' } };
  const pending = Screen.withSudo(async (password) => { seen = password; return {}; }, 'x');
  await flush();
  const prompt = document.querySelector('tf-window.nas-modal');
  assert.ok(prompt, 'prompt opened for an unarmed channel');
  prompt.querySelector('#nas-sudo-pass').value = 'hunter2';
  prompt.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' } }));
  await pending;
  assert.equal(seen, 'hunter2');
  prompt.remove();
  Screen.unmount();
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

  click(tiles[1]);
  await flush();
  assert.equal(Screen.tab, 'disks');
  assert.equal(Screen.diskFilter, 'problems');
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
  assert.equal(root.querySelector('#nas-disk-io-chart'), null, 'the extra 24 h transfer chart is gone');
  assert.equal(root.querySelector('#nas-disk-temp-chart polyline.tf-chart__series-line').getAttribute('points').trim().split(' ').length, 3, 'three temperature samples plotted');
  // One reallocation sample only → no chart, the empty note instead.
  assert.equal(root.querySelector('#nas-disk-realloc-chart tf-line-chart'), null);
  assert.ok(root.querySelector('#nas-disk-realloc-chart .muted'));
  assert.match(root.querySelector('.id-badge').textContent, /hdd/);
  assert.ok(root.querySelector('[data-act="copy-serial"]'), 'the serial can be copied');
  Screen.unmount();
});

test('a pooled disk shows the vdev error counters and opens the replace wizard', async () => {
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: {
      disk: disk({ diskId: 'sdd', name: 'sdd', serial: 'ZR9AB12K', role: 'pool', memberOf: 'tank', health: 'warning', healthReason: '3 nowe realokowane sektory w 7 dni' }),
      attributes: [], selfTests: [], history: [], alerts: [], historyDays: 30,
    },
    tentaNasPoolGetRequest: { pool, properties: [], datasets: [], alerts: [], history: [] },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sdd' });
  await flush();
  await flush();
  assert.equal(kinds('tentaNasPoolGetRequest')[0].payload.name, 'tank');
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
