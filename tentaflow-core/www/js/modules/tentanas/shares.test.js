// =============================================================================
// File: modules/tentanas/shares.test.js
// Description: The Sharing tab against a fake screen: the service rows, the
// share table with the per-node fleet summary, the SMB/NFS filter, the
// empty state, the fleet-mounts table with its retry, the share detail
// window with sessions and per-node mount rows, and the delete dialog's
// retype gate. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, typeInto, confirmWindow, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawShares, openShareDetail, openShareDeleteDialog, fleetSummary, mountStateTone } = await import('./shares.js');

const mount = (nodeId, nodeName, state, detail = '') => ({ nodeId, nodeName, state, detail, mountpoint: state === 'mounted' ? `/mnt/tentanas/x` : '', checkedAt: null });
const share = (overrides = {}) => ({
  shareId: 'sh-1', name: 'dokumenty', protocol: 'smb', sourcePath: '/tank/dokumenty', dataset: 'tank/dokumenty', enabled: true,
  smb: { guests: false, previousVersions: true, recycleBin: true, timeMachine: false, users: [{ user: 'anna', mode: 'rw' }] }, nfs: null,
  fleetMount: true,
  mounts: [mount('node-helios', 'helios', 'source'), mount('node-atlas', 'atlas', 'mounted'), mount('node-orion', 'orion', 'pending', 'channel unarmed'), mount('node-tabbie', 'tabbie', 'unsupported')],
  sessions: 2, state: 'active', stateDetail: '', createdAt: '2026-08-01T10:00:00Z', updatedAt: '2026-08-01T10:00:00Z', ...overrides,
});
const nfsShare = share({ shareId: 'sh-2', name: 'media', protocol: 'nfs', sourcePath: '/tank/media', dataset: 'tank/media', smb: null, nfs: { networks: ['10.10.0.0/24'], readOnly: false, rootSquash: true, asyncWrites: false }, sessions: 0, mounts: [mount('node-helios', 'helios', 'source'), mount('node-atlas', 'atlas', 'error', 'mount.nfs: timed out')] });
const services = [
  { protocol: 'smb', installed: true, running: true, version: '4.20.1', configPath: '/etc/samba/smb.conf', detail: '' },
  { protocol: 'nfs', installed: false, running: false, version: null, configPath: '', detail: '' },
];
const listResponse = { shares: [nfsShare, share()], services, users: [{ name: 'anna', description: '', createdAt: null, shares: ['dokumenty'] }], mountRoot: '/mnt/tentanas' };

function host() {
  const body = document.createElement('div');
  document.body.appendChild(body);
  return body;
}

function screenWith(fixtures, opts) {
  const screen = fakeScreen(fixtures, opts);
  screen.nodes = [];
  screen.environment = { packageManager: 'apt', features: [{ id: 'nfs', packages: ['nfs-kernel-server'] }, { id: 'samba', packages: ['samba'] }] };
  screen.installed = [];
  screen.installFeature = (f) => screen.installed.push(f.id);
  return screen;
}

test('fleet summary folds the per-node mount states into one chip', () => {
  const s = share();
  const ok = fleetSummary(s);
  assert.equal(ok.tone, 'warn', 'a pending node wins over mounted ones');
  assert.match(ok.label, /orion/);
  assert.match(ok.title, /helios ✓/);
  assert.match(ok.title, /orion ⏳/);
  assert.match(ok.title, /tabbie n\/d/);
  assert.equal(fleetSummary(nfsShare).tone, 'err');
  assert.match(fleetSummary(nfsShare).label, /atlas/);
  assert.equal(fleetSummary(share({ mounts: [mount('node-helios', 'helios', 'source'), mount('node-atlas', 'atlas', 'mounted')] })).tone, 'ok');
  assert.equal(fleetSummary(share({ fleetMount: false, mounts: [] })).tone, 'neutral');
  assert.equal(mountStateTone('unsupported'), 'neutral');
});

test('renders services, the share table with fleet chips and filters by protocol', async () => {
  const screen = screenWith({ tentaNasSharesListRequest: listResponse, tentaNasFleetMountsListRequest: { mounts: [] } });
  const body = host();
  await drawShares(screen, body);
  await flush();

  assert.deepEqual(screen.calls.map((c) => c.kind).sort(), ['tentaNasFleetMountsListRequest', 'tentaNasSharesListRequest']);
  assert.match(body.querySelector('#nas-sh-sub').textContent, /2 share'y · SMB 1 · NFS 1/);

  const smbRow = body.querySelector('#nas-sh-services [data-service="smb"]');
  assert.equal(smbRow.querySelector('tf-chip').getAttribute('label'), 'Działa');
  assert.match(smbRow.textContent, /4\.20\.1/);
  const nfsRow = body.querySelector('#nas-sh-services [data-service="nfs"]');
  assert.equal(nfsRow.querySelector('tf-chip').getAttribute('label'), 'Nie zainstalowano');
  const install = nfsRow.querySelector('[data-act="install"]');
  assert.ok(install, 'missing service offers the install button from the environment');
  assert.equal(install.dataset.feature, 'nfs');
  click(install);
  assert.deepEqual(screen.installed, ['nfs']);

  const table = body.querySelector('#nas-sh-table');
  assert.deepEqual(table.rows.map((r) => r._share.name), ['dokumenty', 'media'], 'sorted by name');
  assert.match(table.rows[0].fleet, /orion/);
  assert.match(table.rows[0].fleet, /status="warn"/);
  assert.match(table.rows[0].fleet, /title="helios ✓ · atlas ✓ · orion ⏳ channel unarmed · tabbie n\/d"/);
  assert.match(table.rows[1].fleet, /status="err"/);
  assert.equal(table.rows[0].sessions, 2);
  assert.match(table.rows[0].source, /tank\/dokumenty/);
  assert.match(body.querySelector('#nas-sh-hint').textContent, /2 z 2/);
  assert.equal(body.querySelector('#nas-sh-fleet').hidden, true, 'no fleet mounts on this node');

  const actions = table.rowActions(table.rows[0]);
  assert.deepEqual([...actions.querySelectorAll('tf-button')].map((b) => b.dataset.act), ['details', 'edit', 'refresh-mounts', 'delete']);

  body.querySelector('#nas-sh-filter').dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'nfs' } }));
  assert.deepEqual(table.rows.map((r) => r._share.name), ['media']);
  assert.match(body.querySelector('#nas-sh-hint').textContent, /1 z 2/);
  body.querySelector('#nas-sh-search').dispatchEvent(new window.CustomEvent('search', { bubbles: true, detail: { value: 'nothing' } }));
  assert.equal(table.rows.length, 0);
  screen.dispose();
});

test('a viewer gets no admin actions and the empty state offers the wizard', async () => {
  const screen = screenWith({ tentaNasSharesListRequest: { shares: [], services, users: [], mountRoot: '/mnt/tentanas' }, tentaNasFleetMountsListRequest: { mounts: [] } }, { admin: false });
  const body = host();
  await drawShares(screen, body);
  await flush();
  assert.ok(body.querySelector('#nas-sh-list tf-empty-state'), 'empty state rendered');
  assert.equal(body.querySelector('[data-act="users"]'), null, 'users button is admin-only');
  assert.equal(body.querySelector('#nas-sh-services [data-act="install"]'), null, 'no install button for viewers');
  assert.match(body.querySelector('#nas-sh-services [data-service="nfs"]').textContent, /Środowisko/);
  screen.dispose();
});

test('the fleet-mounts table lists mounts of other nodes and retries through sudo', async () => {
  const mounts = [
    { shareId: 'sh-9', shareName: 'zdjecia', protocol: 'smb', sourceNodeId: 'node-helios', sourceNodeName: 'helios', mountpoint: '/mnt/tentanas/zdjecia', state: 'mounted', detail: '', checkedAt: null },
    { shareId: 'sh-8', shareName: 'backup', protocol: 'nfs', sourceNodeId: 'node-atlas', sourceNodeName: 'atlas', mountpoint: '/mnt/tentanas/backup', state: 'error', detail: 'mount.nfs: access denied', checkedAt: null },
  ];
  const screen = screenWith({
    tentaNasSharesListRequest: { shares: [], services, users: [], mountRoot: '/mnt/tentanas' },
    tentaNasFleetMountsListRequest: { mounts },
    tentaNasFleetMountRetryRequest: { mounts: [mounts[0], { ...mounts[1], state: 'mounted', detail: '' }] },
  });
  const body = host();
  await drawShares(screen, body);
  await flush();
  const section = body.querySelector('#nas-sh-fleet');
  assert.equal(section.hidden, false);
  const table = section.querySelector('#nas-fm-table');
  assert.deepEqual(table.rows.map((r) => r._mount.shareName), ['backup', 'zdjecia']);
  assert.match(table.rows[0].state, /status="err"/);
  assert.equal(table.rows[0].detail, 'mount.nfs: access denied');
  click(table.rowActions(table.rows[0]));
  await flush();
  const retry = screen.calls.find((c) => c.kind === 'tentaNasFleetMountRetryRequest');
  assert.deepEqual(retry.payload, { shareId: 'sh-8', sudoPassword: 'hunter2' });
  assert.match(table.rows[0].state, /status="ok"/, 'the answer replaces the rows');
  click(section.querySelector('[data-act="retry-all"]'));
  await flush();
  assert.deepEqual(screen.calls.filter((c) => c.kind === 'tentaNasFleetMountRetryRequest').at(-1).payload, { shareId: '', sudoPassword: 'hunter2' });
  screen.dispose();
});

test('the detail window shows access, per-node mounts and sessions, and refreshes mounts', async () => {
  const s = share();
  let refreshed = 0;
  const screen = screenWith({
    tentaNasShareGetRequest: { share: s, sessions: [{ client: '10.10.0.7', user: 'anna', connectedAt: null }, { client: '10.10.0.9', user: '', connectedAt: null }] },
    tentaNasShareMountsRefreshRequest: () => { refreshed++; return { share: { ...s, mounts: s.mounts.map((m) => (m.state === 'pending' ? { ...m, state: 'mounted' } : m)) }, sessions: [] }; },
  });
  const win = openShareDetail(screen, 'sh-1', { mountRoot: '/mnt/tentanas' });
  await flush();
  assert.deepEqual(screen.calls[0], { kind: 'tentaNasShareGetRequest', payload: { shareId: 'sh-1' } });
  assert.equal(win.getAttribute('subtitle'), 'dokumenty · SMB');
  const rows = [...win.querySelectorAll('.stat-rows .sr')].map((r) => r.textContent.replace(/\s+/g, ' ').trim());
  assert.ok(rows.some((r) => /\/mnt\/tentanas\/dokumenty/.test(r)), 'fleet path row');
  assert.ok(rows.some((r) => /anna \(RW\)/.test(r)), 'grant row');
  assert.deepEqual([...win.querySelectorAll('#nas-sd-mounts .sr')].map((r) => r.dataset.node), ['node-helios', 'node-atlas', 'node-orion', 'node-tabbie']);
  assert.match(win.querySelector('#nas-sd-mounts [data-node="node-orion"]').textContent, /channel unarmed/);
  assert.equal(win.querySelector('#nas-sd-sessions').rows.length, 2);
  click(win.querySelector('[data-act="refresh-mounts"]'));
  await flush();
  assert.equal(refreshed, 1);
  assert.deepEqual(screen.calls.at(-1).payload, { shareId: 'sh-1' });
  assert.match(win.querySelector('#nas-sd-mounts [data-node="node-orion"]').innerHTML, /status="ok"/);
  screen.dispose();
});

test('deleting a share needs the name retyped and sends the confirm name through sudo', async () => {
  let done = 0;
  const screen = screenWith({ tentaNasShareDeleteRequest: { job: { jobId: 'job-3', kind: 'share_delete', status: 'running', progressPct: 0, log: [] } } });
  const win = openShareDeleteDialog(screen, share(), () => { done++; });
  await flush();
  const btn = win.querySelector('tf-button[data-action="confirm"]');
  assert.ok(btn.hasAttribute('disabled'));
  assert.match(win.querySelector('.loss-list').textContent, /atlas/, 'mounted nodes listed');
  typeInto(win.querySelector('#nas-retype'), 'dokument');
  confirmWindow(win);
  await flush();
  assert.equal(screen.calls.length, 0, 'nothing sent while locked');
  typeInto(win.querySelector('#nas-retype'), 'dokumenty');
  assert.ok(!btn.hasAttribute('disabled'));
  confirmWindow(win);
  await flush();
  await flush();
  assert.deepEqual(screen.calls[0], { kind: 'tentaNasShareDeleteRequest', payload: { shareId: 'sh-1', confirmName: 'dokumenty', sudoPassword: 'hunter2' } });
  assert.equal(screen.jobLogs[0].jobId, 'job-3');
  screen.jobLogs[0].onFinish();
  assert.equal(done, 1);
  screen.dispose();
});
