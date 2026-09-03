// =============================================================================
// File: modules/tentanas/datasets.test.js
// Description: The pool's Datasety tab against a fake screen: the tree
// table comes from DatasetsList joined with SharesList so every dataset
// shows its "Udostępnienie" chips, the snapshot chip carries the schedule
// and count, and the chips navigate — snapshots to the pool's Snapshoty
// tab filtered to the dataset, shares to the Udostępnienia tab — without
// selecting the row. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { drawDatasets, sharesOf, treeRows } = await import('./datasets.js');

const GB = 1024 ** 3;
const ds = (name, extra = {}) => ({
  name, kind: 'filesystem', mountpoint: `/${name}`, compression: 'zstd', compressRatio: 1.3, quotaBytes: 0,
  usedBytes: 10 * GB, availableBytes: 90 * GB, encryption: 'off', snapshotCount: 0, snapshotSchedule: null, ...extra,
});
const datasets = [
  ds('tank'),
  ds('tank/home', { snapshotCount: 12, snapshotSchedule: { schedule: { every: 'daily', hour: 2, minute: 0, weekday: 0, day: 1 } } }),
  ds('tank/home/anna'),
  ds('tank/media', { snapshotCount: 3 }),
];
const shares = [
  { shareId: 'sh-1', name: 'home', protocol: 'smb', sourcePath: '/tank/home', dataset: 'tank/home', enabled: true, mounts: [], sessions: 0, state: 'active' },
  { shareId: 'sh-2', name: 'home-nfs', protocol: 'nfs', sourcePath: '/tank/home', dataset: null, enabled: true, mounts: [], sessions: 0, state: 'active' },
  { shareId: 'sh-3', name: 'films', protocol: 'smb', sourcePath: '/tank/media', dataset: 'tank/media', enabled: false, mounts: [], sessions: 0, state: 'paused' },
];

function mount() {
  const host = document.createElement('div');
  document.body.appendChild(host);
  return host;
}
const fixtures = () => ({ tentaNasDatasetsListRequest: { datasets }, tentaNasSharesListRequest: { shares } });
const shadowRows = (table) => [...table.shadowRoot.querySelectorAll('tbody tr[data-idx]')];
const composedClick = (el) => el.dispatchEvent(new window.MouseEvent('click', { bubbles: true, composed: true, cancelable: true }));

test('joins datasets with their shares and paints the Udostępnienie and snapshot chips', async () => {
  const screen = fakeScreen(fixtures());
  const host = mount();
  await drawDatasets(screen, host, { pool: 'tank' });
  await flush();
  assert.deepEqual(screen.calls.map((c) => c.kind).sort(), ['tentaNasDatasetsListRequest', 'tentaNasSharesListRequest']);
  assert.deepEqual(screen.calls.find((c) => c.kind === 'tentaNasDatasetsListRequest').payload, { pool: 'tank' });

  const table = host.querySelector('#nas-ds-table');
  assert.deepEqual(table.rows.map((r) => r._ds.name), ['tank', 'tank/home', 'tank/home/anna', 'tank/media']);
  assert.deepEqual([...table.querySelectorAll('tf-column')].map((c) => c.getAttribute('label')), ['Nazwa', 'Kompresja', 'Quota', 'Użycie', 'Snapshoty', 'Udostępnienie']);

  const chipsOf = (html) => [...new window.DOMParser().parseFromString(html, 'text/html').querySelectorAll('tf-chip')].map((c) => c.getAttribute('label'));
  assert.deepEqual(chipsOf(table.rows[1].shares), ['SMB „home”', 'NFS „home-nfs”'], 'shares match by dataset name or by mountpoint');
  assert.deepEqual(chipsOf(table.rows[3].shares), ['SMB „films”']);
  assert.match(table.rows[2].shares, /—/);
  assert.match(table.rows[0].shares, /—/);
  assert.deepEqual(chipsOf(table.rows[1].snapshots), ['codziennie o 02:00 · 12']);
  assert.deepEqual(chipsOf(table.rows[3].snapshots), ['3']);
  assert.match(table.rows[0].snapshots, /—/, 'the pool root has no snapshot chip');

  assert.deepEqual(sharesOf(shares, datasets[1]).map((s) => s.name), ['home', 'home-nfs']);
  assert.deepEqual(treeRows({ datasets, query: '', collapsed: new Set(['tank/home']) }).map((d) => d.name), ['tank', 'tank/home', 'tank/media'], 'a collapsed parent hides its children');
  screen.dispose();
});

test('the chips navigate: snapshots to the pool\'s Snapshoty tab for that dataset, shares to Udostępnienia', async () => {
  const screen = fakeScreen(fixtures());
  const host = mount();
  await drawDatasets(screen, host, { pool: 'tank' });
  await flush();
  const table = host.querySelector('#nas-ds-table');
  const rows = shadowRows(table);
  assert.equal(rows.length, 4, 'rows are painted in the table shadow');

  composedClick(rows[1].querySelector('tf-chip[data-nav="snapshots"]'));
  assert.deepEqual(screen.openedPools, [{ name: 'tank', poolTab: 'snapshots', dataset: 'tank/home' }]);
  assert.equal(screen.dataset ?? null, null, 'the chip click did not select the row');

  composedClick(rows[3].querySelector('tf-chip[data-nav="shares"]'));
  assert.deepEqual(screen.switchedTabs, ['shares']);
  assert.equal(screen.dataset ?? null, null);
  screen.dispose();
});

test('a viewer gets no create buttons and no row actions', async () => {
  const screen = fakeScreen(fixtures(), { admin: false });
  const host = mount();
  await drawDatasets(screen, host, { pool: 'tank' });
  await flush();
  assert.equal(host.querySelector('[data-act="create"]'), null);
  assert.equal(host.querySelector('[data-act="create-volume"]'), null);
  const table = host.querySelector('#nas-ds-table');
  assert.equal(table.rowActions(table.rows[1]), null);
  screen.dispose();
});
