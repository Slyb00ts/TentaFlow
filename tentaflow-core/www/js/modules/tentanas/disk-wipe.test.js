// =============================================================================
// File: modules/tentanas/disk-wipe.test.js
// Description: The guarded "clear this disk" dialog against a fake screen: the
// plan is read before anything is shown, the danger button needs the device
// name retyped, a refused plan can never arm it, and a disk a dissolved
// Elastic Array's journal still claims needs a SECOND acknowledgement that
// travels as the array's NAME. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, typeInto, confirmWindow, click } from './_test-setup.js';
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
};

const confirmBtn = (win) => win.querySelector('[data-action="confirm"]');
const armed = (win) => !confirmBtn(win).hasAttribute('disabled');

test('the plan is read first and its removal list names the signature, the label and the UUID', async () => {
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
  assert.match(win.textContent, /33333333-3333-4333-8333-333333333333/);
  // The dialog says WHY the node can refuse late, so a kernel EBUSY is not
  // read as a transient fault worth retrying.
  assert.match(win.textContent, /wyłączność/);
  assert.match(win.textContent, /prywatnej przestrzeni branchy/);
  assert.ok(!win.querySelector('#nas-wipe-ack'), 'no journal claim, no acknowledgement');

  assert.ok(!armed(win), 'the button starts disabled');
  typeInto(win.querySelector('#nas-retype'), 'sdb');
  assert.ok(!armed(win), 'a near miss does not arm it');
  typeInto(win.querySelector('#nas-retype'), 'sdc');
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
  typeInto(win.querySelector('#nas-retype'), 'sdc');
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
  assert.match(win.textContent, /orgtentanas-rig11/, 'whose array it is');
  assert.match(win.textContent, /import/i, 'and that an import would otherwise recover it');

  // The retyped device name alone is NOT enough: the second victim is the
  // array's recoverability, which the device name says nothing about.
  typeInto(win.querySelector('#nas-retype'), 'sdc');
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
  typeInto(win.querySelector('#nas-retype'), 'sdc');
  assert.ok(!armed(win));
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
