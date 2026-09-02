// =============================================================================
// File: modules/tentanas.test.js
// Description: The TentaNas screen against a stubbed transport: the fleet grid
// lists every node and only ready nodes open, the node view forwards each
// request to the selected node (envelope target) and leaves the local node
// unforwarded, and the disks tab renders one row per disk with the filters
// narrowing the set. Runs under happy-dom with the `/js/` resolver hook.
// =============================================================================

import { window } from '../sdk-runtime/_dom-test-harness.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { register } from 'node:module';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { dirname, resolve as pathResolve } from 'node:path';

const here = fileURLToPath(import.meta.url);
const WWW_ROOT = pathResolve(dirname(here), '..', '..');
const hookSource = `
  const WWW_ROOT_URL = ${JSON.stringify(pathToFileURL(WWW_ROOT + '/').href)};
  export async function resolve(specifier, context, nextResolve) {
    if (specifier.startsWith('/js/')) {
      return { url: new URL('.' + specifier, WWW_ROOT_URL).href, shortCircuit: true };
    }
    return nextResolve(specifier, context);
  }
`;
register('data:text/javascript,' + encodeURIComponent(hookSource), import.meta.url);

// tf-tabs measures itself with a ResizeObserver; happy-dom has none.
if (typeof globalThis.ResizeObserver !== 'function') {
  globalThis.ResizeObserver = window.ResizeObserver
    || class { observe() {} unobserve() {} disconnect() {} };
}
if (typeof globalThis.MutationObserver !== 'function' && window.MutationObserver) {
  globalThis.MutationObserver = window.MutationObserver;
}
// shared-styles.js probes `Document.prototype` and tf-table adopts
// /css/controls.css on build; an empty sheet answers the fetch under Node.
if (typeof globalThis.Document === 'undefined' && window.Document) globalThis.Document = window.Document;
globalThis.fetch = () => Promise.resolve({ ok: true, text: () => Promise.resolve('') });

// codec.js starts a WASM fetch at import time that rejects under Node; the
// screen under test never touches the codec because the transport is stubbed.
globalThis.addEventListener?.('unhandledrejection', (e) => e.preventDefault?.());
process.on('unhandledRejection', () => {});

const { ApiBinary } = await import('../protocol/api-binary-shim.js');
const { default: Screen } = await import('./tentanas.js');
await import('../protocol/codec.js').then((m) => m.codecReady).catch(() => {});

const LOCAL = 'nodeaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
const REMOTE = 'nodebbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb';
const MAC = 'nodeccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc';

function node(overrides) {
  return {
    nodeId: LOCAL, nodeName: 'orion', isLocal: true, online: true, instanceStatus: 'ready', health: 'ok',
    osName: 'Debian 12', zfsVersion: '2.2.4', elevationMode: 'helper', disksTotal: 2, disksWarning: 0,
    poolsTotal: 0, sharesTotal: 0, alertsActive: 0, capacityBytes: 4e12, usedBytes: 1e12, updatedAt: '2026-09-02 10:00:00',
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
  features: [{ id: 'zfs', status: 'ok', version: '2.2.4', requiredVersion: null, binaries: ['zpool', 'zfs'], kernelModule: 'zfs', packages: ['zfs'], detail: '', optional: false }],
  elevation: { mode: 'helper', helperState: 'ok', helperPath: '/usr/local/libexec/tentanas-helper', helperVersion: '1', sudoersPath: '/etc/sudoers.d/tentanas', coreUser: 'tentaflow', coreVersion: '1', armedUntil: null, ttlSecs: 900 },
};

// Records every call with its forwarding options so the tests can assert
// which node a request was addressed to.
const calls = [];
function stubTransport(fixtures) {
  const answer = (kind, payload, options) => {
    calls.push({ kind, payload, options: options || {} });
    if (!(kind in fixtures)) return Promise.reject(new Error(`unexpected request ${kind}`));
    const f = fixtures[kind];
    return Promise.resolve(typeof f === 'function' ? f(payload) : f);
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
      node({ nodeId: REMOTE, nodeName: 'vega', isLocal: false, health: 'warning', disksWarning: 1, elevationMode: 'unarmed' }),
      node({ nodeId: MAC, nodeName: 'mini', isLocal: false, instanceStatus: 'unsupported', osName: 'macOS', disksTotal: 0 }),
    ],
  },
  tentaNasEnvironmentRequest: { environment },
  tentaNasElevationPlanRequest: { plan: { helperSource: '/opt/tentaflow/tentanas-helper', helperSourcePresent: true, helperPath: '/usr/local/libexec/tentanas-helper', sudoersPath: '/etc/sudoers.d/tentanas', sudoersLine: 'tentaflow ALL=(root) NOPASSWD: /usr/local/libexec/tentanas-helper', coreUser: 'tentaflow', coreVersion: '1', commands: [['install', '-m', '0755', '/opt/tentaflow/tentanas-helper', '/usr/local/libexec/tentanas-helper']] } },
  tentaNasDisksListRequest: {
    disks: [disk({}), disk({ diskId: 'nvme0n1', name: 'nvme0n1', path: '/dev/nvme0n1', kind: 'nvme', model: 'Samsung 980', serial: 'S-1', health: 'warning', healthReason: 'pending sectors', wearPct: 12, rotational: false })],
    telemetry: { sampledAt: '2026-09-02 10:00:00', smartReadAt: '2026-09-02 09:59:00', smartState: 'live', detail: '' },
  },
  tentaNasJobsListRequest: { jobs: [{ jobId: 'j1', kind: 'smart.test', subject: 'sda', status: 'running', progressPct: 40, startedBy: 'admin', startedAt: '2026-09-02 09:58:00', finishedAt: null, error: null, log: ['started'] }] },
  tentaNasAlertsListRequest: { alerts: [] },
};

const flush = () => new Promise((r) => setTimeout(r, 0));

async function mountScreen(params = {}) {
  calls.length = 0;
  document.body.innerHTML = Screen.render();
  await Screen.mount(params);
  await flush();
  return document.getElementById('nas-root');
}

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

test('requests for a remote node carry its forward target; the local node does not', async () => {
  stubTransport(fixtures);
  await mountScreen({ node: REMOTE, tab: 'disks' });
  const remote = calls.filter((c) => c.kind === 'tentaNasDisksListRequest');
  assert.ok(remote.length >= 1);
  assert.equal(remote[0].options.targetNodeId, REMOTE);
  Screen.unmount();

  await mountScreen({ node: LOCAL, tab: 'disks' });
  const local = calls.filter((c) => c.kind === 'tentaNasDisksListRequest');
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

  Screen.diskFilter = 'problems';
  Screen.applyDiskRows();
  assert.equal(table.rows.length, 1);
  assert.equal(table.rows[0]._disk.diskId, 'nvme0n1');

  Screen.diskFilter = 'all';
  Screen.diskQuery = 'wd-1';
  Screen.applyDiskRows();
  assert.equal(table.rows.length, 1);
  assert.equal(table.rows[0]._disk.diskId, 'sda');
  Screen.unmount();
});

test('environment tab lists features and the other fleet nodes', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL, tab: 'environment' });
  await flush();
  assert.equal(root.querySelector('#nas-feature-table').rows.length, 1);
  const others = root.querySelector('#nas-others-table').rows;
  assert.equal(others.length, 2);
  assert.equal(others.find((r) => r._node.nodeId === REMOTE).channel.status, 'warn', 'unarmed node flagged');
  assert.ok(root.querySelector('[data-act="remove"]'), 'helper mode offers removal to the admin');
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
  prompt.dispatchEvent(new CustomEvent('action', { detail: { action: 'confirm' } }));
  await pending;
  assert.equal(seen, 'hunter2');
  prompt.remove();
  Screen.unmount();
});

test('overview shows running jobs and the node header carries the ZFS and channel chips', async () => {
  stubTransport(fixtures);
  const root = await mountScreen({ node: LOCAL });
  await flush();
  assert.ok(root.querySelector('#nas-ov-jobs .job-row'), 'running job listed');
  const badges = root.querySelector('#nas-head-badges').innerHTML;
  assert.match(badges, /ZFS 2\.2\.4/);
  assert.match(badges, /tentanas\.elevation\.mode_helper|helper/);
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
  // A second poll appends a sample instead of rebuilding the chart.
  await Screen.refreshOverview(root.querySelector('#nas-tab-body'));
  await flush();
  assert.strictEqual(io.querySelector('polyline[data-series-id="read"]'), readLine, 'polyline reused');
  assert.equal(readLine.getAttribute('points').trim().split(' ').length, 2);
  Screen.unmount();
});

test('disk detail draws the 24 h history on line charts and notes when samples are missing', async () => {
  const sample = (at, temperatureC, readBps, reallocatedSectors) => ({ at, temperatureC, reallocatedSectors, pendingSectors: 0, readBps, writeBps: 0, awaitMs: 1 });
  stubTransport({
    ...fixtures,
    tentaNasDiskGetRequest: {
      disk: disk({}),
      attributes: [],
      selfTests: [],
      history: [sample('2026-09-02 08:00:00', 33, 1e6, 0), sample('2026-09-02 09:00:00', 35, 2e6, null), sample('2026-09-02 10:00:00', 34, 5e5, null)],
      alerts: [],
    },
  });
  const root = await mountScreen({ node: LOCAL, tab: 'disks', disk: 'sda' });
  await flush();
  assert.ok(root.querySelector('#nas-disk-temp-chart tf-line-chart'), 'temperature chart mounted');
  assert.ok(root.querySelector('#nas-disk-io-chart tf-line-chart'), 'throughput chart mounted');
  assert.equal(root.querySelector('#nas-disk-temp-chart polyline.tf-chart__series-line').getAttribute('points').trim().split(' ').length, 3, 'three temperature samples plotted');
  // One reallocation sample only → no chart, the empty note instead.
  assert.equal(root.querySelector('#nas-disk-realloc-chart tf-line-chart'), null);
  assert.ok(root.querySelector('#nas-disk-realloc-chart .muted'));
  Screen.unmount();
});
