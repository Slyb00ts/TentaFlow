// =============================================================================
// File: modules/tentanas/share-wizard.test.js
// Description: The share wizard against a fake screen: name and source
// validation gating the first step, the SMB grant editor versus the NFS
// network list, the fleet plan derived from the fleet nodes, the request
// payload sent to ShareCreate through sudo, and the edit mode that reuses the
// access and fleet steps with the identity read-only. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, typeInto, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const { openShareWizard, shareNameValid, parseNetworks, fleetPlan } = await import('./share-wizard.js');

const fleet = [
  { nodeId: 'node-orion', nodeName: 'orion', instanceStatus: 'ready', elevationMode: 'armed' },
  { nodeId: 'node-atlas', nodeName: 'atlas', instanceStatus: 'ready', elevationMode: 'armed' },
  { nodeId: 'node-helios', nodeName: 'helios', instanceStatus: 'ready', elevationMode: 'unarmed' },
  { nodeId: 'node-tabbie', nodeName: 'tabbie', instanceStatus: 'missing', elevationMode: null },
];
const users = [{ name: 'anna', description: 'Anna K.', shares: [] }, { name: 'backup', description: '', shares: [] }];

function screenWith(fixtures, opts) {
  const screen = fakeScreen(fixtures, opts);
  screen.nodes = fleet;
  return screen;
}

const setChoice = (group, value) => group.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value } }));
const setToggle = (toggle, checked) => {
  toggle.checked = checked;
  toggle.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked } }));
};
const nextButton = (win) => win.querySelector('[data-wizard-next]');
const windowTitle = (win) => win.shadowRoot.querySelector('.tf-window-title-text').textContent;
// tf-window detaches itself once its closing animation has run.
const settled = () => new Promise((r) => setTimeout(r, 260));
const summaryRows = (win) => [...win.querySelectorAll('.stat-rows')].at(-1);

test('name and network parsing helpers', () => {
  assert.ok(shareNameValid('dokumenty'));
  assert.ok(shareNameValid('Backup_2026-Q3'));
  assert.ok(!shareNameValid(''));
  assert.ok(!shareNameValid('-leading'));
  assert.ok(!shareNameValid('ma spacje'));
  assert.ok(!shareNameValid('x'.repeat(65)));
  assert.deepEqual(parseNetworks('10.10.0.0/24\n192.168.1.5, 10.10.0.0/24;fd00::/64'), ['10.10.0.0/24', '192.168.1.5', 'fd00::/64']);
  assert.deepEqual(parseNetworks(''), []);
});

test('the fleet plan names the source, the unarmed and the NAS-less nodes', () => {
  const plan = fleetPlan(fleet, 'node-orion');
  assert.deepEqual(plan.map((p) => [p.nodeName, p.outcome]), [
    ['orion', 'source'], ['atlas', 'will_mount'], ['helios', 'after_arm'], ['tabbie', 'unsupported'],
  ]);
  assert.deepEqual(fleetPlan(undefined, 'node-orion'), []);
});

test('step one is gated on a valid name and an absolute source path', async () => {
  const screen = screenWith({});
  const win = openShareWizard(screen, { users, mountRoot: '/mnt/tentanas' });
  await flush();

  assert.ok(win.isConnected);
  assert.equal(screen.openWindow, win);
  assert.equal(win.querySelectorAll('.install-step').length, 3);
  assert.ok(win.querySelector('.install-step.active').textContent.includes('Typ'));
  assert.ok(nextButton(win).hasAttribute('disabled'), 'an empty form cannot proceed');
  assert.ok(win.querySelector('[data-wizard-back]').hasAttribute('disabled'));

  const name = win.querySelector('#nas-sw-name');
  const source = win.querySelector('#nas-sw-source');
  typeInto(name, 'zle imie');
  typeInto(source, '/tank/dokumenty');
  assert.ok(name.hasAttribute('error'), 'an invalid name is flagged inline');
  assert.ok(nextButton(win).hasAttribute('disabled'));

  typeInto(name, 'dokumenty');
  assert.ok(!name.hasAttribute('error'));
  assert.ok(!nextButton(win).hasAttribute('disabled'));

  typeInto(source, 'tank/dokumenty');
  assert.ok(nextButton(win).hasAttribute('disabled'), 'a relative source path blocks the step');

  click(win.querySelector('[data-wizard-cancel]'));
  await settled();
  assert.ok(!win.isConnected);
  screen.dispose();
});

test('the SMB branch grants users and sends the create payload through sudo', async () => {
  const job = { jobId: 'job-7', kind: 'share_create', subject: 'dokumenty' };
  const screen = screenWith({ tentaNasShareCreateRequest: { job } });
  let finished = false;
  const win = openShareWizard(screen, { users, mountRoot: '/mnt/tentanas', onDone: () => { finished = true; } });
  await flush();

  assert.equal(win.querySelector('#nas-sw-protocol').getAttribute('value'), 'smb');
  typeInto(win.querySelector('#nas-sw-name'), 'dokumenty');
  typeInto(win.querySelector('#nas-sw-source'), '/tank/dokumenty');
  click(nextButton(win));
  await flush();

  // Step two: the SMB access editor with no grants yet.
  assert.ok(win.querySelector('#nas-sw-guests'));
  assert.ok(!win.querySelector('#nas-sw-networks'), 'the NFS network list is not part of the SMB branch');
  assert.equal(win.querySelectorAll('#nas-sw-grants .sr[data-user]').length, 0);
  assert.ok(!nextButton(win).hasAttribute('disabled'), 'SMB access is complete without grants (guests or empty ACL)');

  const pick = win.querySelector('#nas-sw-grant-pick');
  pick.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'anna' } }));
  click(win.querySelector('[data-act="grant-add"]'));
  await flush();
  const grantRow = win.querySelector('#nas-sw-grants .sr[data-user="anna"]');
  assert.ok(grantRow, 'the granted user gets a row');
  grantRow.querySelector('[data-grant-mode="anna"]').dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value: 'ro' } }));
  setToggle(win.querySelector('#nas-sw-tm'), true);
  setToggle(win.querySelector('#nas-sw-recycle'), false);
  click(nextButton(win));
  await flush();

  // Step three: fleet plan and summary.
  const planRows = [...win.querySelectorAll('#nas-sw-fleet-plan .sr[data-node]')];
  assert.deepEqual(planRows.map((r) => r.dataset.node), ['node-orion', 'node-atlas', 'node-helios', 'node-tabbie']);
  assert.match(planRows[2].textContent, /uzbroj/i, 'the unarmed node explains it mounts after arming');
  assert.ok(!win.querySelector('#nas-sw-enabled'), 'the enabled toggle belongs to edit mode only');
  assert.match(summaryRows(win).textContent, /\/mnt\/tentanas\/dokumenty/);
  assert.match(nextButton(win).textContent, /Utwórz/);

  click(nextButton(win));
  await flush();
  await flush();

  const call = screen.calls.find((c) => c.kind === 'tentaNasShareCreateRequest');
  assert.ok(call, 'ShareCreate was sent');
  assert.deepEqual(call.payload, {
    name: 'dokumenty',
    protocol: 'smb',
    sourcePath: '/tank/dokumenty',
    smb: { guests: false, previousVersions: true, recycleBin: false, timeMachine: true, users: [{ user: 'anna', mode: 'ro' }] },
    nfs: null,
    fleetMount: true,
    enabled: true,
    sudoPassword: 'hunter2',
  });
  assert.equal(screen.jobLogs.length, 1);
  assert.equal(screen.jobLogs[0].jobId, 'job-7');
  screen.jobLogs[0].onFinish();
  assert.ok(finished);
  await settled();
  assert.ok(!win.isConnected, 'the wizard closes once the job is running');
  screen.dispose();
});

test('the NFS branch needs at least one network and disables the fleet mount on request', async () => {
  const screen = screenWith({ tentaNasShareCreateRequest: { job: { jobId: 'job-8', kind: 'share_create', subject: 'media' } } });
  const win = openShareWizard(screen, { users, mountRoot: '/mnt/tentanas' });
  await flush();

  setChoice(win.querySelector('#nas-sw-protocol'), 'nfs');
  typeInto(win.querySelector('#nas-sw-name'), 'media');
  typeInto(win.querySelector('#nas-sw-source'), '/tank/media');
  click(nextButton(win));
  await flush();

  assert.ok(!win.querySelector('#nas-sw-guests'), 'the SMB editor is not part of the NFS branch');
  const nets = win.querySelector('#nas-sw-networks');
  assert.ok(nets);
  assert.ok(nextButton(win).hasAttribute('disabled'), 'NFS without a network cannot proceed');
  typeInto(nets, '10.10.0.0/24, 10.20.0.7');
  assert.equal(win.querySelectorAll('#nas-sw-network-chips tf-chip').length, 2);
  assert.ok(!nextButton(win).hasAttribute('disabled'));
  setToggle(win.querySelector('#nas-sw-ro'), true);
  setToggle(win.querySelector('#nas-sw-async'), true);
  await flush();
  assert.ok(win.querySelector('.wizard-warning.danger'), 'async writes warn about the loss window');
  click(nextButton(win));
  await flush();

  setToggle(win.querySelector('#nas-sw-fleet'), false);
  await flush();
  assert.ok(!win.querySelector('#nas-sw-fleet-plan'), 'no plan without a fleet mount');
  click(nextButton(win));
  await flush();
  await flush();

  const call = screen.calls.find((c) => c.kind === 'tentaNasShareCreateRequest');
  assert.deepEqual(call.payload, {
    name: 'media',
    protocol: 'nfs',
    sourcePath: '/tank/media',
    smb: null,
    nfs: { networks: ['10.10.0.0/24', '10.20.0.7'], readOnly: true, rootSquash: true, asyncWrites: true },
    fleetMount: false,
    enabled: true,
    sudoPassword: 'hunter2',
  });
  screen.dispose();
});

test('a refused sudo prompt keeps the wizard open and sends nothing', async () => {
  const screen = screenWith({ tentaNasShareCreateRequest: { job: { jobId: 'never', kind: 'share_create', subject: '' } } }, { sudo: null });
  const win = openShareWizard(screen, { users });
  await flush();
  typeInto(win.querySelector('#nas-sw-name'), 'dokumenty');
  typeInto(win.querySelector('#nas-sw-source'), '/tank/dokumenty');
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();
  assert.equal(screen.calls.length, 0);
  assert.equal(screen.jobLogs.length, 0);
  assert.ok(win.isConnected);
  assert.ok(!nextButton(win).hasAttribute('disabled'));
  screen.dispose();
});

test('edit mode starts on the access step with a read-only identity and sends ShareUpdate', async () => {
  const share = {
    shareId: 'sh-1', name: 'dokumenty', protocol: 'smb', sourcePath: '/tank/dokumenty', dataset: 'tank/dokumenty', enabled: true, fleetMount: true,
    smb: { guests: true, previousVersions: false, recycleBin: true, timeMachine: false, users: [{ user: 'anna', mode: 'rw' }] }, nfs: null,
  };
  const screen = screenWith({ tentaNasShareUpdateRequest: { job: { jobId: 'job-9', kind: 'share_update', subject: 'dokumenty' } } });
  const win = openShareWizard(screen, { share, users });
  await flush();

  assert.match(windowTitle(win), /dokumenty/);
  assert.ok(win.querySelector('.install-step.active').textContent.includes('Dostęp'), 'editing skips the identity step');
  assert.ok(win.querySelector('[data-wizard-back]').hasAttribute('disabled'), 'the identity step is not reachable when editing');
  assert.equal(win.querySelectorAll('#nas-sw-grants .sr[data-user]').length, 1);
  click(win.querySelector('[data-grant-remove="anna"]'));
  await flush();
  assert.equal(win.querySelectorAll('#nas-sw-grants .sr[data-user]').length, 0);
  click(nextButton(win));
  await flush();

  const enabled = win.querySelector('#nas-sw-enabled');
  assert.ok(enabled, 'edit mode exposes the enabled switch');
  setToggle(enabled, false);
  assert.match(nextButton(win).textContent, /Zapisz/);
  click(nextButton(win));
  await flush();
  await flush();

  const call = screen.calls.find((c) => c.kind === 'tentaNasShareUpdateRequest');
  assert.deepEqual(call.payload, {
    shareId: 'sh-1',
    smb: { guests: true, previousVersions: false, recycleBin: true, timeMachine: false, users: [] },
    nfs: null,
    fleetMount: true,
    enabled: false,
    sudoPassword: 'hunter2',
  });
  assert.ok(!('name' in call.payload) && !('sourcePath' in call.payload), 'identity is not part of ShareUpdate');
  assert.equal(screen.jobLogs[0].jobId, 'job-9');
  screen.dispose();
});
