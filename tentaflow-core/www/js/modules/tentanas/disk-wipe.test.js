// =============================================================================
// File: modules/tentanas/disk-wipe.test.js
// Description: The guarded "clear this disk" dialog against a fake screen: the
// plan is read before anything is shown, the danger button needs the device
// name retyped, a refused plan can never arm it, and a disk a dissolved
// Elastic Array's journal still claims needs a SECOND acknowledgement that
// travels as the array's NAME. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, typeInto, confirmWindow, click, I18n } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openDiskWipeDialog } = await import('./disk-wipe.js');

const ARRAY_ID = '2a7f1c30-9e64-4b8d-a5f2-71c3e806d914';

const DISK = { diskId: 'wwn-produkt-data', name: 'sdc', role: 'used' };

const plan = (o = {}) => ({
  plan: {
    diskId: 'wwn-produkt-data',
    name: 'sdc',
    path: '/dev/sdc',
    sizeBytes: 32 * 1024 ** 3,
    model: 'HGST HUS726',
    serial: 'K5',
    role: 'used',
    fsType: 'xfs',
    fsLabel: 'd1',
    fsUuid: '33333333-3333-4333-8333-333333333333',
    mountpoints: [],
    refusals: [],
    journalClaim: null,
    allowed: true,
    ...o,
  },
});

const claim = {
  arrayId: ARRAY_ID,
  name: 'produkt',
  arrayRole: 'data',
  memberCount: 9,
  ownerOrgId: 'orgtentanas-rig11',
  ownerAddonId: 'addontentanas',
  // The node's verdict and the node's words for the owner.
  ownerForeign: true,
  // Another instance of the asking organisation, the one kind the node names.
  ownerKind: 'this_org',
  ownerInstanceName: 'NAS Pracownia',
};

const confirmBtn = (win) => win.querySelector('[data-action="confirm"]');
const armed = (win) => !confirmBtn(win).hasAttribute('disabled');

test('the plan is read first and its removal list names the signature and the label, with the UUID as a tooltip', async () => {
  const screen = fakeScreen({
    tentaNasDiskWipePlanRequest: plan(),
    tentaNasDiskWipeRequest: { job: { jobId: 'j1', kind: 'disk_wipe', subject: 'sdc' } },
  });
  const win = await openDiskWipeDialog(screen, DISK, () => {});
  await flush();

  assert.deepEqual(
    screen.calls.map((c) => [c.kind, c.payload.diskId, c.payload.sudoPassword]),
    [['tentaNasDiskWipePlanRequest', 'wwn-produkt-data', 'hunter2']],
    'the plan is the only thing read, and it goes through the sudo prompt',
  );
  // The identity an admin can physically check, beside the name they retype.
  assert.match(win.shadowRoot.querySelector('.tf-window-subtitle-text')?.textContent || '', /\/dev\/sdc/);
  assert.match(win.shadowRoot.querySelector('.tf-window-subtitle-text')?.textContent || '', /S\/N K5/);
  assert.match(win.textContent, /System plików xfs/);
  assert.match(win.textContent, /Etykieta d1/);
  // The UUID identifies the filesystem and names nothing: it is the line's
  // tooltip, never text on the dialog.
  assert.doesNotMatch(win.textContent, /33333333-3333-4333-8333-333333333333/);
  assert.match(win.querySelector('.loss-list [title]').getAttribute('title'), /33333333-3333-4333-8333-333333333333/);
  // The dialog says WHY the node can refuse late, so a kernel EBUSY is not
  // read as a transient fault worth retrying.
  assert.match(win.textContent, /wyłączność/);
  assert.match(win.textContent, /prywatnej przestrzeni branchy/);
  assert.ok(!win.querySelector('#nas-wipe-ack'), 'no journal claim, no acknowledgement');

  assert.ok(!armed(win), 'the button starts disabled');
  typeInto(win.querySelector('#retype-input'), 'sdb');
  assert.ok(!armed(win), 'a near miss does not arm it');
  typeInto(win.querySelector('#retype-input'), 'sdc');
  assert.ok(armed(win), 'the retyped device name arms it');

  confirmWindow(win);
  await flush();
  const wipe = screen.calls.find((c) => c.kind === 'tentaNasDiskWipeRequest');
  assert.deepEqual(wipe.payload, {
    diskId: 'wwn-produkt-data',
    confirmDevice: 'sdc',
    releaseJournalArray: '',
    sudoPassword: 'hunter2',
  });
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['j1'], 'the job log follows the answer');
});

test('a refused plan shows the node’s own sentence and can never arm the button', async () => {
  const screen = fakeScreen({
    tentaNasDiskWipePlanRequest: plan({
      role: 'pool_member',
      allowed: false,
      refusals: [{
        code: 'zfs_pool',
        detail: 'sdc: dysk należy do puli ZFS tank — zniszcz pulę albo odłącz od niej ten dysk, a potem wyczyść go ponownie',
      }],
    }),
  });
  const win = await openDiskWipeDialog(screen, DISK, () => {});
  await flush();

  assert.match(win.textContent, /puli ZFS tank/, 'the refusal is the node’s sentence, verbatim');
  assert.match(win.textContent, /zniszcz pulę/, 'and it names the remedy');
  typeInto(win.querySelector('#retype-input'), 'sdc');
  assert.ok(!armed(win), 'a retyped name cannot override a refusal');
  confirmWindow(win);
  await flush();
  assert.ok(
    !screen.calls.some((c) => c.kind === 'tentaNasDiskWipeRequest'),
    'and nothing is sent',
  );
});

test('a dissolved array’s journal claim needs its own acknowledgement, sent as the array name', async () => {
  const screen = fakeScreen({
    tentaNasDiskWipePlanRequest: plan({ journalClaim: claim }),
    tentaNasDiskWipeRequest: { job: { jobId: 'j2', kind: 'disk_wipe', subject: 'sdc' } },
  });
  const win = await openDiskWipeDialog(screen, DISK, () => {});
  await flush();

  assert.match(win.textContent, /Rezerwacja dziennika/);
  assert.match(win.textContent, /produkt/);
  assert.match(win.textContent, /9 dyskami/, 'how much of the array is being given up');
  // Whose array it is — in words. The ids are the tooltip, never the text.
  assert.match(win.textContent, /Właściciel zapisany w dzienniku: instancja „NAS Pracownia” tej organizacji/, 'whose array it is');
  assert.doesNotMatch(win.textContent, /orgtentanas-rig11|addontentanas/);
  assert.equal(win.querySelector('.wizard-warning [title]').getAttribute('title'), 'orgtentanas-rig11 / addontentanas');
  assert.match(win.textContent, /import/i, 'and that an import would otherwise recover it');

  // The retyped device name alone is NOT enough: the second victim is the
  // array's recoverability, which the device name says nothing about.
  typeInto(win.querySelector('#retype-input'), 'sdc');
  assert.ok(!armed(win), 'the typed device name alone does not arm the button');

  const ack = win.querySelector('#nas-wipe-ack');
  assert.ok(ack, 'the acknowledgement is offered');
  assert.match(ack.getAttribute('label'), /produkt/, 'and it names the array');
  click(ack.shadowRoot?.querySelector('label') || ack.querySelector('label'));
  await flush();
  assert.ok(armed(win), 'both confirmations arm it');

  confirmWindow(win);
  await flush();
  const wipe = screen.calls.find((c) => c.kind === 'tentaNasDiskWipeRequest');
  assert.equal(wipe.payload.releaseJournalArray, 'produkt', 'the acknowledgement travels as a name');
  assert.equal(wipe.payload.confirmDevice, 'sdc');
});

// The owner ids are ALWAYS filled in, so reading their presence as "foreign"
// called this instance's own dissolved arrays another instance's. Foreign is
// the node's verdict.
test('this instance’s own journal is not called another instance’s', async () => {
  const screen = fakeScreen({
    tentaNasDiskWipePlanRequest: plan({ journalClaim: { ...claim, ownerForeign: false, ownerKind: 'this_instance', ownerInstanceName: '' } }),
  });
  const win = await openDiskWipeDialog(screen, DISK, () => {});
  await flush();
  assert.match(win.textContent, /Rezerwacja dziennika/, 'the claim itself is still shown');
  assert.doesNotMatch(win.textContent, /Właściciel zapisany w dzienniku/);
  assert.doesNotMatch(win.textContent, /orgtentanas-rig11|addontentanas/);
});

// The node never sends another organisation's names, and the dialog must not
// make one up either: whatever arrives next to `other_installation` is not
// printed, and the sentence is composed here from the code — in every locale.
test('another organisation’s journal is “another installation”, never named, and the sentence follows the locale', async () => {
  const foreignClaim = { ...claim, ownerKind: 'other_installation', ownerInstanceName: 'Obca Firma NAS' };
  const screen = fakeScreen({ tentaNasDiskWipePlanRequest: plan({ journalClaim: foreignClaim }) });
  const win = await openDiskWipeDialog(screen, DISK, () => {});
  await flush();
  assert.match(win.textContent, /Właściciel zapisany w dzienniku: inna instalacja TentaNas\./);
  assert.doesNotMatch(win.textContent, /Obca Firma/);
  // Nor its ids: an older node may still send them, and the tooltip drops them.
  assert.doesNotMatch(win.innerHTML, /orgtentanas-rig11|addontentanas/);
  win.remove();

  await I18n.setLanguage('en');
  try {
    const en = await openDiskWipeDialog(fakeScreen({ tentaNasDiskWipePlanRequest: plan({ journalClaim: claim }) }), DISK, () => {});
    await flush();
    assert.match(en.textContent, /the “NAS Pracownia” instance of this organisation/);
    assert.doesNotMatch(en.textContent, /tej organizacji|instancja/, 'no Polish phrase in an English UI');
    en.remove();
  } finally {
    await I18n.setLanguage('pl');
  }
});

// A node from before the owner code sends only the flag: a foreign owner is
// then the phrase that claims nothing it does not know.
test('an owner without a code from an older node reads as another installation', async () => {
  const older = { ...claim, ownerKind: undefined, ownerInstanceName: undefined };
  const win = await openDiskWipeDialog(fakeScreen({ tentaNasDiskWipePlanRequest: plan({ journalClaim: older }) }), DISK, () => {});
  await flush();
  assert.match(win.textContent, /Właściciel zapisany w dzienniku: inna instalacja TentaNas\./);
  win.remove();
});

test('a claim on a plan the node already refused is never offered for acknowledgement', async () => {
  const screen = fakeScreen({
    tentaNasDiskWipePlanRequest: plan({
      allowed: false,
      journalClaim: claim,
      refusals: [{ code: 'journal_serving', detail: 'sdc: macierz produkt nadal udostępnia unię' }],
    }),
  });
  const win = await openDiskWipeDialog(screen, DISK, () => {});
  await flush();
  assert.ok(!win.querySelector('#nas-wipe-ack'), 'nothing to acknowledge on a refused plan');
  typeInto(win.querySelector('#retype-input'), 'sdc');
  assert.ok(!armed(win));
});

// Owner's rule, 2026-09-22: a disk of ANOTHER ORGANISATION on this node is
// refused with the code `journal_other_org`, and nothing names its array. The
// dialog words that refusal itself, in the admin's language, from the code —
// not from the node's Polish sentence — and says no array name.
test('another organisation’s disk on this node is refused in the admin’s language and names no array', async () => {
  const refusal = { code: 'journal_other_org', detail: 'sdc: dysk należy do macierzy Elastic innej organizacji na tym nodzie — ta organizacja nie może go wyczyścić ani przejąć' };
  const screen = fakeScreen({ tentaNasDiskWipePlanRequest: plan({ allowed: false, refusals: [refusal] }) });
  const win = await openDiskWipeDialog(screen, DISK, () => {});
  await flush();
  assert.match(win.textContent, /sdc: dysk należy do macierzy Elastic innej organizacji na tym nodzie\. Ta organizacja nie może go wyczyścić ani przejąć tej macierzy\./);
  assert.ok(!win.querySelector('#nas-wipe-ack'), 'nothing to acknowledge');
  typeInto(win.querySelector('#retype-input'), 'sdc');
  assert.ok(!armed(win), 'refused, however carefully the name is typed');
  win.remove();

  await I18n.setLanguage('en');
  try {
    const en = await openDiskWipeDialog(fakeScreen({ tentaNasDiskWipePlanRequest: plan({ allowed: false, refusals: [refusal] }) }), DISK, () => {});
    await flush();
    assert.match(en.textContent, /sdc belongs to an Elastic Array of another organisation on this node/);
    assert.doesNotMatch(en.textContent, /innej organizacji/, 'the node’s Polish sentence is not what an English admin reads');
    en.remove();
  } finally {
    await I18n.setLanguage('pl');
  }
});

// The node sends no claim for another tenant's journal. A claim of that kind
// that arrived anyway (a node with a bug, a hand-built frame) must not print
// that tenant's array name, nor offer to release its journal.
test('a claim of another organisation on this node is never shown, even on a plan that arrives allowed', async () => {
  const leaked = { ...claim, name: 'cudza-macierz', ownerKind: 'other_org_on_node' };
  const win = await openDiskWipeDialog(fakeScreen({ tentaNasDiskWipePlanRequest: plan({ journalClaim: leaked }) }), DISK, () => {});
  await flush();
  assert.doesNotMatch(win.innerHTML, /cudza-macierz/);
  assert.ok(!win.querySelector('#nas-wipe-ack'));
  win.remove();
});

test('a cancelled sudo prompt reads nothing and opens nothing', async () => {
  // The dialogs the earlier tests opened are still in the document, and what
  // matters here is that THIS call added none.
  document.body.innerHTML = '';
  const screen = fakeScreen({ tentaNasDiskWipePlanRequest: plan() }, { sudo: null });
  assert.equal(await openDiskWipeDialog(screen, DISK, () => {}), null);
  assert.equal(screen.calls.length, 0, 'the plan was never read');
  assert.equal(document.querySelectorAll('tf-window').length, 0);
});

test('a plan shape this build cannot read never becomes an armed danger button', async () => {
  document.body.innerHTML = '';
  for (const answer of [{}, { plan: {} }, { plan: { name: 'sdc' } }, { plan: { name: '', refusals: [] } }]) {
    const screen = fakeScreen({ tentaNasDiskWipePlanRequest: answer });
    await assert.rejects(() => openDiskWipeDialog(screen, DISK, () => {}));
    assert.equal(document.querySelectorAll('tf-window').length, 0);
  }
});
