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

const { openShareWizard, shareNameValid, parseNetworks, fleetPlan, rdmaFeature, rdmaAvailable, ksmbdFeature, smbDirectAvailable } = await import('./share-wizard.js');

// The Environment tab's RDMA row as the probe reports it (§5.5a).
const rdmaEnv = (status, detail = '') => ({ features: [{ id: 'nfs', status: 'ok' }, { id: 'rdma', status, detail }] });
// The ksmbd row (§5.4b). It carries the exposure guard too, so 'exposed'
// is a real status a node reports with an RDMA card that works.
const ksmbdEnv = (status, detail = '') => ({ features: [{ id: 'rdma', status: 'ok' }, { id: 'ksmbd', status, detail }] });

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
  assert.match(win.querySelector('#nas-sw-fleet').closest('.toggle-card').textContent, /\/mnt\/tentanas\/dokumenty/, 'the fleet path sits under the mount toggle');
  assert.match(summaryRows(win).textContent, /dokumenty · SMB/);
  assert.match(summaryRows(win).textContent, /1 użytkownik · goście: wył\. · poprzednie wersje: wł\. · kosz sieciowy: wył\. · Time Machine: wł\./);
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
    smb: { guests: false, previousVersions: true, recycleBin: false, timeMachine: true, smbDirect: false, users: [{ user: 'anna', mode: 'ro' }] },
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
    nfs: { networks: ['10.10.0.0/24', '10.20.0.7'], readOnly: true, rootSquash: true, asyncWrites: true, rdma: false },
    fleetMount: false,
    enabled: true,
    sudoPassword: 'hunter2',
  });
  screen.dispose();
});

test('the RDMA row of the environment decides whether the transport is offerable', () => {
  assert.equal(rdmaFeature(rdmaEnv('ok')).status, 'ok');
  assert.equal(rdmaFeature(undefined), null);
  assert.equal(rdmaFeature({}), null);
  assert.ok(rdmaAvailable(rdmaEnv('ok')));
  assert.ok(!rdmaAvailable(rdmaEnv('no_device')));
  assert.ok(!rdmaAvailable(rdmaEnv('missing_module')));
  assert.ok(!rdmaAvailable(undefined));
});

test('a node with RDMA offers the TCP + RDMA transport and sends it on the share', async () => {
  const screen = screenWith({ tentaNasShareCreateRequest: { job: { jobId: 'job-rdma', kind: 'share_create', subject: 'modele' } } });
  screen.environment = rdmaEnv('ok');
  const win = openShareWizard(screen, { users, mountRoot: '/mnt/tentanas' });
  await flush();

  setChoice(win.querySelector('#nas-sw-protocol'), 'nfs');
  typeInto(win.querySelector('#nas-sw-name'), 'modele');
  typeInto(win.querySelector('#nas-sw-source'), '/tank/modele');
  click(nextButton(win));
  await flush();

  const toggle = win.querySelector('#nas-sw-rdma');
  assert.ok(toggle, 'the NFS branch carries the transport option');
  assert.ok(!toggle.hasAttribute('disabled'), 'a probed node may turn it on');
  typeInto(win.querySelector('#nas-sw-networks'), '10.10.0.0/24');
  setToggle(toggle, true);
  await flush();
  // Turning it on says out loud that the listener is the node's, not the
  // share's — nothing about it is silent.
  assert.match(win.textContent, /Nasłuch RDMA/);

  click(nextButton(win));
  await flush();
  assert.match(summaryRows(win).textContent, /RDMA/, 'the summary names the transport');
  click(nextButton(win));
  await flush();
  await flush();

  const call = screen.calls.find((c) => c.kind === 'tentaNasShareCreateRequest');
  assert.equal(call.payload.nfs.rdma, true);
  screen.dispose();
});

test('a node whose probe found no RDMA device shows the option disabled with the reason', async () => {
  const screen = screenWith({});
  screen.environment = rdmaEnv('no_device', 'no RDMA device under /sys/class/infiniband · rpcrdma available');
  const win = openShareWizard(screen, { users, mountRoot: '/mnt/tentanas' });
  await flush();

  setChoice(win.querySelector('#nas-sw-protocol'), 'nfs');
  typeInto(win.querySelector('#nas-sw-name'), 'modele');
  typeInto(win.querySelector('#nas-sw-source'), '/tank/modele');
  click(nextButton(win));
  await flush();

  const toggle = win.querySelector('#nas-sw-rdma');
  assert.ok(toggle, 'the option stays visible — it is never hidden without a trace');
  assert.ok(toggle.hasAttribute('disabled'));
  assert.ok(!toggle.hasAttribute('checked'));
  assert.match(win.textContent, /nie ma sprawnego urządzenia RDMA/);
  assert.match(win.textContent, /sys\/class\/infiniband/, 'the probe detail is shown verbatim');
  screen.dispose();
});

test('an RDMA share edited on a node that lost its device keeps the stored intent', async () => {
  // The card is down, so the toggle cannot be changed — but an unrelated edit
  // must not quietly rewrite the transport the admin chose; the node degrades
  // that share to TCP by itself and picks RDMA back up with the link.
  const share = {
    shareId: 'sh-2', name: 'modele', protocol: 'nfs', sourcePath: '/tank/modele', dataset: 'tank/modele', enabled: true, fleetMount: true,
    smb: null, nfs: { networks: ['10.10.0.0/24'], readOnly: false, rootSquash: true, asyncWrites: false, rdma: true },
  };
  const screen = screenWith({ tentaNasShareUpdateRequest: { job: { jobId: 'job-10', kind: 'share_update', subject: 'modele' } } });
  screen.environment = rdmaEnv('no_device', 'mlx5_0 DOWN (enp1s0f0np0 10.10.0.5) · rpcrdma loaded');
  const win = openShareWizard(screen, { share, users });
  await flush();
  const toggle = win.querySelector('#nas-sw-rdma');
  assert.ok(toggle.hasAttribute('disabled'), 'a down link cannot be switched from here');
  assert.ok(toggle.hasAttribute('checked'), 'the stored choice is still shown');
  assert.match(win.textContent, /mlx5_0 DOWN/, 'and the probe says why it is stuck');
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();
  const call = screen.calls.find((c) => c.kind === 'tentaNasShareUpdateRequest');
  assert.equal(call.payload.nfs.rdma, true);
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
    smb: { guests: true, previousVersions: false, recycleBin: true, timeMachine: false, smbDirect: false, users: [] },
    nfs: null,
    fleetMount: true,
    enabled: false,
    sudoPassword: 'hunter2',
  });
  assert.ok(!('name' in call.payload) && !('sourcePath' in call.payload), 'identity is not part of ShareUpdate');
  assert.equal(screen.jobLogs[0].jobId, 'job-9');
  screen.dispose();
});

test('the ksmbd row of the environment decides whether SMB Direct is offerable', () => {
  assert.equal(ksmbdFeature(ksmbdEnv('ok')).status, 'ok');
  assert.equal(ksmbdFeature(rdmaEnv('ok')), null, 'the RDMA row is not the ksmbd row');
  assert.equal(ksmbdFeature(undefined), null);
  assert.ok(smbDirectAvailable(ksmbdEnv('ok')));
  // A node whose RDMA interface also routes the world is refused BY THE
  // WIZARD, not only by the backend: §5.4b says the option is not offered.
  assert.ok(!smbDirectAvailable(ksmbdEnv('exposed')));
  assert.ok(!smbDirectAvailable(ksmbdEnv('missing')));
  assert.ok(!smbDirectAvailable(ksmbdEnv('no_device')));
  assert.ok(!smbDirectAvailable(undefined));
});

test('a node that may serve SMB Direct offers it, lists what it costs and sends it', async () => {
  const screen = screenWith({ tentaNasShareCreateRequest: { job: { jobId: 'job-sd', kind: 'share_create', subject: 'modele' } } });
  screen.environment = ksmbdEnv('ok');
  const win = openShareWizard(screen, { users, mountRoot: '/mnt/tentanas' });
  await flush();

  typeInto(win.querySelector('#nas-sw-name'), 'modele');
  typeInto(win.querySelector('#nas-sw-source'), '/tank/modele');
  click(nextButton(win));
  await flush();

  const toggle = win.querySelector('#nas-sw-smbdirect');
  assert.ok(toggle, 'the SMB branch carries the SMB Direct option');
  assert.ok(!toggle.hasAttribute('disabled'), 'a probed node may turn it on');
  setToggle(toggle, true);
  await flush();

  // Turning it on says what the RDMA path loses, item by item — the option is
  // a decision, not a speed setting.
  assert.match(win.textContent, /EKSPERYMENTALNY/);
  assert.match(win.textContent, /Bez audytu dostępu/);
  assert.match(win.textContent, /Poprzednich wersji/);
  assert.match(win.textContent, /Bez kosza/);
  assert.match(win.textContent, /Bez Time Machine/);
  assert.match(win.textContent, /ACL ZFS/);
  assert.match(win.textContent, /Multichannel/);

  click(nextButton(win));
  await flush();
  assert.match(summaryRows(win).textContent, /SMB Direct: bez audytu/, 'the summary names it');
  click(nextButton(win));
  await flush();
  await flush();

  const call = screen.calls.find((c) => c.kind === 'tentaNasShareCreateRequest');
  assert.equal(call.payload.smb.smbDirect, true);
  screen.dispose();
});

test('a node whose RDMA interface carries the default gateway cannot turn SMB Direct on', async () => {
  const screen = screenWith({});
  screen.environment = ksmbdEnv('exposed', 'enp3s0 192.168.1.20 also carries the default gateway — SMB Direct needs a dedicated storage network');
  const win = openShareWizard(screen, { users, mountRoot: '/mnt/tentanas' });
  await flush();

  typeInto(win.querySelector('#nas-sw-name'), 'modele');
  typeInto(win.querySelector('#nas-sw-source'), '/tank/modele');
  click(nextButton(win));
  await flush();

  const toggle = win.querySelector('#nas-sw-smbdirect');
  assert.ok(toggle, 'the option stays visible — it is never hidden without a trace');
  assert.ok(toggle.hasAttribute('disabled'));
  assert.ok(!toggle.hasAttribute('checked'));
  assert.match(win.textContent, /nie może serwować SMB Direct/);
  assert.match(win.textContent, /default gateway/, 'the probe detail is shown verbatim');
  // The losses are not listed for an option nobody can turn on.
  assert.ok(!/Bez audytu dostępu/.test(win.textContent));
  screen.dispose();
});

test('an SMB Direct share edited on a node that lost the guard keeps the stored intent', async () => {
  const share = {
    shareId: 'sh-sd', name: 'modele', protocol: 'smb', sourcePath: '/tank/modele', dataset: 'tank/modele', enabled: true, fleetMount: false,
    smb: { guests: false, previousVersions: false, recycleBin: false, timeMachine: false, smbDirect: true, users: [] }, nfs: null,
  };
  const screen = screenWith({ tentaNasShareUpdateRequest: { job: { jobId: 'job-sd2', kind: 'share_update', subject: 'modele' } } });
  screen.environment = ksmbdEnv('exposed', 'enp3s0 also carries the default gateway');
  const win = openShareWizard(screen, { share, users });
  await flush();

  const toggle = win.querySelector('#nas-sw-smbdirect');
  assert.ok(toggle.hasAttribute('checked'), 'the stored decision is shown, not silently cleared');
  assert.ok(toggle.hasAttribute('disabled'));
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();

  const call = screen.calls.find((c) => c.kind === 'tentaNasShareUpdateRequest');
  assert.equal(call.payload.smb.smbDirect, true, 'an unrelated edit does not rewrite the choice');
  screen.dispose();
});
