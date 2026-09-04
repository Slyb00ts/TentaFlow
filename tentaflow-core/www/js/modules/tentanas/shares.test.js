// =============================================================================
// File: modules/tentanas/shares.test.js
// Description: The Sharing tab against a fake screen: the share table with
// the per-node fleet summary, the SMB/NFS filter and search, the pause
// action's update payload, the viewer/empty states, the share detail window
// with sessions and per-node mount rows, and the delete dialog's retype
// gate. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, typeInto, confirmWindow, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawShares, openShareDetail, openShareDeleteDialog, fleetSummary, mountStateTone, mountStateLabel } = await import('./shares.js');

const mount = (nodeId, nodeName, state, detail = '', transport = '') => ({ nodeId, nodeName, state, detail, mountpoint: state === 'mounted' ? `/mnt/tentanas/x` : '', checkedAt: null, transport });
const share = (overrides = {}) => ({
  shareId: 'sh-1', name: 'dokumenty', protocol: 'smb', sourcePath: '/tank/dokumenty', dataset: 'tank/dokumenty', enabled: true,
  smb: { guests: false, previousVersions: true, recycleBin: true, timeMachine: false, users: [{ user: 'anna', mode: 'rw' }] }, nfs: null,
  fleetMount: true,
  mounts: [mount('node-helios', 'helios', 'source'), mount('node-atlas', 'atlas', 'mounted'), mount('node-orion', 'orion', 'pending', 'channel unarmed'), mount('node-tabbie', 'tabbie', 'unsupported')],
  sessions: 2, state: 'active', stateDetail: '', createdAt: '2026-08-01T10:00:00Z', updatedAt: '2026-08-01T10:00:00Z', ...overrides,
});
const nfsShare = share({ shareId: 'sh-2', name: 'media', protocol: 'nfs', sourcePath: '/tank/media', dataset: 'tank/media', smb: null, nfs: { networks: ['10.10.0.0/24'], readOnly: false, rootSquash: true, asyncWrites: false }, sessions: 0, mounts: [mount('node-helios', 'helios', 'source'), mount('node-atlas', 'atlas', 'error', 'mount.nfs: timed out')] });
const listResponse = { shares: [nfsShare, share()], users: [{ name: 'anna', description: '', createdAt: null, shares: ['dokumenty'] }], mountRoot: '/mnt/tentanas' };

function host() {
  const body = document.createElement('div');
  document.body.appendChild(body);
  return body;
}

// The block half of n12 lives in the same tab and asks for its own list, so
// every fake screen answers it; the tests that care about targets supply their
// own rows.
const emptyTargets = { targets: [], services: [], capabilities: { iscsi: true, nvmet: true, iser: false, nvmeRdma: false, dhchap: false, interfaces: [], volumes: [] } };

function screenWith(fixtures, opts) {
  const screen = fakeScreen({ tentaNasTargetsListRequest: emptyTargets, ...fixtures }, opts);
  screen.nodes = [];
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

test('a mounted node names the transport it actually got', () => {
  assert.equal(mountStateLabel('mounted', 'rdma'), 'zamontowany · RDMA');
  assert.equal(mountStateLabel('mounted', 'tcp'), 'zamontowany · TCP');
  // A node that reported before the field existed keeps the plain label
  // rather than claiming a transport nobody measured.
  assert.equal(mountStateLabel('mounted', ''), 'zamontowany');
  assert.equal(mountStateLabel('mounted'), 'zamontowany');
  // Only a mount has a transport: a source reads through the filesystem and
  // a pending node has not mounted anything yet.
  assert.equal(mountStateLabel('source', 'rdma'), 'źródło');
  assert.equal(mountStateLabel('pending', 'tcp'), 'oczekuje');
  assert.equal(mountStateLabel('nonsense', 'rdma'), 'nonsense');
});

test('the transport chip marks an RDMA share in the list and both ways in the detail', async () => {
  const rdmaShare = share({
    shareId: 'sh-3', name: 'modele', protocol: 'nfs', sourcePath: '/tank/modele', dataset: 'tank/modele', smb: null,
    nfs: { networks: ['10.10.0.0/24'], readOnly: false, rootSquash: true, asyncWrites: false, rdma: true },
    mounts: [mount('node-helios', 'helios', 'source'), mount('node-atlas', 'atlas', 'mounted', '', 'rdma'), mount('node-orion', 'orion', 'mounted', '', 'tcp')],
    sessions: 0,
  });
  const screen = screenWith({ tentaNasSharesListRequest: { ...listResponse, shares: [rdmaShare] } });
  const body = host();
  await drawShares(screen, body);
  await flush();

  const table = body.querySelector('#nas-sh-table');
  assert.equal(table.rows.length, 1);
  assert.match(table.rows[0].protocol, /label="RDMA"/, 'the list flags the non-default transport');
  // Every node's outcome is in the fleet tooltip, transport included.
  assert.match(table.rows[0].fleet, /atlas ✓ RDMA/);
  assert.match(table.rows[0].fleet, /orion ✓ TCP/);
  // A TCP-only NFS share carries no chip: TCP is what every share does.
  assert.ok(!/label="TCP"/.test(table.rows[0].protocol));
  screen.dispose();

  const detail = screenWith({ tentaNasShareGetRequest: { share: rdmaShare, sessions: [] } });
  const win = openShareDetail(detail, 'sh-3', { mountRoot: '/mnt/tentanas' });
  await flush();
  const rows = [...win.querySelectorAll('.stat-rows .sr')].map((r) => r.textContent);
  assert.ok(rows.some((r) => /Transport/.test(r)), 'the detail names the transport explicitly');
  const mounts = win.querySelector('#nas-sd-mounts').innerHTML;
  assert.match(mounts, /zamontowany · RDMA/);
  assert.match(mounts, /zamontowany · TCP/);
  detail.dispose();
});

test('renders the share table with fleet chips, filters by protocol and searches', async () => {
  const screen = screenWith({ tentaNasSharesListRequest: listResponse });
  const body = host();
  await drawShares(screen, body);
  await flush();

  // The tab feeds both halves of n12: the file shares and the block targets.
  assert.deepEqual(screen.calls.map((c) => c.kind), ['tentaNasSharesListRequest', 'tentaNasTargetsListRequest']);
  assert.equal(body.querySelector('#nas-sh-count').getAttribute('label'), '2');
  assert.match(body.querySelector('#nas-sh-mount-hint').textContent, /\/mnt\/tentanas\/<nazwa>/);
  assert.match(body.querySelector('#nas-sh-explain').textContent, /montowane automatycznie/);
  // n12's toolbar: five segments over both tables, and a count that spans them.
  const filter = body.querySelector('#nas-sh-filter');
  assert.deepEqual([...filter.querySelectorAll('option')].map((o) => o.textContent), ['Wszystkie 2', 'SMB 1', 'NFS 1', 'iSCSI 0', 'NVMe-oF 0']);

  const table = body.querySelector('#nas-sh-table');
  // n12:182 — the trailing actions column carries a visible header.
  assert.equal(table.getAttribute('actions-label'), 'Akcje');
  assert.equal(table.shadowRoot.querySelector('thead th.tf-table__actions-col').textContent, 'Akcje');
  assert.deepEqual(table.rows.map((r) => r._share.name), ['dokumenty', 'media'], 'sorted by name');
  assert.match(table.rows[0].name, /i-share/, 'the name cell carries the share glyph');
  assert.match(table.rows[0].fleet, /orion/);
  assert.match(table.rows[0].fleet, /status="warn"/);
  assert.match(table.rows[0].fleet, /title="helios ✓ · atlas ✓ · orion ⏳ channel unarmed · tabbie n\/d"/);
  assert.match(table.rows[1].fleet, /status="err"/);
  assert.equal(table.rows[0].sessions, 2);
  assert.match(table.rows[0].source, /tank\/dokumenty/);

  const actions = table.rowActions(table.rows[0]);
  assert.deepEqual([...actions.querySelectorAll('tf-button')].map((b) => b.dataset.act), ['edit', 'pause', 'delete']);
  assert.equal(actions.querySelector('[data-act="pause"]').getAttribute('title'), 'Zatrzymaj udostępnianie');

  filter.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'nfs' } }));
  assert.deepEqual(table.rows.map((r) => r._share.name), ['media']);
  body.querySelector('#nas-sh-search').dispatchEvent(new window.CustomEvent('search', { bubbles: true, detail: { value: 'nothing' } }));
  assert.equal(table.rows.length, 0);
  screen.dispose();
});

test('the pause action resends the share with enabled=false through sudo and refreshes', async () => {
  const s = share();
  const screen = screenWith({
    tentaNasSharesListRequest: { ...listResponse, shares: [s] },
    tentaNasShareUpdateRequest: (payload) => ({ share: { ...s, enabled: payload.enabled, state: payload.enabled ? 'active' : 'disabled' } }),
  });
  const body = host();
  await drawShares(screen, body);
  await flush();
  const table = body.querySelector('#nas-sh-table');
  click(table.rowActions(table.rows[0]).querySelector('[data-act="pause"]'));
  await flush();
  await flush();
  const update = screen.calls.find((c) => c.kind === 'tentaNasShareUpdateRequest');
  assert.deepEqual(update.payload, {
    shareId: 'sh-1', smb: s.smb, nfs: null, fleetMount: true, enabled: false, sudoPassword: 'hunter2',
  }, 'options and fleet mount are kept, only enabled flips');
  assert.equal(screen.calls.filter((c) => c.kind === 'tentaNasSharesListRequest').length, 2, 'the list refreshed after the answer');
  screen.dispose();
});

test('a viewer gets only the details action and the empty state offers the wizard', async () => {
  const screen = screenWith({ tentaNasSharesListRequest: { shares: [], users: [], mountRoot: '/mnt/tentanas' } }, { admin: false });
  const body = host();
  await drawShares(screen, body);
  await flush();
  assert.ok(body.querySelector('#nas-sh-list tf-empty-state'), 'empty state rendered');
  assert.ok(body.querySelector('#nas-sh-list [data-act="create-empty"]'), 'the empty state offers the wizard');
  screen.dispose();

  const viewer = screenWith({ tentaNasSharesListRequest: listResponse }, { admin: false });
  const body2 = host();
  await drawShares(viewer, body2);
  await flush();
  const table = body2.querySelector('#nas-sh-table');
  assert.deepEqual([...table.rowActions(table.rows[0]).querySelectorAll('tf-button')].map((b) => b.dataset.act), ['details']);
  viewer.dispose();
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

test('an SMB Direct share is chipped with what the RDMA path costs, in the list and in the detail', async () => {
  const directShare = share({
    shareId: 'sh-4', name: 'modele', sourcePath: '/tank/modele', dataset: 'tank/modele',
    smb: { guests: false, previousVersions: false, recycleBin: false, timeMachine: false, smbDirect: true, users: [] },
    mounts: [], sessions: 0,
  });
  const screen = screenWith({ tentaNasSharesListRequest: { ...listResponse, shares: [directShare] } });
  const body = host();
  await drawShares(screen, body);
  await flush();

  const table = body.querySelector('#nas-sh-table');
  assert.match(table.rows[0].protocol, /label="SMB Direct: bez audytu"/, 'the chip names the transport and its cost together');
  screen.dispose();

  // A plain SMB share carries no chip: Samba-only is what every share does.
  const plain = screenWith({ tentaNasSharesListRequest: { ...listResponse, shares: [share()] } });
  const plainBody = host();
  await drawShares(plain, plainBody);
  await flush();
  assert.ok(!/SMB Direct/.test(plainBody.querySelector('#nas-sh-table').rows[0].protocol));
  plain.dispose();

  // The detail names it BOTH ways, so which SMB backends serve a share is
  // never something the admin has to infer from an absent chip.
  const detail = screenWith({ tentaNasShareGetRequest: { share: directShare, sessions: [] } });
  const win = openShareDetail(detail, 'sh-4', { mountRoot: '/mnt/tentanas' });
  await flush();
  const rows = [...win.querySelectorAll('.stat-rows .sr')].map((r) => r.innerHTML);
  assert.ok(rows.some((r) => /SMB Direct: bez audytu/.test(r)), 'the detail row names it');
  detail.dispose();

  const off = screenWith({ tentaNasShareGetRequest: { share: share(), sessions: [] } });
  const offWin = openShareDetail(off, 'sh-1', { mountRoot: '/mnt/tentanas' });
  await flush();
  assert.ok([...offWin.querySelectorAll('.stat-rows .sr')].some((r) => /tylko Samba/.test(r.innerHTML)));
  off.dispose();
});

test('a node that refused to start SMB Direct warns on an otherwise active share', async () => {
  // The reconcile refusal of §5.4b: Samba keeps exporting the share, so the
  // state stays active — but the option that did not take effect must not
  // look the same as one that did.
  const refused = share({
    shareId: 'sh-5', name: 'modele', sourcePath: '/tank/modele',
    smb: { guests: false, previousVersions: false, recycleBin: false, timeMachine: false, smbDirect: true, users: [] },
    mounts: [], sessions: 0, state: 'active',
    stateDetail: 'SMB Direct is not served on this node: enp3s0 192.168.1.20 also carries the default gateway',
  });
  const screen = screenWith({ tentaNasSharesListRequest: { ...listResponse, shares: [refused] } });
  const body = host();
  await drawShares(screen, body);
  await flush();

  const row = body.querySelector('#nas-sh-table').rows[0];
  assert.match(row.name, /label="Ostrzeżenie"/, 'an active share with a detail is a warning, not a plain row');
  assert.match(row.name, /also carries the default gateway/, 'the reason is on the row, not only in the job log');
  assert.ok(!/label="Błąd"/.test(row.name), 'the share is still exported over the LAN');
  screen.dispose();
});

test('the Sharing tab carries both n12 tables and the five segments switch between them', async () => {
  const target = {
    targetId: 't1', name: 'vm-store', protocol: 'iscsi', wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store',
    enabled: true,
    luns: [{ index: 0, source: 'tank/vm-store', devicePath: '/dev/zvol/tank/vm-store', sizeBytes: 2199023255552, thin: true, uuid: 'u1', groupId: 1, sourceKind: 'zvol' }],
    portals: [{ interface: 'storage0', address: '10.10.0.5', port: 3260, transport: 'tcp' }],
    auth: { method: 'mutual-chap', username: 'vmware01', secretSet: true, mutualSecretSet: true },
    initiators: [], portGroups: [{ groupId: 1, state: 'optimized', preferred: false }],
    sessions: 0, state: 'active', stateDetail: '', createdAt: null, updatedAt: null,
  };
  const screen = screenWith({
    tentaNasSharesListRequest: listResponse,
    tentaNasTargetsListRequest: { ...emptyTargets, targets: [target] },
  });
  const body = host();
  await drawShares(screen, body);
  await flush();

  // Both cards are on screen at once, as n12 draws them.
  assert.equal(body.querySelector('#nas-sh-table').rows.length, 2);
  assert.equal(body.querySelector('#nas-tg-table').rows.length, 1);
  const filter = body.querySelector('#nas-sh-filter');
  assert.deepEqual([...filter.querySelectorAll('option')].map((o) => o.textContent), ['Wszystkie 3', 'SMB 1', 'NFS 1', 'iSCSI 1', 'NVMe-oF 0']);

  const shareCard = body.querySelector('#nas-sh-list').closest('.section-card');
  const targetHost = body.querySelector('#nas-sh-targets');
  assert.ok(!shareCard.hidden && !targetHost.hidden, 'everything shows both halves');

  // A block segment hides the file table and vice versa.
  filter.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'iscsi' } }));
  await flush();
  assert.ok(shareCard.hidden && !targetHost.hidden);
  filter.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'smb' } }));
  await flush();
  assert.ok(!shareCard.hidden && targetHost.hidden);

  // The toolbar carries n12's two create buttons.
  assert.ok(body.querySelector('[data-act="create"]'));
  assert.ok(body.querySelector('[data-act="create-target"]'));
  screen.dispose();
});
