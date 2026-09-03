// =============================================================================
// File: modules/tentanas/config-transfer.test.js
// Description: The configuration window against a fake screen: the export
// side fetches the node's JSON on open, shows its tinted preview and hands
// it to the browser from "Pobierz plik" under the backend filename; a
// failed export lands in the preview; the Eksport | Import segment swaps
// the body and footer action; the import side plans the picked file, blocks
// on conflicts and applies through sudo as ConfigImportApply with the job
// log opened — or, when the import overwrites something and four eyes are on
// (§5.10), reports the parked request instead. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, window, windowTitle } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { exportConfig, openConfigWindow, jsonPreviewHtml, readJsonFile, renderImportPlan, planBlocked, planCounts, applyImport } = await import('./config-transfer.js');

const CONFIG_JSON = JSON.stringify({ version: 1, shares: [{ name: 'dokumenty' }] }, null, 2);

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
const setSegment = (win, value) => {
  const seg = win.querySelector('#nas-cfg-segment');
  seg.value = value;
  seg.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value } }));
};

test('the export side previews the node JSON and "Pobierz plik" downloads it under the backend filename', async () => {
  const screen = fakeScreen({ tentaNasConfigExportRequest: { json: CONFIG_JSON, filename: 'tentanas-orion-2026-09-02.json' } });
  const seen = captureDownloads();
  try {
    const win = exportConfig(screen);
    assert.equal(windowTitle(win), 'Konfiguracja TentaNas — eksport / import');
    assert.deepEqual([...win.querySelectorAll('#nas-cfg-segment .tf-seg-opt')].map((o) => o.textContent), ['Eksport', 'Import']);
    assert.equal(win.querySelector('#nas-cfg-export').hidden, false);
    assert.equal(win.querySelector('#nas-cfg-import').hidden, true);
    const download = win.querySelector('[data-act="download"]');
    assert.equal(download.textContent.trim(), 'Pobierz plik');
    assert.ok(download.hasAttribute('disabled'), 'nothing to download before the node answers');
    assert.equal(win.querySelector('[data-act="apply"]').hidden, true);
    await flush();
    await flush();
    assert.deepEqual(screen.calls, [{ kind: 'tentaNasConfigExportRequest', payload: {} }]);
    const preview = win.querySelector('#nas-cfg-preview');
    assert.equal(preview.textContent, CONFIG_JSON, 'the whole short export is previewed');
    assert.ok(preview.querySelector('span.k'), 'keys are tinted');
    assert.ok(preview.querySelector('span.s'), 'strings are tinted');
    assert.ok(!download.hasAttribute('disabled'));

    click(download);
    await flush();
    assert.equal(seen.urls.length, 1);
    assert.equal(seen.urls[0].blob.type, 'application/json');
    assert.equal(await seen.urls[0].blob.text(), CONFIG_JSON);
    assert.equal(seen.clicks.length, 1);
    assert.equal(seen.clicks[0].download, 'tentanas-orion-2026-09-02.json');
    assert.ok(seen.clicks[0].href.endsWith('blob:test/0'));
    assert.ok(seen.clicks[0].connected, 'the anchor is in the document when clicked');
    assert.deepEqual(seen.revoked, ['blob:test/0']);
    assert.equal(document.querySelectorAll('a[download]').length, 0, 'the helper anchor is gone afterwards');
    await new Promise((r) => setTimeout(r, 260));
    assert.ok(!win.isConnected, 'the window closes after the download');
  } finally {
    seen.restore();
  }
  screen.dispose();
});

test('a failed export lands in the preview and keeps the download locked', async () => {
  const screen = fakeScreen({ tentaNasConfigExportRequest: () => { throw new Error('node unreachable'); } });
  const seen = captureDownloads();
  try {
    const win = openConfigWindow(screen);
    await flush();
    await flush();
    const preview = win.querySelector('#nas-cfg-preview');
    assert.equal(preview.className, 'num-err');
    assert.match(preview.textContent, /node unreachable/);
    assert.ok(win.querySelector('[data-act="download"]').hasAttribute('disabled'));
    click(win.querySelector('[data-act="download"]'));
    assert.equal(seen.urls.length, 0);
    win.remove();
  } finally {
    seen.restore();
  }
  screen.dispose();
});

test('the preview cuts long exports and escapes markup inside strings', () => {
  const long = Array.from({ length: 45 }, (_, i) => `"k${i}": "v"`).join(',\n');
  assert.ok(jsonPreviewHtml(long).endsWith('\n…'), 'a tail marker after the line cap');
  assert.equal((jsonPreviewHtml(long).match(/class="k"/g) || []).length, 40);
  const html = jsonPreviewHtml('{"name": "<b>x</b>"}');
  assert.ok(!html.includes('<b>'));
  assert.ok(html.includes('&lt;b&gt;'));
});

test('the segment swaps the body and the footer action; the import side opens directly from the import entry', async () => {
  const screen = fakeScreen({ tentaNasConfigExportRequest: { json: CONFIG_JSON, filename: 'x.json' } });
  const win = openConfigWindow(screen);
  await flush();
  setSegment(win, 'import');
  assert.equal(win.querySelector('#nas-cfg-export').hidden, true);
  assert.equal(win.querySelector('#nas-cfg-import').hidden, false);
  assert.equal(win.querySelector('[data-act="download"]').hidden, true);
  assert.equal(win.querySelector('[data-act="apply"]').hidden, false);
  assert.ok(win.querySelector('#nas-ci-picker #nas-ci-file'), 'the picker mounts inside the import side');
  setSegment(win, 'export');
  assert.equal(win.querySelector('#nas-cfg-export').hidden, false);
  assert.equal(win.querySelector('[data-act="apply"]').hidden, true);
  win.remove();

  const imp = openConfigWindow(screen, { segment: 'import' });
  await flush();
  assert.equal(imp.querySelector('#nas-cfg-segment').getAttribute('value'), 'import');
  assert.equal(imp.querySelector('#nas-cfg-import').hidden, false);
  assert.equal(imp.querySelector('[data-act="apply"]').hidden, false);
  imp.remove();
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

test('the import side plans the picked file, blocks on conflicts and applies through sudo', async () => {
  const job = { jobId: 'job-11', kind: 'config_import', subject: 'tentanas-orion.json' };
  const screen = fakeScreen({
    tentaNasConfigExportRequest: { json: CONFIG_JSON, filename: 'x.json' },
    tentaNasConfigImportPlanRequest: (p) => (p.json.includes('conflict')
      ? { items: [{ kind: 'share', name: 'dokumenty', action: 'conflict', detail: 'source differs' }], warnings: [] }
      : { items: [{ kind: 'share', name: 'dokumenty', action: 'import', detail: '' }], warnings: ['w1'] }),
    tentaNasConfigImportApplyRequest: { job },
  });
  let finished = false;
  const win = openConfigWindow(screen, { segment: 'import', onDone: () => { finished = true; } });
  await flush();
  await flush();
  const apply = win.querySelector('[data-act="apply"]');
  assert.equal(apply.textContent.trim(), 'Zastosuj');
  assert.ok(apply.hasAttribute('disabled'), 'nothing to apply before a file is picked');
  const requests = () => screen.calls.filter((c) => c.kind !== 'tentaNasConfigExportRequest');

  pickFile(win, 'broken.json', '{oops');
  await flush();
  await flush();
  assert.equal(requests().length, 0, 'a non-JSON file is rejected before asking the node');
  assert.equal(win.querySelector('#nas-ci-status').className, 'num-err');
  assert.ok(apply.hasAttribute('disabled'));

  const conflictJson = JSON.stringify({ version: 1, note: 'conflict' });
  pickFile(win, 'conflict.json', conflictJson);
  await flush();
  await flush();
  assert.deepEqual(requests().at(-1), { kind: 'tentaNasConfigImportPlanRequest', payload: { json: conflictJson } });
  assert.ok(win.querySelector('.wizard-warning.danger'));
  assert.ok(apply.hasAttribute('disabled'), 'a conflicting plan cannot be applied');
  click(apply);
  await flush();
  assert.ok(!screen.calls.some((c) => c.kind === 'tentaNasConfigImportApplyRequest'));

  pickFile(win, 'tentanas-orion.json', CONFIG_JSON);
  await flush();
  await flush();
  assert.equal(win.querySelector('#nas-ci-plan').rows.length, 1);
  assert.match(win.querySelector('#nas-ci-status').textContent, /tentanas-orion\.json/);
  assert.ok(!apply.hasAttribute('disabled'), 'a clean plan enables Apply');

  click(apply);
  await flush();
  await flush();
  const applied = screen.calls.find((c) => c.kind === 'tentaNasConfigImportApplyRequest');
  assert.deepEqual(applied.payload, { json: CONFIG_JSON, sudoPassword: 'hunter2' });
  assert.equal(screen.jobLogs.length, 1);
  assert.equal(screen.jobLogs[0].jobId, 'job-11');
  screen.jobLogs[0].onFinish();
  assert.ok(finished);
  await new Promise((r) => setTimeout(r, 260));
  assert.ok(!win.isConnected, 'the window closes once the job runs');
  screen.dispose();
});

test('applyImport reports a refused sudo prompt as not started', async () => {
  const screen = fakeScreen({ tentaNasConfigImportApplyRequest: { job: { jobId: 'never', kind: 'config_import', subject: '' } } }, { sudo: null });
  assert.equal(await applyImport(screen, CONFIG_JSON, null), false);
  assert.equal(screen.calls.length, 0);
  assert.equal(screen.jobLogs.length, 0);
  screen.dispose();
});

test('an overwriting import that goes to a second admin reports the parked request instead of a job', async () => {
  document.body.innerHTML = '';
  const approval = {
    requestId: 'r-11', operation: 'config_import', subject: 'helios',
    detail: 'nadpisuje 2: scrub tank, snapshot tank/home', status: 'pending',
    requestedBy: 'u-anna', requestedAt: '2026-09-03 10:00:00',
    expiresAt: new Date(Date.now() + 3600_000).toISOString(),
    decidedBy: null, decidedAt: null, decisionNote: '', decisionJobId: null, isOwnRequest: true,
  };
  let done = 0;
  const screen = fakeScreen({ tentaNasConfigImportApplyRequest: { approval } });
  const ok = await applyImport(screen, '{"schema":1}', () => { done += 1; });
  await flush();
  assert.equal(ok, true, 'the wizard treats a parked request as a finished step');
  assert.equal(done, 1);
  assert.equal(screen.jobLogs.length, 0, 'nothing executed, so there is no job log');
  const win = [...document.querySelectorAll('tf-window')].pop();
  assert.match(win.textContent, /Nic jeszcze nie zostało wykonane/);
  assert.match(win.textContent, /Import konfiguracji/);
  screen.dispose();
});
