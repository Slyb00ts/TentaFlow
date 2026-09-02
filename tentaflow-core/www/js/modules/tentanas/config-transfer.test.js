// =============================================================================
// File: modules/tentanas/config-transfer.test.js
// Description: Config export and import against a fake screen: the export
// hands the node's JSON to the browser as a download named by the backend
// and releases the object URL, the import plan renders kind / name / action
// chips plus warnings, a conflict blocks Apply, and a clean plan is applied
// through sudo as ConfigImportApply with the job log opened. Runs under
// happy-dom.
// =============================================================================

import { fakeScreen, flush, confirmWindow, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { exportConfig, readJsonFile, renderImportPlan, planBlocked, planCounts, openConfigImportDialog, applyImport } = await import('./config-transfer.js');

const CONFIG_JSON = JSON.stringify({ version: 1, shares: [{ name: 'dokumenty' }] });

/** Replaces the object-URL API and anchor clicks for one test and records what the download did. */
function captureDownloads() {
  const seen = { urls: [], revoked: [], clicks: [] };
  const original = { create: URL.createObjectURL, revoke: URL.revokeObjectURL, click: window.HTMLAnchorElement.prototype.click };
  URL.createObjectURL = (blob) => { const u = `blob:test/${seen.urls.length}`; seen.urls.push({ url: u, blob }); return u; };
  URL.revokeObjectURL = (u) => seen.revoked.push(u);
  window.HTMLAnchorElement.prototype.click = function () { seen.clicks.push({ href: this.href, download: this.download, connected: this.isConnected }); };
  seen.restore = () => {
    URL.createObjectURL = original.create;
    URL.revokeObjectURL = original.revoke;
    window.HTMLAnchorElement.prototype.click = original.click;
  };
  return seen;
}

const pickFile = (host, name, text) => {
  host.querySelector('#nas-ci-file').dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { files: [{ name, text: async () => text }] } }));
};

test('exporting downloads the backend JSON under the backend filename and revokes the URL', async () => {
  const screen = fakeScreen({ tentaNasConfigExportRequest: { json: CONFIG_JSON, filename: 'tentanas-orion-2026-09-02.json' } });
  const seen = captureDownloads();
  try {
    const name = await exportConfig(screen);
    assert.equal(name, 'tentanas-orion-2026-09-02.json');
    assert.deepEqual(screen.calls, [{ kind: 'tentaNasConfigExportRequest', payload: {} }]);
    assert.equal(seen.urls.length, 1);
    assert.equal(seen.urls[0].blob.type, 'application/json');
    assert.equal(await seen.urls[0].blob.text(), CONFIG_JSON);
    assert.equal(seen.clicks.length, 1);
    assert.equal(seen.clicks[0].download, 'tentanas-orion-2026-09-02.json');
    assert.ok(seen.clicks[0].href.endsWith('blob:test/0'));
    assert.ok(seen.clicks[0].connected, 'the anchor is in the document when clicked');
    assert.deepEqual(seen.revoked, ['blob:test/0']);
    assert.equal(document.querySelectorAll('a[download]').length, 0, 'the helper anchor is gone afterwards');
  } finally {
    seen.restore();
  }
  screen.dispose();
});

test('a failed export reports and downloads nothing', async () => {
  const screen = fakeScreen({});
  const seen = captureDownloads();
  try {
    assert.equal(await exportConfig(screen), null);
    assert.equal(seen.urls.length, 0);
  } finally {
    seen.restore();
  }
  screen.dispose();
});

test('picked files must parse as JSON', async () => {
  assert.equal(await readJsonFile({ name: 'ok.json', text: async () => CONFIG_JSON }), CONFIG_JSON);
  await assert.rejects(readJsonFile({ name: 'broken.json', text: async () => '{not json' }), /broken\.json/);
});

test('the plan renders one row per item with action chips, a summary and the warnings', async () => {
  const host = document.createElement('div');
  document.body.appendChild(host);
  const plan = {
    items: [
      { kind: 'pool', name: 'tank', action: 'skip', detail: 'already imported' },
      { kind: 'dataset', name: 'tank/dokumenty', action: 'create', detail: '' },
      { kind: 'share', name: 'dokumenty', action: 'import', detail: 'SMB' },
      { kind: 'share_user', name: 'anna', action: 'missing', detail: 'password must be set again' },
      { kind: 'schedule', name: 'snap-daily', action: 'update', detail: '' },
    ],
    warnings: ['Hasła użytkowników nie są częścią kopii.'],
  };
  renderImportPlan(host, plan);
  await flush();

  const table = host.querySelector('#nas-ci-plan');
  assert.equal(table.rows.length, 5);
  assert.match(table.rows[0].kind, /Pula/);
  assert.match(table.rows[1].name, /tank\/dokumenty/);
  assert.deepEqual(table.rows[2].action, { status: 'ok', label: 'import', dot: true });
  assert.equal(table.rows[3].action.status, 'warn');
  assert.equal(table.rows[4].action.status, 'info');
  assert.equal(table.rows[3].detail, 'password must be set again');
  const chips = [...host.querySelectorAll('#nas-ci-summary tf-chip')].map((c) => c.getAttribute('label'));
  assert.deepEqual(chips, ['import: 1', 'utwórz: 1', 'aktualizuj: 1', 'pomiń: 1', 'brak: 1']);
  assert.equal(host.querySelectorAll('.wizard-warning.info').length, 1);
  assert.match(host.querySelector('.wizard-warning.info').textContent, /Hasła/);
  assert.ok(!host.querySelector('.wizard-warning.danger'));
  assert.deepEqual(planCounts(plan.items), { skip: 1, create: 1, import: 1, missing: 1, update: 1 });
  assert.ok(!planBlocked(plan.items));

  renderImportPlan(host, { items: [{ kind: 'share', name: 'dokumenty', action: 'conflict', detail: 'exists with a different source' }], warnings: [] });
  assert.ok(host.querySelector('.wizard-warning.danger'), 'a conflict is called out');
  assert.ok(planBlocked([{ action: 'conflict' }]));

  renderImportPlan(host, { items: [], warnings: [] });
  assert.ok(!host.querySelector('#nas-ci-plan'));
  assert.match(host.querySelector('#nas-ci-summary').textContent, /\S/);
  host.remove();
});

test('the import dialog plans the picked file, blocks on conflicts and applies through sudo', async () => {
  const job = { jobId: 'job-11', kind: 'config_import', subject: 'tentanas-orion.json' };
  const screen = fakeScreen({
    tentaNasConfigImportPlanRequest: (p) => (p.json.includes('conflict')
      ? { items: [{ kind: 'share', name: 'dokumenty', action: 'conflict', detail: 'source differs' }], warnings: [] }
      : { items: [{ kind: 'share', name: 'dokumenty', action: 'import', detail: '' }], warnings: ['w1'] }),
    tentaNasConfigImportApplyRequest: { job },
  });
  let finished = false;
  const win = openConfigImportDialog(screen, { onDone: () => { finished = true; } });
  await flush();
  const confirm = win.querySelector('[data-action="confirm"]');
  assert.ok(confirm.hasAttribute('disabled'), 'nothing to apply before a file is picked');

  pickFile(win, 'broken.json', '{oops');
  await flush();
  await flush();
  assert.equal(screen.calls.length, 0, 'a non-JSON file is rejected before asking the node');
  assert.equal(win.querySelector('#nas-ci-status').className, 'num-err');
  assert.ok(confirm.hasAttribute('disabled'));

  const conflictJson = JSON.stringify({ version: 1, note: 'conflict' });
  pickFile(win, 'conflict.json', conflictJson);
  await flush();
  await flush();
  assert.deepEqual(screen.calls.at(-1), { kind: 'tentaNasConfigImportPlanRequest', payload: { json: conflictJson } });
  assert.ok(win.querySelector('.wizard-warning.danger'));
  assert.ok(confirm.hasAttribute('disabled'), 'a conflicting plan cannot be applied');
  confirmWindow(win);
  await flush();
  assert.ok(!screen.calls.some((c) => c.kind === 'tentaNasConfigImportApplyRequest'));

  pickFile(win, 'tentanas-orion.json', CONFIG_JSON);
  await flush();
  await flush();
  assert.equal(win.querySelector('#nas-ci-plan').rows.length, 1);
  assert.match(win.querySelector('#nas-ci-status').textContent, /tentanas-orion\.json/);
  assert.ok(!confirm.hasAttribute('disabled'), 'a clean plan enables Apply');

  confirmWindow(win);
  await flush();
  await flush();
  const apply = screen.calls.find((c) => c.kind === 'tentaNasConfigImportApplyRequest');
  assert.deepEqual(apply.payload, { json: CONFIG_JSON, sudoPassword: 'hunter2' });
  assert.equal(screen.jobLogs.length, 1);
  assert.equal(screen.jobLogs[0].jobId, 'job-11');
  screen.jobLogs[0].onFinish();
  assert.ok(finished);
  await new Promise((r) => setTimeout(r, 260));
  assert.ok(!win.isConnected, 'the dialog closes once the job runs');
  screen.dispose();
});

test('applyImport reports a refused sudo prompt as not started', async () => {
  const screen = fakeScreen({ tentaNasConfigImportApplyRequest: { job: { jobId: 'never', kind: 'config_import', subject: '' } } }, { sudo: null });
  assert.equal(await applyImport(screen, CONFIG_JSON, null), false);
  assert.equal(screen.calls.length, 0);
  assert.equal(screen.jobLogs.length, 0);
  screen.dispose();
});
