// =============================================================================
// File: modules/tentanas/elastic-import.test.js
// Description: The Elastic Array import dialog against a fake screen: the scan
// is a separate explicit action (it needs sudo), each candidate shows the
// filesystem, the disk counts, the journal owner and — when it is incomplete —
// exactly which disks are missing or reused, adopting needs the array name
// retyped, and a re-scan patches only the row whose data changed. Runs under
// happy-dom.
// =============================================================================

import { fakeScreen, flush, typeInto, confirmWindow, click } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openElasticImportDialog } = await import('./pools.js');

const MEDIA = '2a7f1c30-9e64-4b8d-a5f2-71c3e806d914';
const ARCHIVE = '7b1e2d40-8a53-4c9e-b6d1-52f4a917c825';

const candidate = (o) => ({
  arrayId: MEDIA,
  name: 'media',
  filesystem: 'xfs',
  ownerOrgId: 'orgtentanas-rig11',
  ownerAddonId: 'addontentanas',
  dataDisks: 2,
  parityDisks: 1,
  cacheDisks: 0,
  disksMatched: 3,
  disksMissing: [],
  disksReused: [],
  unionMounted: true,
  status: 'importable',
  detail: 'all 3 members still carry the filesystem UUID the journal recorded',
  ...o,
});

const incomplete = candidate({
  arrayId: ARCHIVE,
  name: 'archiwum',
  dataDisks: 2,
  parityDisks: 1,
  disksMatched: 2,
  disksMissing: ['d2 · dev-sdd (S/N WD-7788)'],
  unionMounted: false,
  status: 'incomplete',
  detail: '2 of 3 members still carry the filesystem UUID the journal recorded',
});

const rows = (win) => [...win.querySelectorAll('.vdev-group[data-array]')];
const row = (win, id) => win.querySelector(`.vdev-group[data-array="${id}"]`);
const adoptButton = (el) => el.querySelector('[data-act="adopt"]');
const lastWindow = () => [...document.querySelectorAll('tf-window')].at(-1);

test('the scan is a separate action and its result names the owner, the counts and the disks it refuses on', async () => {
  const screen = fakeScreen({
    tentaNasElasticArrayImportScanRequest: { candidates: [candidate({}), incomplete] },
  });
  const win = openElasticImportDialog(screen, () => {});
  await flush();

  // Opening a dialog must never be what makes a node ask for a root password.
  assert.equal(screen.calls.length, 0, 'nothing is scanned on open');
  assert.equal(rows(win).length, 0);
  assert.match(win.querySelector('#nas-eimport-status').textContent, /nie został jeszcze uruchomiony/);

  click(win.querySelector('[data-act="scan"]'));
  await flush();
  assert.deepEqual(
    screen.calls.map((c) => [c.kind, c.payload.sudoPassword]),
    [['tentaNasElasticArrayImportScanRequest', 'hunter2']],
    'the scan goes through the sudo prompt',
  );
  assert.equal(rows(win).length, 2);

  const media = row(win, MEDIA);
  assert.match(media.textContent, /Elastic · xfs/);
  assert.match(media.textContent, /dane: 2 · parity: 1 · cache: 0/);
  assert.match(media.textContent, /orgtentanas-rig11\/addontentanas/, 'whose array this is');
  assert.match(media.textContent, /UUID potwierdzony na 3 z 3 dysków/);
  assert.match(media.textContent, /unia zamontowana/, 'it is serving right now');
  assert.ok(!adoptButton(media).hasAttribute('disabled'), 'an importable array can be adopted');

  // An incomplete candidate has to say WHICH disk stops it, not just that
  // something does.
  const archive = row(win, ARCHIVE);
  assert.match(archive.textContent, /Brak dysków: d2 · dev-sdd \(S\/N WD-7788\)/);
  assert.ok(!/unia zamontowana/.test(archive.textContent));
  assert.ok(adoptButton(archive).hasAttribute('disabled'), 'and it cannot be adopted');

  click(adoptButton(archive));
  await flush();
  assert.equal(document.querySelectorAll('tf-window').length, 1, 'a disabled adopt opens nothing');

  win.remove();
  screen.dispose();
});

test('an empty scan says so, and a cancelled sudo prompt leaves the dialog saying it has not looked', async () => {
  const empty = fakeScreen({ tentaNasElasticArrayImportScanRequest: { candidates: [] } });
  const first = openElasticImportDialog(empty, () => {});
  await flush();
  click(first.querySelector('[data-act="scan"]'));
  await flush();
  assert.match(first.querySelector('#nas-eimport-status').textContent, /Nie znaleziono macierzy/);
  first.remove();
  empty.dispose();

  const cancelled = fakeScreen({ tentaNasElasticArrayImportScanRequest: { candidates: [candidate({})] } }, { sudo: null });
  const second = openElasticImportDialog(cancelled, () => {});
  await flush();
  click(second.querySelector('[data-act="scan"]'));
  await flush();
  assert.equal(cancelled.calls.length, 0, 'the request never left');
  assert.match(second.querySelector('#nas-eimport-status').textContent, /nie został jeszcze uruchomiony/,
    'and "nothing scanned" is not reported as "nothing found"');
  second.remove();
  cancelled.dispose();
});

test('adopting needs the array name retyped and sends the array id it was scanned under', async () => {
  const done = [];
  const screen = fakeScreen({
    tentaNasElasticArrayImportScanRequest: { candidates: [candidate({})] },
    tentaNasElasticArrayImportRequest: { array: { name: 'media', kind: 'elastic-array', state: 'active' } },
  });
  const win = openElasticImportDialog(screen, (res) => done.push(res));
  await flush();
  click(win.querySelector('[data-act="scan"]'));
  await flush();

  click(adoptButton(row(win, MEDIA)));
  await flush();
  const confirm = lastWindow();
  assert.notEqual(confirm, win, 'the adoption asks in its own dialog');
  // Re-owning somebody else's storage has to be said out loud.
  assert.match(confirm.querySelector('.wizard-warning.danger').textContent,
    /orgtentanas-rig11\/addontentanas/);
  assert.match(confirm.querySelector('.explain-box').textContent, /media/);

  const button = confirm.querySelector('tf-button[data-action="confirm"]');
  assert.ok(button.hasAttribute('disabled'), 'locked before the name is typed');
  const input = confirm.querySelector('#nas-retype');
  typeInto(input, 'Media');
  assert.ok(button.hasAttribute('disabled'), 'case matters');
  confirmWindow(confirm);
  await flush();
  assert.equal(screen.calls.length, 1, 'nothing was adopted while locked');

  typeInto(input, 'media');
  assert.ok(!button.hasAttribute('disabled'));
  confirmWindow(confirm);
  await flush();
  const request = screen.calls.at(-1);
  assert.equal(request.kind, 'tentaNasElasticArrayImportRequest');
  assert.deepEqual(
    [request.payload.arrayId, request.payload.confirmName, request.payload.sudoPassword],
    [MEDIA, 'media', 'hunter2'],
    'the array is addressed by id; the typed name travels as the confirmation',
  );
  assert.equal(done.length, 1, 'the caller refreshes once the node answers');

  confirm.remove();
  win.remove();
  screen.dispose();
});

test('a re-scan keeps the node of every row whose data did not change', async () => {
  let answer = [candidate({}), incomplete];
  const screen = fakeScreen({ tentaNasElasticArrayImportScanRequest: () => ({ candidates: answer }) });
  const win = openElasticImportDialog(screen, () => {});
  await flush();
  click(win.querySelector('[data-act="scan"]'));
  await flush();
  const before = { media: row(win, MEDIA), archive: row(win, ARCHIVE) };

  click(win.querySelector('[data-act="scan"]'));
  await flush();
  assert.equal(row(win, MEDIA), before.media, 'an identical re-scan touches no node at all');
  assert.equal(row(win, ARCHIVE), before.archive);

  // The missing disk came back: only that row is rebuilt.
  answer = [candidate({}), candidate({ arrayId: ARCHIVE, name: 'archiwum', unionMounted: false })];
  click(win.querySelector('[data-act="scan"]'));
  await flush();
  assert.equal(row(win, MEDIA), before.media, 'the untouched array keeps its node');
  assert.notEqual(row(win, ARCHIVE), before.archive, 'and the changed one is repainted');
  assert.ok(!adoptButton(row(win, ARCHIVE)).hasAttribute('disabled'));

  win.remove();
  screen.dispose();
});
