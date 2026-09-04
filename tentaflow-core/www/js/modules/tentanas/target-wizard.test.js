// =============================================================================
// File: modules/tentanas/target-wizard.test.js
// Description: The "Nowy target" wizard (n14) against a fake screen: the three
// steps and their gating, the volume picker that disables an already-exported
// zvol, the portal that is bound to an interface by default and needs a
// deliberate confirmation for 0.0.0.0, the transports the node's probe allows,
// the CHAP / DH-HMAC-CHAP fields, every warning the mockup shows, the summary
// checklist and the request the last step sends. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, typeInto, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';

const {
  openTargetWizard, targetNameValid, parseSize, transportOptions, transportsOf,
  sharedWithoutAuth, AUTH_METHODS, defaultTransport, defaultMethod, parseHostNqns, WWN_AUTHORITY,
  primaryAddress, sharedHostTargets, sharedHostNqns, bindableAddresses, ALL_INTERFACES_ADDRESS,
  invalidHostNqns,
} = await import('./target-wizard.js');

const caps = (over = {}) => ({
  iscsi: true,
  nvmet: true,
  iser: true,
  nvmeRdma: true,
  dhchap: true,
  iscsiDetail: '',
  nvmetDetail: '',
  rdmaDetail: '',
  dhchapDetail: '',
  interfaces: [
    // The LAN comes first on purpose: the wizard must still start on the
    // dedicated one (§5.5b), not on whatever the node listed first.
    { name: 'lan0', address: '192.168.1.5', rdma: false, shared: true, supported: true },
    { name: 'storage0', address: '10.10.0.5', rdma: true, shared: false, supported: true },
  ],
  volumes: [
    { name: 'tank/wolny', pool: 'tank', sizeBytes: 1099511627776, thin: true, devicePath: '/dev/zvol/tank/wolny', exportedBy: '' },
    { name: 'tank/vm-store', pool: 'tank', sizeBytes: 2199023255552, thin: true, devicePath: '/dev/zvol/tank/vm-store', exportedBy: 'vm-store' },
  ],
  ...over,
});

const nextButton = (win) => win.querySelector('[data-wizard-next]');
const setValue = (el, value) => el.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { value } }));
const setChecked = (el, checked) => {
  el.checked = checked;
  el.dispatchEvent(new window.CustomEvent('change', { bubbles: true, detail: { checked } }));
};
const settled = () => new Promise((r) => setTimeout(r, 260));
// tf-select builds a real <select> inside itself and `setOptions` fills that,
// so the options are read from the built element rather than from the light
// DOM the wizard wrote.
const selectOptions = (el) => [...el.querySelectorAll('option')];
const selectValues = (el) => selectOptions(el).map((o) => o.value);
const selectLabels = (el) => selectOptions(el).map((o) => o.textContent);
const disabledValues = (el) => selectOptions(el).filter((o) => o.disabled).map((o) => o.value);

// Walks step 1 with a valid name and lands on step 2.
async function toStepTwo(screen, opts = {}) {
  const win = openTargetWizard(screen, { capabilities: caps(), ...opts });
  await flush();
  typeInto(win.querySelector('#nas-tw-name'), 'vm-store2');
  await flush();
  click(nextButton(win));
  await flush();
  return win;
}

test('the name rule is the lowercase subset an IQN/NQN tail may hold', () => {
  assert.ok(targetNameValid('vm-store2'));
  assert.ok(targetNameValid('vm.store'));
  assert.ok(!targetNameValid('VM-Store'));
  assert.ok(!targetNameValid('2fast'));
  assert.ok(!targetNameValid('vm store'));
  assert.ok(!targetNameValid('vm/store'));
  assert.ok(!targetNameValid(''));
  assert.ok(!targetNameValid('a'.repeat(65)));
});

test('the size parser takes the forms an admin types', () => {
  assert.equal(parseSize('1T'), 1099511627776);
  assert.equal(parseSize('500G'), 536870912000);
  assert.equal(parseSize('1.5T'), 1649267441664);
  assert.equal(parseSize('2048'), 2048);
  assert.equal(parseSize('duzo'), 0);
  assert.equal(parseSize(''), 0);
});

test('the transports offered are the ones the node probed, per protocol', () => {
  // iSER is a flag on the iSCSI portal, so iSCSI has two options and NVMe-oF
  // has three (§5.5a).
  assert.deepEqual(transportOptions('iscsi', caps()).map((t) => [t.value, t.ok]), [['tcp', true], ['iser', true]]);
  assert.deepEqual(transportOptions('nvmet', caps()).map((t) => [t.value, t.ok]), [['tcp', true], ['rdma', true], ['tcp+rdma', true]]);
  const noRdma = caps({ iser: false, nvmeRdma: false });
  assert.deepEqual(transportOptions('iscsi', noRdma).map((t) => t.ok), [true, false]);
  assert.deepEqual(transportOptions('nvmet', noRdma).map((t) => t.ok), [true, false, false]);
  assert.deepEqual(transportsOf('tcp+rdma'), ['tcp', 'rdma']);
  assert.deepEqual(transportsOf('iser'), ['iser']);
  // Each protocol has its own authentication vocabulary.
  assert.deepEqual(AUTH_METHODS.iscsi, ['chap', 'mutual-chap', 'none']);
  assert.deepEqual(AUTH_METHODS.nvmet, ['dhchap', 'dhchap-bidi', 'none']);
});

test('the shared-interface warning fires only for an unauthenticated target on the LAN', () => {
  const c = caps();
  assert.equal(sharedWithoutAuth(c, 'storage0', 'none'), null, 'a dedicated interface is not the LAN');
  assert.equal(sharedWithoutAuth(c, 'lan0', 'chap'), null, 'CHAP is what makes the LAN acceptable');
  assert.equal(sharedWithoutAuth(c, 'lan0', 'none').name, 'lan0');
  // Every interface at once is at least as exposed as the LAN.
  assert.ok(sharedWithoutAuth(c, '', 'none'));
});

test('step one is gated on the name and the wizard shows n14\'s three steps', async () => {
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, { capabilities: caps() });
  await flush();
  assert.equal(win.querySelectorAll('.install-step').length, 3);
  assert.deepEqual(
    [...win.querySelectorAll('.install-step .label')].map((l) => l.textContent),
    ['Typ', 'Źródło i sieć', 'Podsumowanie'],
  );
  assert.ok(win.querySelector('.install-step.active').textContent.includes('Typ'));
  assert.ok(nextButton(win).hasAttribute('disabled'), 'an unnamed target cannot proceed');
  assert.ok(win.querySelector('[data-wizard-back]').hasAttribute('disabled'));
  // Both protocols are offered as cards, exactly as n14a draws them.
  assert.deepEqual(
    [...win.querySelectorAll('tf-choice-card')].map((c) => c.getAttribute('value')),
    ['iscsi', 'nvmet'],
  );

  typeInto(win.querySelector('#nas-tw-name'), 'ZLE');
  await flush();
  assert.ok(win.querySelector('#nas-tw-name').hasAttribute('error'));
  assert.ok(nextButton(win).hasAttribute('disabled'));
  typeInto(win.querySelector('#nas-tw-name'), 'vm-store2');
  await flush();
  assert.ok(!nextButton(win).hasAttribute('disabled'));
  screen.dispose();
});

test('a protocol the node cannot serve is disabled and says why', async () => {
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, {
    capabilities: caps({ nvmet: false, nvmetDetail: 'nvmet is not loaded on this node' }),
  });
  await flush();
  const cards = [...win.querySelectorAll('tf-choice-card')];
  assert.ok(!cards[0].hasAttribute('disabled'));
  assert.ok(cards[1].hasAttribute('disabled'));
  assert.match(win.textContent, /nvmet is not loaded on this node/);
  screen.dispose();
});

test('step two starts on a dedicated interface and offers 0.0.0.0 last', async () => {
  const screen = fakeScreen({});
  const win = await toStepTwo(screen);
  const iface = win.querySelector('#nas-tw-iface');
  // The picker starts on the interface that does NOT carry the default route,
  // even though the node listed the LAN first (§5.5b).
  assert.equal(iface.getAttribute('value'), 'storage0');
  assert.deepEqual(selectValues(iface), ['lan0', 'storage0', '']);
  assert.match(win.textContent, /Zalecany dedykowany interfejs/);
  // A bound portal has no confirmation checkbox at all.
  assert.equal(win.querySelector('#nas-tw-confirm-all'), null);

  // The one warning n14b shows, verbatim in meaning: the IQN is spoofable and
  // only CHAP authenticates.
  assert.match(win.textContent, /IQN initiatora można podszyć/);
  assert.match(win.textContent, /uwierzytelnia tylko CHAP/);

  // Choosing "every interface" adds the danger warning AND blocks the step
  // until the checkbox is ticked: 0.0.0.0 is never a default (§5.5a).
  setValue(iface, '');
  await flush();
  assert.match(win.textContent, /Portal na 0\.0\.0\.0 słucha na każdym interfejsie/);
  // The step is blocked for two independent reasons now — the missing CHAP
  // secret and the unconfirmed 0.0.0.0. Filling the credentials leaves the
  // second one standing, which is what proves it is its own gate.
  typeInto(win.querySelector('#nas-tw-user'), 'vmware01');
  typeInto(win.querySelector('#nas-tw-secret'), 'sekret-inicjatora');
  typeInto(win.querySelector('#nas-tw-muser'), 'helios');
  typeInto(win.querySelector('#nas-tw-msecret'), 'sekret-targetu-1');
  await flush();
  assert.ok(nextButton(win).hasAttribute('disabled'), '0.0.0.0 is refused without a confirmation');
  setChecked(win.querySelector('#nas-tw-confirm-all'), true);
  await flush();
  assert.ok(!nextButton(win).hasAttribute('disabled'));
  screen.dispose();
});

test('an unauthenticated target on the LAN interface is warned about by name', async () => {
  const screen = fakeScreen({});
  const win = await toStepTwo(screen);
  setValue(win.querySelector('#nas-tw-iface'), 'lan0');
  await flush();
  // CHAP on the LAN is not warned about.
  assert.ok(!/interfejs z domyślną trasą/.test(win.textContent));
  setValue(win.querySelector('#nas-tw-auth'), 'none');
  await flush();
  assert.match(win.textContent, /Target bez uwierzytelnienia na lan0/);
  assert.match(win.textContent, /interfejs z domyślną trasą/);
  screen.dispose();
});

test('the volume picker disables a zvol another target already exports', async () => {
  const screen = fakeScreen({});
  const win = await toStepTwo(screen);
  const select = win.querySelector('#nas-tw-volume');
  const labels = selectLabels(select);
  // n14's first AND selected option is the new zvol.
  assert.ok(labels[0].startsWith('+ Nowy zvol'), JSON.stringify(labels));
  assert.equal(select.getAttribute('value'), '');
  assert.ok(labels.some((l) => l.includes('już wyeksportowany (target vm-store)')), JSON.stringify(labels));
  // Two targets on one zvol is two clients writing one raw disk, so the taken
  // one cannot be picked at all.
  assert.deepEqual(disabledValues(select), ['tank/vm-store']);
  screen.dispose();
});

test('CHAP asks for a user and a secret, mutual CHAP for both pairs', async () => {
  const screen = fakeScreen({});
  const win = await toStepTwo(screen);
  // The wizard STARTS on mutual CHAP (n14's active segment), so both pairs are
  // asked for before anything can proceed.
  assert.ok(win.querySelector('#nas-tw-msecret'), 'mutual CHAP asks for the target secret');
  assert.ok(nextButton(win).hasAttribute('disabled'), 'CHAP without a secret cannot proceed');
  typeInto(win.querySelector('#nas-tw-user'), 'vmware01');
  typeInto(win.querySelector('#nas-tw-secret'), 'sekret-inicjatora');
  await flush();
  assert.ok(nextButton(win).hasAttribute('disabled'), 'the target half is still missing');
  typeInto(win.querySelector('#nas-tw-muser'), 'helios');
  typeInto(win.querySelector('#nas-tw-msecret'), 'sekret-targetu-1');
  await flush();
  assert.ok(!nextButton(win).hasAttribute('disabled'));

  // Stepping DOWN to one-way CHAP drops the target's own credentials from the
  // form, and the initiator pair that is already typed is enough.
  setValue(win.querySelector('#nas-tw-auth'), 'chap');
  await flush();
  assert.equal(win.querySelector('#nas-tw-msecret'), null, 'one-way CHAP has no target secret');
  assert.ok(!nextButton(win).hasAttribute('disabled'));

  // "Brak" needs nothing and proceeds — with the warnings, not without them.
  setValue(win.querySelector('#nas-tw-auth'), 'none');
  await flush();
  assert.ok(!nextButton(win).hasAttribute('disabled'));
  assert.equal(win.querySelector('#nas-tw-secret'), null);
  screen.dispose();
});

test('NVMe-oF asks for a DHHC-1 key, names the allowlist rule and says TLS is not here yet', async () => {
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, { capabilities: caps() });
  await flush();
  setValue(win.querySelector('#nas-tw-protocol'), 'nvmet');
  typeInto(win.querySelector('#nas-tw-name'), 'scratch');
  await flush();
  click(nextButton(win));
  await flush();

  // The warning is the NQN one, not the IQN one.
  assert.match(win.textContent, /NQN hosta można podszyć/);
  assert.match(win.textContent, /uwierzytelnia tylko DH-HMAC-CHAP/);
  // The trap the plan did not know about: nvmet keeps the keys on the host
  // objects of the allowlist, so an authenticated subsystem needs one.
  assert.match(win.textContent, /trzyma klucze na obiektach hostów/);
  // TLS for NVMe/TCP is a later phase and the wizard says exactly that.
  assert.match(win.textContent, /TLS dla NVMe\/TCP: niedostępne jeszcze/);
  assert.equal(win.querySelector('#nas-tw-secret').getAttribute('placeholder'), 'DHHC-1:00:…');
  assert.equal(win.querySelector('#nas-tw-user'), null, 'DH-HMAC-CHAP has no user name');

  setValue(win.querySelector('#nas-tw-auth'), 'dhchap-bidi');
  await flush();
  assert.ok(win.querySelector('#nas-tw-msecret'), 'the bidirectional form asks for the controller key');
  screen.dispose();
});

test('a kernel without CONFIG_NVME_TARGET_AUTH offers no DH-HMAC-CHAP and says why', async () => {
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, {
    capabilities: caps({ dhchap: false, dhchapDetail: 'this kernel was built without CONFIG_NVME_TARGET_AUTH' }),
  });
  await flush();
  setValue(win.querySelector('#nas-tw-protocol'), 'nvmet');
  typeInto(win.querySelector('#nas-tw-name'), 'scratch');
  await flush();
  click(nextButton(win));
  await flush();
  assert.match(win.textContent, /CONFIG_NVME_TARGET_AUTH/);
  const auth = win.querySelector('#nas-tw-auth');
  const disabled = [...auth.querySelectorAll('option')].filter((o) => o.hasAttribute('disabled')).map((o) => o.value);
  assert.deepEqual(disabled, ['dhchap', 'dhchap-bidi']);
  screen.dispose();
});

test('an RDMA transport the node cannot serve is disabled with the probe reason', async () => {
  const screen = fakeScreen({});
  const win = await toStepTwo(screen, { capabilities: caps({ iser: false, rdmaDetail: 'no RDMA device under /sys/class/infiniband' }) });
  const options = [...win.querySelector('#nas-tw-transport').querySelectorAll('option')];
  assert.deepEqual(options.map((o) => [o.value, o.hasAttribute('disabled')]), [['tcp', false], ['iser', true]]);
  screen.dispose();
});

test('the summary is n14c: the four rows, the checklist and the raw-disk warning', async () => {
  const screen = fakeScreen({});
  const win = await toStepTwo(screen);
  typeInto(win.querySelector('#nas-tw-user'), 'vmware01');
  typeInto(win.querySelector('#nas-tw-secret'), 'sekret-inicjatora');
  await flush();
  setValue(win.querySelector('#nas-tw-auth'), 'mutual-chap');
  await flush();
  typeInto(win.querySelector('#nas-tw-muser'), 'helios');
  typeInto(win.querySelector('#nas-tw-msecret'), 'sekret-targetu-1');
  await flush();
  click(nextButton(win));
  await flush();

  const rows = [...win.querySelectorAll('.stat-rows .sr .k')].map((k) => k.textContent);
  assert.deepEqual(rows, ['Target', 'LUN0', 'Portal', 'Uwierzytelnienie']);
  assert.match(win.querySelector('#nas-tw-sum-wwn').textContent, /^iqn\.2026-09\.local\.tentaflow:orion\.vm-store2$/);
  assert.match(win.textContent, /10\.10\.0\.5:3260/);
  // The three green checks of n14c.
  const good = [...win.querySelectorAll('.loss-list .ll.good')].map((li) => li.textContent.trim());
  assert.equal(good.length, 3, JSON.stringify(good));
  assert.ok(good.some((t) => t.includes('storage0') && t.includes('nie 0.0.0.0')), JSON.stringify(good));
  assert.ok(good.some((t) => t.includes('Mutual CHAP')), JSON.stringify(good));
  assert.ok(good.some((t) => t.includes('bez saveconfig')), JSON.stringify(good));
  // And the red one the mockup ends on.
  assert.match(win.textContent, /surowy dysk/);
  assert.match(win.textContent, /zniszczy dane/);
  assert.equal(nextButton(win).textContent.trim(), 'Utwórz target');
  screen.dispose();
});

test('an unauthenticated target on 0.0.0.0 turns both checklist rows red', async () => {
  const screen = fakeScreen({});
  const win = await toStepTwo(screen);
  setValue(win.querySelector('#nas-tw-iface'), '');
  await flush();
  setChecked(win.querySelector('#nas-tw-confirm-all'), true);
  await flush();
  setValue(win.querySelector('#nas-tw-auth'), 'none');
  await flush();
  click(nextButton(win));
  await flush();
  const bad = [...win.querySelectorAll('.loss-list .ll.bad')].map((li) => li.textContent.trim());
  assert.ok(bad.some((t) => t.includes('0.0.0.0')), JSON.stringify(bad));
  assert.ok(bad.some((t) => t.includes('filtr, nie login')), JSON.stringify(bad));
  screen.dispose();
});

test('creating sends the wizard choices and opens the job log', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasTargetCreateRequest: (payload) => { sent = payload; return { job: { jobId: 'j1', kind: 'target_create', subject: 'vm-store2' } }; },
  });
  const win = await toStepTwo(screen);
  setValue(win.querySelector('#nas-tw-transport'), 'iser');
  await flush();
  // The wizard starts on mutual CHAP (n14), so both pairs are filled.
  typeInto(win.querySelector('#nas-tw-user'), 'vmware01');
  typeInto(win.querySelector('#nas-tw-secret'), 'sekret-inicjatora');
  typeInto(win.querySelector('#nas-tw-muser'), 'helios');
  typeInto(win.querySelector('#nas-tw-msecret'), 'sekret-targetu-1');
  await flush();
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();

  assert.equal(sent.name, 'vm-store2');
  assert.equal(sent.protocol, 'iscsi');
  // The default volume choice is "+ Nowy zvol", which carries a size.
  assert.equal(sent.source, 'tank/vm-store2');
  assert.equal(sent.createSizeBytes, 1099511627776);
  assert.equal(sent.thin, true);
  assert.equal(sent.portalInterface, 'storage0');
  assert.deepEqual(sent.transports, ['iser']);
  assert.equal(sent.auth.method, 'mutual-chap');
  assert.equal(sent.auth.username, 'vmware01');
  assert.equal(sent.auth.secret, 'sekret-inicjatora');
  assert.equal(sent.auth.mutualUsername, 'helios');
  assert.equal(sent.auth.mutualSecret, 'sekret-targetu-1');
  assert.equal(sent.confirmAllInterfaces, false);
  assert.equal(sent.enabled, true);
  assert.equal(sent.sudoPassword, 'hunter2');
  assert.deepEqual(screen.jobLogs.map((j) => j.jobId), ['j1']);
  await settled();
  screen.dispose();
});

test('choosing an existing volume sends no create size', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasTargetCreateRequest: (payload) => { sent = payload; return { job: { jobId: 'j1', kind: 'target_create', subject: 'x' } }; },
  });
  const win = await toStepTwo(screen);
  setValue(win.querySelector('#nas-tw-volume'), 'tank/wolny');
  await flush();
  setValue(win.querySelector('#nas-tw-auth'), 'none');
  await flush();
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();
  assert.equal(sent.source, 'tank/wolny');
  assert.equal(sent.createSizeBytes, 0);
  assert.equal(sent.auth.method, 'none');
  await settled();
  screen.dispose();
});

test('edit mode starts on step two, keeps the identity read-only and keeps a stored secret', async () => {
  let sent = null;
  const target = {
    targetId: 't1',
    name: 'vm-store',
    protocol: 'iscsi',
    wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store',
    enabled: true,
    luns: [{ index: 0, source: 'tank/vm-store', sizeBytes: 2199023255552, thin: true, groupId: 1, sourceKind: 'zvol' }],
    portals: [{ interface: 'storage0', address: '10.10.0.5', port: 3260, transport: 'tcp' }],
    auth: { method: 'mutual-chap', username: 'vmware01', mutualUsername: 'helios', secretSet: true, mutualSecretSet: true },
    initiators: ['iqn.1998-01.com.vmware:esx01'],
    portGroups: [{ groupId: 1, state: 'optimized', preferred: false }],
  };
  const screen = fakeScreen({
    tentaNasTargetUpdateRequest: (payload) => { sent = payload; return { job: { jobId: 'j9', kind: 'target_update', subject: 'vm-store' } }; },
  });
  const win = openTargetWizard(screen, { target, capabilities: caps() });
  await flush();
  // Straight to "Źródło i sieć": the type and the name are not editable.
  assert.ok(win.querySelector('.install-step.active').textContent.includes('Źródło'));
  assert.ok(win.querySelector('[data-wizard-back]').hasAttribute('disabled'));
  assert.ok(win.querySelector('#nas-tw-volume').hasAttribute('disabled'));
  // A stored secret means the empty fields are allowed to stay empty.
  assert.ok(!nextButton(win).hasAttribute('disabled'), 'a stored secret satisfies the step');

  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();
  assert.equal(sent.targetId, 't1');
  assert.equal(sent.auth.method, 'mutual-chap');
  // Empty secrets mean "keep the stored ones" — never "clear them".
  assert.equal(sent.auth.secret, null);
  assert.equal(sent.auth.mutualSecret, null);
  assert.deepEqual(sent.initiators, ['iqn.1998-01.com.vmware:esx01']);
  assert.deepEqual(sent.portGroups, [{ groupId: 1, state: 'optimized', preferred: false }]);
  assert.deepEqual(sent.portals.map((p) => [p.interface, p.transport]), [['storage0', 'tcp']]);
  // The wizard is the ONE place a portal may change address — but this edit
  // did not touch the portal, so it does not carry the intent. Sending it
  // unconditionally meant that on an interface with a second address, changing
  // a CHAP secret moved a LIVE portal onto the primary one and the prune then
  // `rmdir`-ed the old one under every initiator logged in on it.
  assert.equal(sent.repickPortal, undefined, 'an untouched portal carries no re-pick intent');
  // …and the browser never says what the address IS. That is the node's answer.
  assert.deepEqual(sent.portals.map((p) => p.address), ['']);
  await settled();
  screen.dispose();
});

test('re-picking an interface whose address moved says so before the save', async () => {
  // The drift alert tells the admin to re-pick the interface. Doing that moves
  // the portal onto a different address, and every initiator logged in on the
  // old one loses its path the moment the old portal is removed — so the
  // summary has to say which address it moves from and to. Nothing else in
  // this app may move a portal at all.
  const target = {
    targetId: 't1',
    name: 'vm-store',
    protocol: 'iscsi',
    wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store',
    enabled: true,
    luns: [{ index: 0, source: 'tank/vm-store', sizeBytes: 2199023255552, thin: true, groupId: 1, sourceKind: 'zvol' }],
    // The address storage0 held when the target was made.
    portals: [{ interface: 'storage0', address: '10.10.0.5', port: 3260, transport: 'tcp' }],
    auth: { method: 'none' },
    initiators: [],
    portGroups: [{ groupId: 1, state: 'optimized', preferred: false }],
  };
  // The lease moved: storage0 is on .9 now.
  const moved = caps({
    interfaces: [
      { name: 'lan0', address: '192.168.1.5', rdma: false, shared: true, supported: true },
      { name: 'storage0', address: '10.10.0.9', rdma: true, shared: false, supported: true },
    ],
  });
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, { target, capabilities: moved });
  await flush();
  // Step 2 says what happened and OFFERS the move; it does not take it.
  assert.match(win.textContent, /czeka zamrożony/);
  const box = win.querySelector('#nas-tw-move-portal');
  assert.ok(box, 'the move is offered as a choice');
  assert.ok(!box.hasAttribute('checked'), 'and it starts unticked');
  setChecked(box, true);
  await flush();
  click(nextButton(win));
  await flush();
  const text = win.textContent;
  assert.ok(text.includes('10.10.0.5') && text.includes('10.10.0.9'), text);
  assert.ok(text.includes('stracą ścieżkę'), 'the summary names what the move costs');
  screen.dispose();
});

test('an alias keeps its portal: editing a secret does not move a healthy target', async () => {
  // THE case the re-pick intent exists for. `storage0` carries two addresses;
  // the target was made when only `10.10.0.9` existed, so it sits on the
  // SECOND one and is perfectly healthy — the node's drift check compares
  // against the whole list. Changing the CHAP secret must not move it, because
  // moving it means `rmdir np/10.10.0.9:3260` under every initiator logged in
  // there. Before this, the wizard sent `repickPortal` on every edit and there
  // was no way to say no.
  let sent = null;
  const aliased = caps({
    interfaces: [
      { name: 'lan0', address: '192.168.1.5', rdma: false, shared: true, supported: true },
      { name: 'storage0', address: '10.10.0.5', rdma: true, shared: false, supported: true },
      { name: 'storage0', address: '10.10.0.9', rdma: true, shared: false, supported: true },
    ],
  });
  const target = {
    targetId: 't1',
    name: 'vm-store',
    protocol: 'iscsi',
    wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store',
    enabled: true,
    luns: [{ index: 0, source: 'tank/vm-store', sizeBytes: 2199023255552, thin: true, groupId: 1, sourceKind: 'zvol' }],
    portals: [{ interface: 'storage0', address: '10.10.0.9', port: 3260, transport: 'tcp' }],
    auth: { method: 'chap', username: 'vmware01', secretSet: true },
    initiators: [],
    portGroups: [{ groupId: 1, state: 'optimized', preferred: false }],
  };
  const screen = fakeScreen({
    tentaNasTargetUpdateRequest: (payload) => { sent = payload; return { job: { jobId: 'j1', kind: 'target_update', subject: 'vm-store' } }; },
  });
  const win = openTargetWizard(screen, { target, capabilities: aliased });
  await flush();
  typeInto(win.querySelector('#nas-tw-secret'), 'nowy-sekret-1');
  await flush();
  click(nextButton(win));
  await flush();
  // No red warning: nothing is moving.
  assert.ok(!win.textContent.includes('stracą ścieżkę'), win.textContent);
  click(nextButton(win));
  await flush();
  await flush();
  assert.equal(sent.repickPortal, undefined, 'a secret edit is not a request to move the portal');
  assert.equal(sent.auth.secret, 'nowy-sekret-1');
  await settled();
  screen.dispose();
});

test('a portal whose address left its interface IS the re-pick the alert asked for', async () => {
  // The other side of the same coin, and the reason the intent cannot simply
  // be "did the admin touch the interface picker". When the address DRIFTS,
  // the alert tells the admin to re-pick the interface — and re-picking the
  // same interface has to mean something, or the one repair the app names is
  // a no-op. The rule that tells this from the alias case is the same one the
  // node's drift check uses: is the saved address still one this interface
  // holds?
  let sent = null;
  const moved = caps({
    interfaces: [
      { name: 'lan0', address: '192.168.1.5', rdma: false, shared: true, supported: true },
      { name: 'storage0', address: '10.10.0.9', rdma: true, shared: false, supported: true },
    ],
  });
  const target = {
    targetId: 't1',
    name: 'vm-store',
    protocol: 'iscsi',
    wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store',
    enabled: true,
    luns: [{ index: 0, source: 'tank/vm-store', sizeBytes: 2199023255552, thin: true, groupId: 1, sourceKind: 'zvol' }],
    portals: [{ interface: 'storage0', address: '10.10.0.5', port: 3260, transport: 'tcp' }],
    auth: { method: 'none' },
    initiators: [],
    portGroups: [{ groupId: 1, state: 'optimized', preferred: false }],
  };
  const screen = fakeScreen({
    tentaNasTargetUpdateRequest: (payload) => { sent = payload; return { job: { jobId: 'j1', kind: 'target_update', subject: 'vm-store' } }; },
  });
  const win = openTargetWizard(screen, { target, capabilities: moved });
  await flush();
  // Saving WITHOUT ticking the box leaves the portal exactly where it is —
  // which is what makes a frozen target editable at all. The node keeps the
  // row's own address when no intent is sent.
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();
  assert.equal(sent.repickPortal, undefined, 'no tick, no move');
  assert.deepEqual(sent.portals.map((p) => p.interface), ['storage0']);
  await settled();
  screen.dispose();

  // Ticking it IS the re-pick the alert asked for — and re-picking the SAME
  // interface has to mean something, or the one repair this app names would be
  // a no-op.
  const screen2 = fakeScreen({
    tentaNasTargetUpdateRequest: (payload) => { sent = payload; return { job: { jobId: 'j2', kind: 'target_update', subject: 'vm-store' } }; },
  });
  const again = openTargetWizard(screen2, { target, capabilities: moved });
  await flush();
  setChecked(again.querySelector('#nas-tw-move-portal'), true);
  await flush();
  click(nextButton(again));
  await flush();
  assert.ok(again.textContent.includes('10.10.0.5') && again.textContent.includes('10.10.0.9'), again.textContent);
  click(nextButton(again));
  await flush();
  await flush();
  assert.equal(sent.repickPortal, true, 'the drift repair carries the intent');
  await settled();
  screen2.dispose();
});

test('narrowing an every-interface target to one interface says what it costs', async () => {
  // 0.0.0.0 on both sides of the comparison. Narrowing `np/0.0.0.0:3260` down
  // to one interface drops the portal EVERY initiator is logged in on — the
  // loudest version of this warning, and the one it used to skip because one
  // side of the comparison was an empty string.
  const target = {
    targetId: 't1',
    name: 'vm-store',
    protocol: 'iscsi',
    wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store',
    enabled: true,
    luns: [{ index: 0, source: 'tank/vm-store', sizeBytes: 2199023255552, thin: true, groupId: 1, sourceKind: 'zvol' }],
    portals: [{ interface: '', address: '0.0.0.0', port: 3260, transport: 'tcp' }],
    auth: { method: 'none' },
    initiators: [],
    portGroups: [{ groupId: 1, state: 'optimized', preferred: false }],
  };
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, { target, capabilities: caps() });
  await flush();
  setValue(win.querySelector('#nas-tw-iface'), 'storage0');
  await flush();
  click(nextButton(win));
  await flush();
  assert.ok(win.textContent.includes('0.0.0.0') && win.textContent.includes('10.10.0.5'), win.textContent);
  assert.ok(win.textContent.includes('stracą ścieżkę'), 'the move is named, in both directions');
  screen.dispose();
});

test('an interface that left the node still lets the target be edited', async () => {
  // Two failures, one after the other. The summary used to print
  // `0.0.0.0:3260` for a target whose interface is gone — the one portal the
  // node is certain NOT to create. Blocking step 2 fixed that and created a
  // worse one: the target became uneditable ENTIRELY — not its secret, not its
  // transport, not its allowlist — because the only way out of the wizard was
  // a portal move to an address that does not exist.
  //
  // The node has always accepted a save that leaves the portal alone. So does
  // the wizard now, and it says why the move is not on offer.
  const target = {
    targetId: 't1',
    name: 'vm-store',
    protocol: 'iscsi',
    wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store',
    enabled: true,
    luns: [{ index: 0, source: 'tank/vm-store', sizeBytes: 2199023255552, thin: true, groupId: 1, sourceKind: 'zvol' }],
    portals: [{ interface: 'storage9', address: '10.10.9.5', port: 3260, transport: 'tcp' }],
    auth: { method: 'none' },
    initiators: [],
    portGroups: [{ groupId: 1, state: 'optimized', preferred: false }],
  };
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, { target, capabilities: caps() });
  await flush();
  assert.ok(!nextButton(win).hasAttribute('disabled'), 'the save is not blocked');
  assert.match(win.textContent, /storage9/);
  assert.match(win.textContent, /portal zostaje tam, gdzie był/);
  // No move on offer: there is no address to move to.
  assert.equal(win.querySelector('#nas-tw-move-portal'), null);
  click(nextButton(win));
  await flush();
  assert.ok(!win.textContent.includes('0.0.0.0:3260'), win.textContent);
  screen.dispose();
});

test('one definition of an interface address, and IPv6 is not one', () => {
  // The node has the same function (`targets::primary_address`). Two rules for
  // one phrase is what let an aliased interface pass the drift check on one
  // address and be rewritten onto the other by the next save.
  const aliased = {
    interfaces: [
      { name: 'storage0', address: '10.10.0.5', supported: true },
      { name: 'storage0', address: '10.10.0.9', supported: true },
      { name: 'lan0', address: 'fe80::1', supported: false },
    ],
  };
  assert.equal(primaryAddress(aliased, 'storage0'), '10.10.0.5');
  // An IPv6-only interface has no address a portal can bind, and an empty
  // string is not one either — it would be a portal on nothing.
  assert.equal(primaryAddress(aliased, 'lan0'), '');
  assert.equal(primaryAddress(aliased, 'nope'), '');
});

test('editing an NVMe-oF subsystem sends the host NQNs the admin just typed', async () => {
  // The wizard renders the "NQN hostów" field on an EDIT too and `canProceed`
  // is gated on it, so sending the STORED list back means: the admin edits the
  // allowlist, gets a green notification, and the node saves the old list.
  // Going from "Brak" to DH-HMAC-CHAP was worse — the wizard let the step
  // through and the node refused an authenticated subsystem with no host NQN,
  // so that path could not be walked at all.
  let sent = null;
  const target = {
    targetId: 't2',
    name: 'scratch',
    protocol: 'nvmet',
    wwn: 'nqn.2026-09.local.tentaflow:helios.scratch',
    enabled: true,
    luns: [{ index: 1, source: 'fast/scratch', sizeBytes: 1099511627776, thin: true, groupId: 1, sourceKind: 'zvol' }],
    portals: [{ interface: 'storage0', address: '10.10.0.5', port: 4420, transport: 'tcp' }],
    auth: { method: 'dhchap', secretSet: true },
    initiators: ['nqn.2014-08.org.nvmexpress:uuid:stary'],
    portGroups: [{ groupId: 1, state: 'optimized', preferred: false }],
  };
  const screen = fakeScreen({
    tentaNasTargetUpdateRequest: (payload) => { sent = payload; return { job: { jobId: 'j9', kind: 'target_update', subject: 'scratch' } }; },
  });
  const win = openTargetWizard(screen, { target, capabilities: caps() });
  await flush();
  // The field is pre-filled from the row, so an edit that does not touch it
  // still sends what is there.
  assert.equal(win.querySelector('#nas-tw-hosts').getAttribute('value'), 'nqn.2014-08.org.nvmexpress:uuid:stary');
  typeInto(win.querySelector('#nas-tw-hosts'), 'nqn.2014-08.org.nvmexpress:uuid:nowy');
  await flush();
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();
  assert.deepEqual(sent.initiators, ['nqn.2014-08.org.nvmexpress:uuid:nowy']);
  await settled();
  screen.dispose();
});

test('the step-3 IQN preview derives the same WWN the node would create', async () => {
  // The authority is a constant in tentanas/targets.rs and this file carries a
  // copy, because step 3 previews the WWN of a target that does not exist yet.
  // Pinning the CONSTANT is not enough: the preview also sanitises the node
  // name, and the two implementations do it differently (`to_ascii_lowercase`
  // + an ASCII filter over there, `.toLowerCase()` + a regex here). A node
  // called `Helios_02.lan` is exactly where those two can disagree.
  //
  // So this asserts the whole derived string against a literal, and
  // `the_wizard_previews_the_wwn_with_this_module_s_own_naming_authority` in
  // targets.rs asserts `wwn_for` against THE SAME literal. Either side
  // drifting fails one of them.
  assert.equal(WWN_AUTHORITY, '2026-09.local.tentaflow');
  const screen = fakeScreen({});
  screen.currentNode = () => ({ nodeId: 'n1', nodeName: 'Helios_02.lan', isLocal: true });
  const win = openTargetWizard(screen, { capabilities: caps() });
  await flush();
  typeInto(win.querySelector('#nas-tw-name'), 'vm-store');
  await flush();
  click(nextButton(win));
  await flush();
  typeInto(win.querySelector('#nas-tw-user'), 'vmware01');
  typeInto(win.querySelector('#nas-tw-secret'), 'sekret-inicjatora');
  typeInto(win.querySelector('#nas-tw-muser'), 'helios');
  typeInto(win.querySelector('#nas-tw-msecret'), 'sekret-targetu-1');
  await flush();
  click(nextButton(win));
  await flush();
  assert.equal(
    win.querySelector('#nas-tw-sum-wwn').textContent,
    'iqn.2026-09.local.tentaflow:helios02lan.vm-store'
  );
  screen.dispose();
});

test('§5.5a: RDMA is the starting transport when the probe found it on the chosen interface', () => {
  const c = caps();
  // storage0 has an RDMA device, lan0 does not — the default follows the
  // INTERFACE, because RDMA on a card that has none is a default that cannot
  // work.
  assert.equal(defaultTransport('iscsi', c, 'storage0'), 'iser');
  assert.equal(defaultTransport('nvmet', c, 'storage0'), 'tcp+rdma');
  assert.equal(defaultTransport('iscsi', c, 'lan0'), 'tcp');
  assert.equal(defaultTransport('nvmet', c, 'lan0'), 'tcp');
  assert.equal(defaultTransport('iscsi', c, ''), 'tcp', 'every interface at once has no one card');
  // The node's own probe still decides: no iSER module, no iSER default.
  assert.equal(defaultTransport('iscsi', caps({ iser: false }), 'storage0'), 'tcp');
  assert.equal(defaultTransport('nvmet', caps({ nvmeRdma: false }), 'storage0'), 'tcp');
  // The MUTUAL variant, which is what n14 shows as the active segment: one-way
  // CHAP proves the initiator to the target and nothing back, so an initiator
  // cannot tell this target from whatever else answered on that address.
  assert.equal(defaultMethod('iscsi'), 'mutual-chap');
  assert.equal(defaultMethod('nvmet'), 'dhchap-bidi');
});

test('a new iSCSI target on an RDMA interface starts on iSER, and switching interface follows', async () => {
  const screen = fakeScreen({});
  const win = await toStepTwo(screen);
  assert.equal(win.querySelector('#nas-tw-transport').getAttribute('value'), 'iser');
  // Moving the portal to the LAN card drops back to TCP rather than leaving a
  // transport the interface cannot serve selected.
  setValue(win.querySelector('#nas-tw-iface'), 'lan0');
  await flush();
  assert.equal(win.querySelector('#nas-tw-transport').getAttribute('value'), 'tcp');
  screen.dispose();
});

test('a node without iSCSI starts on NVMe-oF with an NVMe-oF method selected', async () => {
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, {
    capabilities: caps({ iscsi: false, iscsiDetail: 'no target_core_mod' }),
  });
  await flush();
  assert.equal(win.querySelector('#nas-tw-protocol').getAttribute('value'), 'nvmet');
  typeInto(win.querySelector('#nas-tw-name'), 'scratch');
  await flush();
  click(nextButton(win));
  await flush();
  // 'chap' does not exist for NVMe-oF: carrying it over would leave the
  // segmented control with nothing selected and the save refused by the node.
  const auth = win.querySelector('#nas-tw-auth');
  assert.equal(auth.getAttribute('value'), 'dhchap-bidi', 'the mutual variant, as n14 shows');
  assert.ok([...auth.querySelectorAll('option')].some((o) => o.value === 'dhchap'));
  screen.dispose();
});

test('an authenticated NVMe-oF subsystem cannot be created without a host NQN', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasTargetCreateRequest: (payload) => { sent = payload; return { job: { jobId: 'j1', kind: 'target_create', subject: 'scratch' } }; },
  });
  const win = openTargetWizard(screen, { capabilities: caps() });
  await flush();
  setValue(win.querySelector('#nas-tw-protocol'), 'nvmet');
  typeInto(win.querySelector('#nas-tw-name'), 'scratch');
  await flush();
  click(nextButton(win));
  await flush();

  // The wizard starts on the BIDIRECTIONAL variant (n14's active segment), so
  // both keys are asked for.
  typeInto(win.querySelector('#nas-tw-secret'), 'DHHC-1:00:cGxhY2Vob2xkZXI=:');
  typeInto(win.querySelector('#nas-tw-msecret'), 'DHHC-1:00:Y3RybC1rZXk=:');
  await flush();
  // nvmet keeps the DH-HMAC-CHAP key on the HOST object of the allowlist, so a
  // subsystem with a key and no host is refused by the node. The wizard has to
  // stop here — before the zvol is created, which is what made the failure
  // permanent for a given name.
  assert.ok(nextButton(win).hasAttribute('disabled'), 'no host NQN, no next step');
  assert.match(win.textContent, /wymaga co najmniej jednego NQN hosta/);

  typeInto(win.querySelector('#nas-tw-hosts'), 'nqn.2014-08.org.nvmexpress:uuid:1b4e28ba\n\nnqn.2014-08.org.nvmexpress:uuid:1b4e28ba');
  await flush();
  assert.ok(!nextButton(win).hasAttribute('disabled'));
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();

  // The allowlist rides along with the create, deduplicated.
  assert.deepEqual(sent.initiators, ['nqn.2014-08.org.nvmexpress:uuid:1b4e28ba']);
  assert.equal(sent.auth.method, 'dhchap-bidi');
  assert.equal(sent.protocol, 'nvmet');
  await settled();
  screen.dispose();
});

test('an unauthenticated NVMe-oF subsystem needs no host NQN and sends none', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasTargetCreateRequest: (payload) => { sent = payload; return { job: { jobId: 'j1', kind: 'target_create', subject: 'scratch' } }; },
  });
  const win = openTargetWizard(screen, { capabilities: caps() });
  await flush();
  setValue(win.querySelector('#nas-tw-protocol'), 'nvmet');
  typeInto(win.querySelector('#nas-tw-name'), 'scratch');
  await flush();
  click(nextButton(win));
  await flush();
  setValue(win.querySelector('#nas-tw-auth'), 'none');
  await flush();
  // The field STAYS. It used to disappear here — while the save still sent
  // whatever was in it — so "no authentication" could mean an allowlist the
  // admin could not see. It is a filter, not a login (§5.5): legal without a
  // key, and empty means "every initiator".
  assert.ok(win.querySelector('#nas-tw-hosts'), 'the allowlist is not an auth field');
  assert.ok(!nextButton(win).hasAttribute('disabled'), 'and it is not required without a key');
  click(nextButton(win));
  await flush();
  click(nextButton(win));
  await flush();
  await flush();
  assert.deepEqual(sent.initiators, [], 'an untouched allowlist sends nothing');
  await settled();
  screen.dispose();
});

test('an IPv6 interface is offered and disabled with its reason, not dropped', async () => {
  const screen = fakeScreen({});
  const c = caps({
    interfaces: [
      { name: 'storage0', address: 'fd00::5', rdma: false, shared: false, supported: false },
      { name: 'lan0', address: '192.168.1.5', rdma: false, shared: true, supported: true },
    ],
  });
  const win = await toStepTwo(screen, { capabilities: c });
  const iface = win.querySelector('#nas-tw-iface');
  // Listed, so an IPv6-only node can see WHY it has nothing to pick.
  assert.ok(selectLabels(iface).some((l) => l.includes('fd00::5') && l.includes('IPv6')), JSON.stringify(selectLabels(iface)));
  assert.deepEqual(disabledValues(iface), ['storage0']);
  // …and the picker starts on the one a portal can actually bind.
  assert.equal(iface.getAttribute('value'), 'lan0');
  screen.dispose();
});

test('the host NQN parser drops blanks and duplicates', () => {
  assert.deepEqual(parseHostNqns('a\n b ;a,\n\nc'), ['a', 'b', 'c']);
  assert.deepEqual(parseHostNqns(''), []);
});


test('a host NQN another NVMe-oF target already allows is named, because the key is shared', async () => {
  // nvmet keeps the DH-HMAC-CHAP key on the HOST object, which is node-wide —
  // so two targets naming the same host share one key whether anybody meant
  // them to or not. The node refuses to guess (it will not overwrite a key
  // another subsystem uses, and it will not wipe one on unlink); what it
  // cannot do is tell the admin that the field they are typing into is shared.
  //
  // Ordinary, not exotic: one LUN per target (§6.1) means two zvols for one
  // VMware host are two targets carrying the same NQN.
  const esx = 'nqn.2014-08.org.nvmexpress:uuid:1b4e28ba';
  const others = [
    { targetId: 't9', name: 'vm-a', protocol: 'nvmet', initiators: [esx] },
    { targetId: 't8', name: 'vm-iscsi', protocol: 'iscsi', initiators: [esx] },
  ];
  // The pure rule first: the iSCSI row is not a collision (its allowlist is
  // IQNs on a TPG, not a shared host object), and a target never collides with
  // itself on an edit.
  assert.deepEqual(sharedHostTargets(others, 'nvmet', [esx], null), ['vm-a']);
  assert.deepEqual(sharedHostTargets(others, 'nvmet', ['nqn.2014-08.org.nvmexpress:uuid:other'], null), []);
  assert.deepEqual(sharedHostTargets(others, 'iscsi', [esx], null), []);
  assert.deepEqual(sharedHostTargets([{ targetId: 't9', name: 'vm-a', protocol: 'nvmet', initiators: [esx] }], 'nvmet', [esx], 't9'), []);
  // The self-exclusion over a ONE-element list is satisfied by `return []`.
  // The list that matters is the one the edit path actually passes: the whole
  // node, the edited row INCLUDED — the row itself is dropped and the real
  // collision still reported.
  const withSelf = [{ targetId: 't7', name: 'vm-b', protocol: 'nvmet', initiators: [esx] }, ...others];
  assert.deepEqual(sharedHostTargets(withSelf, 'nvmet', [esx], 't7'), ['vm-a']);
  // Case-insensitive on BOTH sides. The node stores NQNs lower-case, so a
  // capital pasted into the field used to make a real collision invisible and
  // silently turn this warning off.
  assert.deepEqual(sharedHostTargets(others, 'nvmet', [esx.toUpperCase()], null), ['vm-a']);
  assert.deepEqual(
    sharedHostTargets([{ targetId: 't9', name: 'vm-a', protocol: 'nvmet', initiators: [esx.toUpperCase()] }], 'nvmet', [esx], null),
    ['vm-a'],
  );
  assert.deepEqual(parseHostNqns('NQN.Example:X'), ['nqn.example:x'], 'and the parser lower-cases');
  // WHICH NQN, not only which targets: on an allowlist of four the name of
  // the other target does not tell the admin which line to change.
  const two = [esx, 'nqn.2014-08.org.nvmexpress:uuid:esx02'];
  assert.deepEqual(sharedHostNqns(others, 'nvmet', two, null), [esx]);
  assert.deepEqual(sharedHostNqns(others, 'nvmet', [two[1]], null), []);
  assert.deepEqual(sharedHostNqns(others, 'iscsi', two, null), []);

  // …and the wizard says it where the admin types the NQN.
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, { capabilities: caps(), targets: others });
  await flush();
  setValue(win.querySelector('#nas-tw-protocol'), 'nvmet');
  typeInto(win.querySelector('#nas-tw-name'), 'vm-b');
  await flush();
  click(nextButton(win));
  await flush();
  assert.ok(!win.textContent.includes('vm-a'), 'nothing to say until an NQN is typed');
  typeInto(win.querySelector('#nas-tw-hosts'), esx);
  await flush();
  assert.match(win.textContent, /vm-a/);
  assert.ok(win.textContent.includes(esx), 'and the NQN the collision is about');
  assert.match(win.textContent, /wspólnym dla całego węzła/);
  // The sentence has to be TRUE. The node refuses a conflicting apply; it does
  // not "decline to overwrite" and carry on, which is what five locales used
  // to say and what would have told the admin the opposite of what happens.
  assert.match(win.textContent, /odmówi zastosowania/);
  assert.ok(!win.textContent.includes('nie nadpisze'), 'the old, false promise is gone');

  // Typing does not take the caret out of the field: the warning is repainted,
  // the step is not. This is an 80-character NQN — a wizard that dropped focus
  // on every keystroke made the field unusable by hand.
  const field = win.querySelector('#nas-tw-hosts');
  typeInto(field, `${esx}x`);
  await flush();
  assert.equal(win.querySelector('#nas-tw-hosts'), field, 'the NQN field survives a keystroke');

  // And the shape check, which did not exist: `esx01` used to pass all three
  // steps and come back from the node as a raw catalog error after the sudo
  // prompt.
  typeInto(field, 'esx01');
  await flush();
  assert.match(win.textContent, /nqn\./, 'a malformed NQN is named before the save');
  assert.ok(nextButton(win).hasAttribute('disabled'), 'and it gates the step');

  // …and the warning survives to step 3, like every other warning about a
  // CONSEQUENCE of the save. This consequence lands on a target the admin is
  // not editing and cannot see from the summary.
  typeInto(field, esx);
  typeInto(win.querySelector('#nas-tw-secret'), 'DHHC-1:00:cGxhY2Vob2xkZXItaG9zdA==:');
  await flush();
  click(nextButton(win));
  await flush();
  assert.match(win.textContent, /Podsumowanie|podsumowan/i);
  assert.match(win.textContent, /vm-a/, 'the shared-host warning is repeated on the summary');
  screen.dispose();
});

test('the allowlist is offered with authentication off, and says the shared key stays', async () => {
  // Turning authentication off used to HIDE the NQN field — while the save
  // still sent what was in it. So the node kept an allowlist the admin could
  // no longer see, and, on a host shared with an authenticated target, kept
  // demanding a key the UI said was gone. The allowlist is a filter, not a
  // login (§5.5): it is legal without a key, so it is shown without one.
  const esx = 'nqn.2014-08.org.nvmexpress:uuid:1b4e28ba';
  const others = [{ targetId: 't9', name: 'vm-a', protocol: 'nvmet', initiators: [esx] }];
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, { capabilities: caps(), targets: others });
  await flush();
  setValue(win.querySelector('#nas-tw-protocol'), 'nvmet');
  typeInto(win.querySelector('#nas-tw-name'), 'vm-b');
  await flush();
  click(nextButton(win));
  await flush();
  setValue(win.querySelector('#nas-tw-auth'), 'none');
  await flush();
  const field = win.querySelector('#nas-tw-hosts');
  assert.ok(field, 'the allowlist is still there with no authentication');
  assert.ok(!win.querySelector('#nas-tw-secret'), 'and the key field is not');
  typeInto(field, esx);
  await flush();
  // The OTHER sentence: with no key of our own we cannot take theirs off the
  // shared object, so the kernel keeps demanding it — on this target too.
  assert.match(win.textContent, /vm-a/);
  assert.match(win.textContent, /nadal będzie go żądać/);
  // An empty allowlist is fine here — it is what "admit every initiator"
  // means — so the step is not gated on it the way the authenticated one is.
  typeInto(field, '');
  await flush();
  assert.ok(!nextButton(win).hasAttribute('disabled'), 'no key, no allowlist, still valid');
  screen.dispose();
});

test('a protocol switch does not carry the typed secret to the other protocol', async () => {
  // A 12-character CHAP password is not a `DHHC-1:` key. Carrying it across
  // the switch offered it to a field whose catalog rule refuses its shape —
  // and the refusal arrives after the sudo prompt.
  const screen = fakeScreen({});
  const win = openTargetWizard(screen, { capabilities: caps() });
  await flush();
  setValue(win.querySelector('#nas-tw-protocol'), 'iscsi');
  typeInto(win.querySelector('#nas-tw-name'), 'vm-b');
  await flush();
  click(nextButton(win));
  await flush();
  setValue(win.querySelector('#nas-tw-auth'), 'chap');
  await flush();
  typeInto(win.querySelector('#nas-tw-user'), 'esx01');
  typeInto(win.querySelector('#nas-tw-secret'), 'sekret-12-znakow');
  await flush();
  assert.equal(win.querySelector('#nas-tw-secret').value, 'sekret-12-znakow');
  // Back to step one and over to the other protocol.
  click(win.querySelector('[data-wizard-back]'));
  await flush();
  setValue(win.querySelector('#nas-tw-protocol'), 'nvmet');
  await flush();
  click(nextButton(win));
  await flush();
  setValue(win.querySelector('#nas-tw-auth'), 'dhchap');
  await flush();
  assert.equal(win.querySelector('#nas-tw-secret').value, '', 'the CHAP secret did not follow');
  screen.dispose();
});

test('the malformed-NQN check is the node\'s own rule, not a looser one', () => {
  // Mirrors `block::validate_nqn` / `validate_target_name`. If the two ever
  // disagree the node is the authority and the save still fails — but after
  // the sudo prompt, which is the whole reason this exists.
  assert.deepEqual(invalidHostNqns('nqn.2014-08.org.nvmexpress:uuid:1b4e28ba'), []);
  assert.deepEqual(invalidHostNqns('NQN.2014-08.ORG:X'), [], 'a pasted capital is lower-cased, not refused');
  assert.deepEqual(invalidHostNqns('esx01'), ['esx01'], "an NQN starts with 'nqn.'");
  assert.deepEqual(invalidHostNqns('nqn.a_b'), ['nqn.a_b'], "'_' is not in the node's alphabet");
  assert.deepEqual(invalidHostNqns('nqn.a..b'), ['nqn.a..b'], "'..' is a path escape");
  assert.deepEqual(invalidHostNqns(`nqn.${'a'.repeat(300)}`).length, 1, '223 characters is the limit');
  assert.deepEqual(invalidHostNqns('nqn.ok\nesx01'), ['esx01'], 'only the bad one is named');
});

test('an interface with an alias is ONE option, not two with the same value', async () => {
  // `caps.interfaces` legitimately carries the same name twice — a storage
  // VLAN on a secondary address is exactly that — and the option's value IS
  // the name. Two options with one value means picking the second silently
  // picks the first, and there is no way to say "the .9 alias" at all.
  const screen = fakeScreen({});
  const c = caps({
    interfaces: [
      { name: 'storage0', address: '10.10.0.5', rdma: false, shared: false, supported: true },
      { name: 'storage0', address: '10.10.0.9', rdma: false, shared: false, supported: true },
      { name: 'lan0', address: '192.168.1.5', rdma: false, shared: true, supported: true },
    ],
  });
  const win = await toStepTwo(screen, { capabilities: c });
  const iface = win.querySelector('#nas-tw-iface');
  const values = selectValues(iface);
  assert.equal(values.filter((v) => v === 'storage0').length, 1, JSON.stringify(values));
  // Both addresses are still visible — they are what the portal may bind, so
  // they belong in the label rather than in a second, unselectable row.
  const label = selectLabels(iface).find((l) => l.includes('storage0'));
  assert.ok(label.includes('10.10.0.5') && label.includes('10.10.0.9'), label);
  screen.dispose();
});

test('the addresses of an interface are a list, and the primary is the first of it', () => {
  // `bindableAddresses` is what tells an ALIAS apart from a DRIFT — the whole
  // reason the re-pick intent exists — and it had no test of its own, only
  // indirect coverage through the drift warning.
  const withAlias = caps({
    interfaces: [
      { name: 'storage0', address: '10.10.0.5', supported: true },
      { name: 'storage0', address: '10.10.0.9', supported: true },
      { name: 'eth0', address: '192.168.1.2', supported: true },
    ],
  });
  assert.deepEqual(bindableAddresses(withAlias, 'storage0'), ['10.10.0.5', '10.10.0.9']);
  assert.equal(primaryAddress(withAlias, 'storage0'), '10.10.0.5');
  // A portal on the alias is bindable, so it is NOT drift — that is the whole
  // distinction, and it is why this is a list and not one address.
  assert.ok(bindableAddresses(withAlias, 'storage0').includes('10.10.0.9'));
  assert.deepEqual(bindableAddresses(withAlias, 'nie-ma-takiego0'), []);
  assert.equal(primaryAddress(withAlias, 'nie-ma-takiego0'), '');
  // "Every interface" is not an interface, and it is spelled once.
  assert.equal(ALL_INTERFACES_ADDRESS, '0.0.0.0');
  assert.deepEqual(bindableAddresses(withAlias, ''), []);
});
