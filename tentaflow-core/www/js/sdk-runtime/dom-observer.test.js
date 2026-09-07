// =============================================================================
// Plik: sdk-runtime/dom-observer.test.js
// Opis: Potwierdzenia snapshotów zachowują obserwatora DOM po rzeczywistym GC.
// Przykład: node --test js/sdk-runtime/dom-observer.test.js
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';

const setupUrl = new URL('../modules/tentanas/_test-setup.js', import.meta.url).href;
const snapshotsUrl = new URL('../modules/tentanas/snapshots.js', import.meta.url).href;

for (const operation of ['create', 'delete']) {
  test(`potwierdzenie snapshot ${operation} przeżywa GC obserwatora`, () => {
    const result = spawnSync(process.execPath, ['--expose-gc', '--input-type=module', '-e', `
      import assert from 'node:assert/strict';
      const { fakeScreen, flush, confirmWindow, click, typeInto } = await import(${JSON.stringify(setupUrl)});
      const { openSnapshotNowDialog, drawSnapshots } = await import(${JSON.stringify(snapshotsUrl)});
      const calls = [];
      let complete;
      const completed = new Promise(resolve => { complete = resolve; });
      const receive = payload => { calls.push(payload); complete(payload); return { snapshots: [], total: 0 }; };
      const snapshot = { name: 'tank/home@protected', shortName: 'protected', dataset: 'tank/home',
        createdAt: '2026-09-01 00:00:00', origin: 'manual', holds: 1, clones: [], usedBytes: 1 };
      const screen = fakeScreen({
        tentaNasSnapshotCreateRequest: receive, tentaNasSnapshotDestroyRequest: receive,
        tentaNasSnapshotsListRequest: { snapshots: [snapshot], total: 1, totalUsedBytes: 1 },
        tentaNasSnapshotSchedulesListRequest: { schedules: [] }, tentaNasSharesListRequest: { shares: [] },
      });
      if (${JSON.stringify(operation)} === 'create') {
        const win = openSnapshotNowDialog(screen, { dataset: 'tank/home', onDone() {} });
        await flush();
        win.querySelector('#nas-sn-protect').checked = true;
        win.querySelector('#nas-sn-protect').dispatchEvent(new CustomEvent('change'));
        typeInto(win.querySelector('#nas-sn-protect-days'), '90');
        confirmWindow(win);
      } else {
        const host = document.createElement('div');
        document.body.append(host);
        await drawSnapshots(screen, host, { pool: 'tank', datasets: [{ name: 'tank/home' }] });
        const table = host.querySelector('#nas-snap-table');
        click(table.rowActions(table.rows[0]).querySelector('[data-act="delete"]'));
      }
      await flush();
      const confirm = [...document.querySelectorAll('tf-window')].at(-1);
      assert.ok(confirm);
      assert.equal(calls.length, 0);
      globalThis.gc();
      confirmWindow(confirm);
      let timer;
      const payload = await Promise.race([completed, new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('Brak odpowiedzi po odłączeniu dialogu')), 1000);
      })]);
      clearTimeout(timer);
      assert.equal(confirm.isConnected, false);
      assert.equal(calls.length, 1);
      if (${JSON.stringify(operation)} === 'create') assert.equal(payload.protectDays, 90);
      else assert.deepEqual(payload.names, [snapshot.name]);
      screen.dispose();
      process.exit(0);
    `], { encoding: 'utf8', timeout: 5000 });
    assert.equal(result.status, 0, result.stderr || String(result.error));
  });
}
