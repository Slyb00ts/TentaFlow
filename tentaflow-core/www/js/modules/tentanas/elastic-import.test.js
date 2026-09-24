// =============================================================================
// File: modules/tentanas/elastic-import.test.js
// Description: The Elastic Array import dialog against a fake screen: the scan
// is a separate explicit action (it needs sudo), each candidate shows the
// filesystem, the disk counts, the journal owner and — when it is incomplete —
// exactly which disks are missing or reused, adopting needs the array name
// retyped, and a re-scan patches only the row whose data changed. Runs under
// happy-dom.
// =============================================================================

import { fakeScreen, flush, typeInto, confirmWindow, click, I18n } from './_test-setup.js';
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
  // The node decides foreignness against the instance asking and sends a
  // code the dialog words; the ids above (own organisation only) are the
  // tooltip.
  ownerForeign: true,
  // Another instance of the asking organisation, the one kind the node names.
  ownerKind: 'this_org',
  ownerInstanceName: 'NAS Pracownia',
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
  // Structured, as the node sends it: the dialog words it.
  disksMissing: [{ name: 'd2', role: 'data', index: null, diskName: '', serial: 'WD-7788' }],
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
  // Whose array this is — in words. The owner ids are the tooltip only.
  assert.match(media.textContent, /Właściciel w dzienniku: instancja „NAS Pracownia” tej organizacji/, 'whose array this is');
  assert.doesNotMatch(media.textContent, /orgtentanas-rig11|addontentanas/);
  assert.equal(media.querySelector('.hint[title]').getAttribute('title'), 'orgtentanas-rig11 / addontentanas');
  assert.match(media.textContent, /UUID potwierdzony na 3 z 3 dysków/);
  assert.match(media.textContent, /unia zamontowana/, 'it is serving right now');
  assert.ok(!adoptButton(media).hasAttribute('disabled'), 'an importable array can be adopted');

  // An incomplete candidate has to say WHICH disk stops it, not just that
  // something does.
  const archive = row(win, ARCHIVE);
  assert.match(archive.textContent, /Brak dysków: dysk danych 2 \(S\/N WD-7788\)/);
  assert.ok(!/unia zamontowana/.test(archive.textContent));
  assert.ok(adoptButton(archive).hasAttribute('disabled'), 'and it cannot be adopted');

  click(adoptButton(archive));
  await flush();
  assert.equal(document.querySelectorAll('tf-window').length, 1, 'a disabled adopt opens nothing');

  win.remove();
  screen.dispose();
});

// A journal the node cannot load is not hidden from the scan: it is listed as
// unreadable with the node's own reason, and nothing offers to adopt it.
test('an unreadable journal is listed with its reason and cannot be adopted', async () => {
  // Nothing could be read, so the node sends no name — the journal's file
  // name is the array's UUID, and a UUID is not a name.
  const unreadable = candidate({
    arrayId: ARCHIVE,
    name: '',
    ownerOrgId: '',
    ownerAddonId: '',
    ownerKind: '',
    ownerInstanceName: '',
    filesystem: '',
    dataDisks: 0,
    parityDisks: 0,
    disksMatched: 0,
    unionMounted: false,
    status: 'unreadable',
    detail: 'journal: unknown variant `exploded`',
  });
  const screen = fakeScreen({ tentaNasElasticArrayImportScanRequest: { candidates: [candidate({}), unreadable] } });
  const win = openElasticImportDialog(screen, () => {});
  await flush();
  click(win.querySelector('[data-act="scan"]'));
  await flush();
  const listed = row(win, ARCHIVE);
  assert.ok(listed, 'listed, not hidden');
  const chip = listed.querySelector('tf-chip');
  assert.equal(chip.getAttribute('status'), 'err');
  assert.equal(chip.getAttribute('label'), 'nieczytelny dziennik');
  assert.match(listed.querySelector('.num-err').textContent, /unknown variant `exploded`/);
  assert.match(listed.querySelector('.vg-head').textContent, /macierz bez nazwy/);
  assert.doesNotMatch(listed.textContent, new RegExp(ARCHIVE), 'the UUID is not printed');
  assert.equal(listed.querySelector('.vg-head [title]').getAttribute('title'), ARCHIVE, 'it is the tooltip');
  assert.ok(adoptButton(listed).hasAttribute('disabled'));
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
  assert.ok(confirm !== win, 'the adoption asks in its own dialog');
  // Re-owning somebody else's storage has to be said out loud — naming the
  // owner the way the node names it, not by its ids.
  assert.match(confirm.querySelector('.wizard-warning.danger').textContent, /instancja „NAS Pracownia” tej organizacji/);
  assert.doesNotMatch(confirm.textContent, /orgtentanas-rig11|addontentanas/);
  assert.match(confirm.querySelector('.explain-box').textContent, /media/);

  const button = confirm.querySelector('tf-button[data-action="confirm"]');
  assert.ok(button.hasAttribute('disabled'), 'locked before the name is typed');
  const input = confirm.querySelector('#retype-input');
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

// A journal that already names this instance re-owns nothing, so the dialog
// must not warn that it does.
test('adopting an array whose journal already names this instance warns of no re-owning', async () => {
  const own = candidate({ ownerForeign: false, ownerKind: 'this_instance', ownerInstanceName: '' });
  const screen = fakeScreen({ tentaNasElasticArrayImportScanRequest: { candidates: [own] } });
  const win = openElasticImportDialog(screen, () => {});
  await flush();
  click(win.querySelector('[data-act="scan"]'));
  await flush();
  assert.match(row(win, MEDIA).textContent, /Właściciel w dzienniku: ta instancja TentaNas/);
  click(adoptButton(row(win, MEDIA)));
  await flush();
  const confirm = lastWindow();
  assert.equal(Boolean(confirm.querySelector('.wizard-warning.danger')), false, 'no re-own warning');
  assert.match(confirm.textContent, /już wskazuje tę instancję/);
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
  assert.ok(row(win, MEDIA) === before.media, 'an identical re-scan touches no node at all');
  assert.ok(row(win, ARCHIVE) === before.archive);

  // The missing disk came back: only that row is rebuilt.
  answer = [candidate({}), candidate({ arrayId: ARCHIVE, name: 'archiwum', unionMounted: false })];
  click(win.querySelector('[data-act="scan"]'));
  await flush();
  assert.ok(row(win, MEDIA) === before.media, 'the untouched array keeps its node');
  assert.ok(row(win, ARCHIVE) !== before.archive, 'and the changed one is repainted');
  assert.ok(!adoptButton(row(win, ARCHIVE)).hasAttribute('disabled'));

  win.remove();
  screen.dispose();
});

// The members that stop an adoption arrive as DATA and are worded here, by
// the same helper the Elastic detail screen names its members with — so an
// English UI reads English, a reused disk goes by its kernel name, and a
// missing one by its part in the array plus the serial on the drive.
test('missing and reused members are worded in the UI language from structured data', async () => {
  const broken = candidate({
    arrayId: ARCHIVE,
    name: 'archiwum',
    ownerKind: 'other_installation',
    ownerInstanceName: '',
    ownerOrgId: '',
    ownerAddonId: '',
    disksMatched: 1,
    disksMissing: [
      { name: 'd2', role: 'data', index: null, diskName: '', serial: 'WD-7788' },
      { name: 'parity1', role: 'parity', index: 1, diskName: '', serial: '' },
    ],
    disksReused: [{ name: 'c1', role: 'cache', index: null, diskName: 'sdh', serial: 'SN-1' }],
    status: 'incomplete',
  });
  const scanned = async () => {
    const screen = fakeScreen({ tentaNasElasticArrayImportScanRequest: { candidates: [broken] } });
    const win = openElasticImportDialog(screen, () => {});
    await flush();
    click(win.querySelector('[data-act="scan"]'));
    await flush();
    return { screen, win, el: row(win, ARCHIVE) };
  };

  const pl = await scanned();
  assert.match(pl.el.textContent, /Brak dysków: dysk danych 2 \(S\/N WD-7788\), dysk parity 1(?! \()/);
  assert.match(pl.el.textContent, /Dyski użyte ponownie: sdh/);
  assert.doesNotMatch(pl.el.textContent, /SN-1/, 'a disk that is here goes by its kernel name');
  // Another installation: no ids anywhere, the tooltip included.
  assert.equal(pl.el.querySelector('.hint[title]').getAttribute('title'), '');
  pl.win.remove();
  pl.screen.dispose();

  await I18n.setLanguage('en');
  try {
    const en = await scanned();
    assert.doesNotMatch(en.el.textContent, /dysk danych|dysk parity/, 'no Polish member name in an English UI');
    assert.match(en.el.textContent, /WD-7788/);
    en.win.remove();
    en.screen.dispose();
  } finally {
    await I18n.setLanguage('pl');
  }
});

// An older node still sends another installation's ids; the tooltip drops
// them anyway.
test('another installation’s ids never reach the tooltip, even from an older node', async () => {
  const foreign = candidate({ ownerKind: 'other_installation', ownerInstanceName: '' });
  const screen = fakeScreen({ tentaNasElasticArrayImportScanRequest: { candidates: [foreign] } });
  const win = openElasticImportDialog(screen, () => {});
  await flush();
  click(win.querySelector('[data-act="scan"]'));
  await flush();
  assert.doesNotMatch(win.innerHTML, /orgtentanas-rig11|addontentanas/);
  assert.match(row(win, MEDIA).textContent, /Właściciel w dzienniku: inna instalacja TentaNas/);
  win.remove();
  screen.dispose();
});

// Owner's rule, 2026-09-22: another organisation of this node is invisible to
// this one. The node leaves its journals out of the scan; a reply that still
// carried one (a node with a bug, a hand-built frame) must not put that
// tenant's array name on this tenant's screen, nor count it as found. An
// organisation the node has never heard of (`other_installation`) stays.
test('a candidate of another organisation on this node is never listed, while an unknown one is', async () => {
  const hidden = candidate({ arrayId: ARCHIVE, name: 'cudza-macierz', ownerKind: 'other_org_on_node', ownerInstanceName: '' });
  const moved = candidate({ ownerKind: 'other_installation', ownerInstanceName: '', ownerOrgId: '', ownerAddonId: '' });
  const screen = fakeScreen({ tentaNasElasticArrayImportScanRequest: { candidates: [hidden, moved] } });
  const win = openElasticImportDialog(screen, () => {});
  await flush();
  click(win.querySelector('[data-act="scan"]'));
  await flush();
  assert.doesNotMatch(win.innerHTML, /cudza-macierz/);
  assert.ok(row(win, ARCHIVE) === null, 'no row for it');
  assert.deepEqual(rows(win).map((el) => el.dataset.array), [MEDIA]);
  assert.ok(!adoptButton(row(win, MEDIA)).hasAttribute('disabled'), 'the unknown org’s array stays adoptable');
  assert.match(win.querySelector('#nas-eimport-status').textContent, /1/);
  win.remove();
  screen.dispose();
});
