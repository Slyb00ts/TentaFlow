// =============================================================================
// File: modules/tentanas/targets.test.js
// Description: The block-target half of the Sharing tab (n12) against a fake
// screen: the table's five columns, the authentication chip that never reads
// like a lock when there is none, the portal cell naming the interface AND the
// address, the detail window's initiator allowlist with its "filter, not a
// login" warning and the redacted configfs preview, pause/resume and the
// retype-gated delete. Runs under happy-dom.
// =============================================================================

import { fakeScreen, flush, click, window } from './_test-setup.js';
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

const {
  mountTargetsSection, openTargetDetail, openTargetDeleteDialog, setTargetEnabled,
  authChipHtml, authLabel, portalCellHtml, sourceCellHtml, protocolLabel, parseInitiators,
  groupStateLabel, targetRow, sessionsCountLabel, sessionsEmptyText,
  sessionLine, protocolChipHtml, transportLabel,
} = await import('./targets.js');

const iscsiTarget = (over = {}) => ({
  targetId: 't1',
  name: 'vm-store',
  protocol: 'iscsi',
  wwn: 'iqn.2026-09.local.tentaflow:helios.vm-store',
  enabled: true,
  luns: [{ index: 0, source: 'tank/vm-store', devicePath: '/dev/zvol/tank/vm-store', sizeBytes: 2199023255552, thin: true, uuid: 'u1', groupId: 1, sourceKind: 'zvol' }],
  portals: [{ interface: 'storage0', address: '10.10.0.5', port: 3260, transport: 'tcp' }],
  auth: { method: 'mutual-chap', username: 'vmware01', mutualUsername: 'helios', secret: null, mutualSecret: null, secretSet: true, mutualSecretSet: true, dhchapHash: '', dhchapDhgroup: '' },
  initiators: ['iqn.1998-01.com.vmware:esx01'],
  portGroups: [{ groupId: 1, state: 'optimized', preferred: false }],
  sessions: 2,
  sessionsKnown: true,
  state: 'active',
  stateDetail: '',
  createdAt: '2026-09-03T12:00:00Z',
  updatedAt: '2026-09-03T12:00:00Z',
  ...over,
});

const nvmetTarget = (over = {}) => iscsiTarget({
  targetId: 't2',
  name: 'scratch',
  protocol: 'nvmet',
  wwn: 'nqn.2026-09.local.tentaflow:helios.scratch',
  luns: [{ index: 1, source: 'fast/scratch', devicePath: '/dev/zvol/fast/scratch', sizeBytes: 536870912000, thin: true, uuid: 'u2', groupId: 1, sourceKind: 'zvol' }],
  portals: [
    { interface: 'storage0', address: '10.10.0.5', port: 4420, transport: 'tcp' },
    { interface: 'storage0', address: '10.10.0.5', port: 4420, transport: 'rdma' },
  ],
  auth: { method: 'dhchap', username: '', mutualUsername: '', secret: null, mutualSecret: null, secretSet: true, mutualSecretSet: false, dhchapHash: 'hmac(sha256)', dhchapDhgroup: 'ffdhe2048' },
  sessions: 0,
  // The node could not read debugfs, so it did not measure anything. The
  // fixture defaults to the honest half of the pair; the tests that want a
  // measured NVMe-oF node pass `sessionsKnown: true` themselves.
  sessionsKnown: false,
  ...over,
});

const capabilities = {
  iscsi: true, nvmet: true, iser: true, nvmeRdma: true, dhchap: true,
  interfaces: [
    { name: 'storage0', address: '10.10.0.5', rdma: true, shared: false, supported: true },
    { name: 'lan0', address: '192.168.1.5', rdma: false, shared: true, supported: true },
  ],
  volumes: [],
};

const listAnswer = (targets) => ({ targets, services: [], capabilities });
const settled = () => new Promise((r) => setTimeout(r, 260));

test('the authentication chip never reads like a lock when there is none', () => {
  assert.match(authChipHtml({ method: 'mutual-chap' }), /status="ok"/);
  assert.match(authChipHtml({ method: 'mutual-chap' }), /icon="lock"/);
  assert.match(authChipHtml({ method: 'dhchap-bidi' }), /status="ok"/);
  // No authentication is a WARNING, not a neutral label: the allowlist that
  // may sit next to it is a filter and not a login (§5.5).
  assert.match(authChipHtml({ method: 'none' }), /status="warn"/);
  assert.match(authChipHtml(null), /status="warn"/);
  assert.equal(authLabel('mutual-chap'), 'CHAP mutual');
  assert.equal(authLabel('dhchap'), 'DH-HMAC-CHAP');
  assert.equal(protocolLabel('nvmet'), 'NVMe-oF');
  assert.equal(protocolLabel('iscsi'), 'iSCSI');
});

test('the portal cell names the address the kernel binds and the interface it came from', () => {
  const bound = portalCellHtml(iscsiTarget());
  assert.match(bound, /10\.10\.0\.5:3260/);
  assert.match(bound, /iface storage0/);
  // Two portals of one NVMe-oF target are one address and two transports.
  const both = portalCellHtml(nvmetTarget());
  assert.match(both, /10\.10\.0\.5:4420/);
  assert.match(both, /TCP \+ RDMA/);
  // No interface is not blank: it is the fact that the target listens on all
  // of them, which is a decision the wizard made you confirm.
  const every = portalCellHtml(iscsiTarget({ portals: [{ interface: '', address: '0.0.0.0', port: 3260, transport: 'tcp' }] }));
  assert.match(every, /0\.0\.0\.0:3260/);
  assert.match(every, /0\.0\.0\.0\)/);
  assert.equal(portalCellHtml(iscsiTarget({ portals: [] })), '—');
});

test('the source cell names the zvol, its size and whether it is thin', () => {
  assert.match(sourceCellHtml(iscsiTarget()), /zvol tank\/vm-store/);
  assert.match(sourceCellHtml(iscsiTarget()), /thin/);
  assert.equal(sourceCellHtml(iscsiTarget({ luns: [] })), '—');
});

test('an active target with no authentication carries the reason as a warning chip', () => {
  const clean = targetRow(iscsiTarget());
  assert.ok(!clean.name.includes('status="warn"'), clean.name);
  const open = targetRow(iscsiTarget({
    auth: { method: 'none' },
    stateDetail: 'no authentication — the IQN/NQN allowlist is a filter, not a login',
  }));
  assert.match(open.name, /status="warn"/);
  assert.match(open.name, /filter, not a login/);
  assert.match(open.auth, /status="warn"/);
  // A stopped target is neutral, an errored one is red.
  assert.match(targetRow(iscsiTarget({ enabled: false, state: 'disabled' })).name, /status="neutral"/);
  assert.match(targetRow(iscsiTarget({ state: 'error', stateDetail: 'nvmet missing' })).name, /status="err"/);
});

test('the section lists the targets and offers the n12 row actions to an admin', async () => {
  const screen = fakeScreen({});
  const host = document.createElement('div');
  document.body.appendChild(host);
  const section = mountTargetsSection(screen, host, {});
  section.set(listAnswer([iscsiTarget(), nvmetTarget()]));
  await flush();

  assert.equal(host.querySelector('#nas-tg-count').getAttribute('label'), '2');
  const table = host.querySelector('#nas-tg-table');
  assert.equal(table.rows.length, 2);
  assert.deepEqual(
    [...table.querySelectorAll('tf-column')].map((c) => c.getAttribute('key')),
    ['name', 'protocol', 'source', 'auth', 'portal'],
  );
  const actions = table.rowActions(table.rows[0]);
  assert.deepEqual(
    [...actions.querySelectorAll('tf-button')].map((b) => b.dataset.act),
    ['edit', 'pause', 'delete'],
  );

  // The protocol filter of n12 narrows the same list.
  section.filter('nvmet');
  assert.equal(host.querySelector('#nas-tg-table').rows.length, 1);
  section.filter('all');
  section.search('scratch');
  assert.equal(host.querySelector('#nas-tg-table').rows.length, 1);
  section.search('');
  assert.equal(host.querySelector('#nas-tg-table').rows.length, 2);
  screen.dispose();
});

test('a reader sees the details action and no mutation, and an empty node offers the wizard', async () => {
  const reader = fakeScreen({}, { admin: false });
  const host = document.createElement('div');
  document.body.appendChild(host);
  const section = mountTargetsSection(reader, host, {});
  section.set(listAnswer([iscsiTarget()]));
  await flush();
  const table = host.querySelector('#nas-tg-table');
  assert.deepEqual([...table.rowActions(table.rows[0]).querySelectorAll('tf-button')].map((b) => b.dataset.act), ['details']);
  // A reader is not offered the create button on the empty state either.
  section.set(listAnswer([]));
  assert.ok(!host.querySelector('[data-act="create-empty"]'));
  reader.dispose();

  const admin = fakeScreen({});
  const host2 = document.createElement('div');
  document.body.appendChild(host2);
  mountTargetsSection(admin, host2, {}).set(listAnswer([]));
  assert.ok(host2.querySelector('tf-empty-state'));
  assert.ok(host2.querySelector('[data-act="create-empty"]'));
  admin.dispose();
});

test('a failing target list shows its error instead of an empty table', async () => {
  const screen = fakeScreen({});
  const host = document.createElement('div');
  document.body.appendChild(host);
  const section = mountTargetsSection(screen, host, {});
  section.fail('privilege channel not available');
  await flush();
  assert.match(host.querySelector('#nas-tg-list').textContent, /privilege channel not available/);
  screen.dispose();
});

test('the detail window shows the allowlist, both warnings and the redacted configfs', async () => {
  const preview = [
    'mkdir /sys/kernel/config/target/iscsi/iqn.2026-09.local.tentaflow:helios.vm-store/tpgt_1',
    'write /sys/kernel/config/target/iscsi/iqn.2026-09.local.tentaflow:helios.vm-store/tpgt_1/auth/password = ***',
  ].join('\n');
  const screen = fakeScreen({
    tentaNasTargetGetRequest: { target: iscsiTarget(), sessions: [{ client: 'iqn.1998-01.com.vmware:esx01', user: '', connectedAt: null }], configPreview: preview },
  });
  const win = openTargetDetail(screen, 't1', { capabilities });
  await flush();
  await flush();

  const text = win.textContent;
  assert.match(text, /iqn\.2026-09\.local\.tentaflow:helios\.vm-store/);
  assert.match(text, /tank\/vm-store/);
  // §5.5: both sentences the plan asks for are on the screen, always.
  assert.match(text, /filtr, nie login/);
  assert.match(text, /surowy dysk/);
  // The ALUA/ANA group state is visible from the first version (R8).
  assert.match(win.querySelector('#nas-td-groups').textContent, /Active\/Optimized/);
  // The preview is what the node writes — the REDACTION happens in Rust
  // (`block::render`, which has no mode that prints a secret), so what this
  // asserts is that the window shows the node's string unaltered and adds
  // nothing of its own. There is no redaction path in JS to exercise.
  const shown = win.querySelector('#nas-td-preview').textContent;
  assert.match(shown, /auth\/password = \*\*\*/);
  assert.ok(!/password = \w/.test(shown.replace('***', '')), shown);
  // The allowlist is editable text, one initiator per line.
  assert.equal(win.querySelector('#nas-td-initiators').getAttribute('value'), 'iqn.1998-01.com.vmware:esx01');
  screen.dispose();
});

test('an nvmet node that cannot read debugfs says so instead of reporting zero as a fact', async () => {
  const screen = fakeScreen({
    tentaNasTargetGetRequest: { target: nvmetTarget(), sessions: [], configPreview: '' },
  });
  const win = openTargetDetail(screen, 't2', { capabilities });
  await flush();
  await flush();
  assert.match(win.textContent, /nie odczyta kontrolerów NVMe-oF/);
  // The count is a dash, never a 0 the node did not measure.
  assert.equal(sessionsCountLabel(nvmetTarget()), '—');
  screen.dispose();
});

test('an nvmet node that DID read debugfs lists the host NQNs and counts them', async () => {
  // OWNER DECISION (2026-09-04): where the kernel publishes its controllers,
  // the app reads them. `sessionsKnown` is what separates this from the test
  // above — the same empty list means two different things.
  const target = nvmetTarget({ sessions: 1, sessionsKnown: true });
  const screen = fakeScreen({
    tentaNasTargetGetRequest: {
      target,
      // Measured on a node: nvmet publishes `host_traddr` next to `hostnqn`,
      // so the session carries an address AND an identity — the detail shows
      // both, because only the first one is not client-declared.
      sessions: [{ client: '192.168.10.24', user: 'nqn.2014-08.org.nvmexpress:uuid:esx01', connectedAt: null }],
      configPreview: '',
    },
  });
  const win = openTargetDetail(screen, 't2', { capabilities });
  await flush();
  await flush();
  assert.match(win.querySelector('#nas-td-sessions').textContent, /192\.168\.10\.24 · nqn\.2014-08\.org\.nvmexpress:uuid:esx01/);
  assert.equal(sessionsCountLabel(target), '1');
  // A MEASURED zero is a zero, and says so with the ordinary sentence.
  assert.match(sessionsEmptyText(nvmetTarget({ sessionsKnown: true })), /Brak zalogowanych/);
  screen.dispose();
});

test('the detail allowlist warns about a host another NVMe-oF target already allows', async () => {
  // The SECOND surface an allowlist is edited from, and it had no check at
  // all. nvmet keeps the DH-HMAC-CHAP key on the host object, which is
  // node-wide: adding an NQN here aims the next apply at an object another
  // target authenticates with, and the node then refuses that apply with an
  // error the admin had no way to see coming.
  const esx = 'nqn.2014-08.org.nvmexpress:uuid:1b4e28ba';
  const other = nvmetTarget({ targetId: 't3', name: 'vm-a', initiators: [esx] });
  const screen = fakeScreen({
    tentaNasTargetGetRequest: { target: nvmetTarget({ initiators: [] }), sessions: [], configPreview: '' },
  });
  const win = openTargetDetail(screen, 't2', { capabilities, siblings: [nvmetTarget({ initiators: [] }), other] });
  await flush();
  await flush();
  assert.ok(!win.textContent.includes('vm-a'), 'nothing to say for an empty allowlist');

  const box = win.querySelector('#nas-td-initiators');
  box.value = esx;
  box.dispatchEvent(new window.CustomEvent('input', { bubbles: true }));
  await flush();
  assert.match(win.querySelector('#nas-td-shared').textContent, /vm-a/);
  // Repainted in place, not by redrawing the window: the field the admin is
  // typing an 80-character NQN into keeps the caret.
  assert.equal(win.querySelector('#nas-td-initiators'), box, 'the field survives the update');
  screen.dispose();
});

test('editing from the detail window hands the wizard the whole node, not just this target', async () => {
  // `sharedHostTargets` excludes the target being edited, so the one-element
  // list this path used to pass always filtered to empty — the shared-host
  // warning never rendered on the ordinary way to edit an existing target,
  // which is the only way to ADD an NQN to one.
  const esx = 'nqn.2014-08.org.nvmexpress:uuid:1b4e28ba';
  const mine = nvmetTarget({ initiators: [] });
  const other = nvmetTarget({ targetId: 't3', name: 'vm-a', initiators: [esx] });
  const screen = fakeScreen({
    tentaNasTargetGetRequest: { target: mine, sessions: [], configPreview: '' },
    tentaNasCapabilitiesRequest: { capabilities },
  });
  const win = openTargetDetail(screen, 't2', { capabilities, siblings: [mine, other] });
  await flush();
  await flush();
  click(win.querySelector('[data-act="edit"]'));
  await flush();
  const wizard = screen.openWindow;
  assert.ok(wizard, 'the wizard opened');
  const hosts = wizard.querySelector('#nas-tw-hosts');
  assert.ok(hosts, 'and it is on the step that holds the allowlist');
  hosts.value = esx;
  hosts.dispatchEvent(new window.CustomEvent('input', { bubbles: true }));
  await flush();
  assert.match(wizard.textContent, /vm-a/, 'the collision with the other target is named');
  screen.dispose();
});

test('the session line, the protocol chip and the transport label say what they are', () => {
  // Three exports with no test call site at all. Two of them share a NAME with
  // a DIFFERENT function in another module of this same directory
  // (`shares.js` has its own `protocolChipHtml`, `format.js` its own
  // `transportLabel` taking a boolean) — so an import fixed by autocomplete
  // would compile, render, and be wrong.
  assert.match(sessionLine({ client: '192.168.10.24', user: 'nqn.x' }), /192\.168\.10\.24 · nqn\.x/);
  // One identity, printed once — not "x · x".
  assert.equal(sessionLine({ client: 'iqn.a', user: 'iqn.a' }), 'iqn.a');
  assert.equal(sessionLine({}), '—', 'a session the node could not name is a dash, not empty');

  assert.match(protocolChipHtml('iscsi'), /iSCSI/);
  assert.match(protocolChipHtml('nvmet'), /NVMe-oF/);
  assert.match(protocolChipHtml('iscsi'), /<tf-chip/);

  // This one takes the TRANSPORT STRING; `format.js` exports a different
  // function of the same name that takes a boolean.
  assert.equal(transportLabel('tcp'), 'TCP');
  assert.match(transportLabel('iser'), /iSER/);
  assert.match(transportLabel('rdma'), /RDMA/);
  assert.equal(transportLabel('nonsense'), transportLabel('tcp'), 'an unknown transport falls back to TCP');
});

test('deleting a target whose sessions are unknown says the blast radius is unknown', async () => {
  // The red path may not understate itself: with no measurement there is no
  // "(n) initiators lose the disk" line, so the dialog says that outright
  // instead of showing nothing at all.
  const unknown = openTargetDeleteDialog(fakeScreen({}), nvmetTarget(), () => {});
  assert.match(unknown.textContent, /Liczba podłączonych hostów NVMe-oF jest nieznana/);
  assert.ok(!/tracą dysk natychmiast/.test(unknown.textContent), unknown.textContent);
  // A measured node states the number and drops the unknown line.
  const measured = openTargetDeleteDialog(fakeScreen({}), nvmetTarget({ sessions: 2, sessionsKnown: true }), () => {});
  assert.match(measured.textContent, /tracą dysk natychmiast/);
  assert.ok(!/jest nieznana/.test(measured.textContent), measured.textContent);
});

test('saving the allowlist sends every initiator line and keeps the rest of the target', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasTargetGetRequest: { target: iscsiTarget(), sessions: [], configPreview: '' },
    tentaNasTargetUpdateRequest: (payload) => { sent = payload; return { job: { jobId: 'j1', kind: 'target_update', subject: 'vm-store' } }; },
  });
  const win = openTargetDetail(screen, 't1', { capabilities });
  await flush();
  await flush();
  const box = win.querySelector('#nas-td-initiators');
  box.value = 'iqn.1998-01.com.vmware:esx01\niqn.1998-01.com.vmware:esx02\n\niqn.1998-01.com.vmware:esx01';
  box.dispatchEvent(new window.CustomEvent('input', { bubbles: true }));
  click(win.querySelector('[data-act="save"]'));
  await flush();
  await flush();

  assert.deepEqual(sent.initiators, ['iqn.1998-01.com.vmware:esx01', 'iqn.1998-01.com.vmware:esx02']);
  assert.equal(sent.targetId, 't1');
  assert.equal(sent.enabled, true);
  // The rest of the target rides along unchanged: an allowlist edit is not a
  // portal or an authentication edit.
  assert.equal(sent.auth.method, 'mutual-chap');
  // …and "not a portal edit" is now literal: NO portals are sent and the flag
  // that would move one is absent, so the node keeps the address the admin
  // picked. Sending them used to re-derive the address from the interface on
  // the node, which quietly healed a drift alert nobody answered and, on an
  // aliased interface, removed a LIVE portal (owner decision 2026-09-04).
  assert.deepEqual(sent.portals, []);
  assert.equal(sent.repickPortal, undefined);
  assert.deepEqual(sent.portGroups, [{ groupId: 1, state: 'optimized', preferred: false }]);
  assert.equal(sent.confirmAllInterfaces, false);
  assert.equal(sent.sudoPassword, 'hunter2');
  screen.dispose();
});

test('pausing a target flips only `enabled` and re-confirms an all-interface portal', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasTargetUpdateRequest: (payload) => { sent = payload; return { job: { jobId: 'j1', kind: 'target_update', subject: 'vm-store' } }; },
  });
  await setTargetEnabled(screen, iscsiTarget(), false, null);
  assert.equal(sent.enabled, false);
  assert.equal(sent.confirmAllInterfaces, false);
  // Pausing a target is not a request to move its portal.
  assert.deepEqual(sent.portals, []);
  assert.equal(sent.repickPortal, undefined);
  // A target that already lives on 0.0.0.0 must not be refused when it is
  // resumed: the decision was taken when it was created.
  await setTargetEnabled(screen, iscsiTarget({ portals: [{ interface: '', address: '0.0.0.0', port: 3260, transport: 'tcp' }] }), true, null);
  assert.equal(sent.enabled, true);
  assert.equal(sent.confirmAllInterfaces, true);
  screen.dispose();
});

test('deleting is retype-gated and promises the volume survives', async () => {
  let sent = null;
  const screen = fakeScreen({
    tentaNasTargetDeleteRequest: (payload) => { sent = payload; return { job: { jobId: 'j2', kind: 'target_delete', subject: 'vm-store' } }; },
  });
  const dialog = openTargetDeleteDialog(screen, iscsiTarget(), null);
  await flush();
  const text = dialog.textContent;
  assert.match(text, /surowego dysku/);
  assert.match(text, /tank\/vm-store/);
  // The two logged-in initiators are named as a loss, the zvol as a keep. The
  // COUNT, not the digit: `/2/` also matched the `2199023255552` two lines up.
  assert.match(text, /Zalogowane initiatory \(2\)/);
  assert.equal(dialog.querySelectorAll('.loss-list .ll.good').length, 1);

  const input = dialog.querySelector('tf-input');
  input.value = 'vm-store';
  input.dispatchEvent(new window.CustomEvent('input', { bubbles: true }));
  await flush();
  const confirm = dialog.querySelector('[data-action="confirm"]');
  assert.ok(!confirm.hasAttribute('disabled'), 'the retyped name unlocks the button');
  dialog.dispatchEvent(new window.CustomEvent('action', { detail: { action: 'confirm' }, cancelable: true }));
  await flush();
  await flush();
  assert.deepEqual(
    { targetId: sent.targetId, confirmName: sent.confirmName, sudoPassword: sent.sudoPassword },
    { targetId: 't1', confirmName: 'vm-store', sudoPassword: 'hunter2' },
  );
  await settled();
  screen.dispose();
});

test('every icon this module names exists in the application sprite', () => {
  // `sprite()` and the components expand an icon name straight into
  // `<use href="#i-NAME">` with no fallback, so a name the sprite does not
  // carry renders NOTHING and nothing anywhere complains. `i-target` was
  // missing for four rounds: the n12 card title, every target row, the empty
  // state, the wizard header, the iSCSI choice card and two window headers
  // all drew a blank box, which is the first thing an admin sees.
  //
  // The mockups declare the symbols, so this is a contract, not a preference —
  // and it is exactly the kind of cross-file gap no unit test of either file
  // alone can see.
  const sprite = readFileSync(new URL('../../../index.html', import.meta.url), 'utf8');
  const declared = new Set([...sprite.matchAll(/id="i-([a-z0-9-]+)"/g)].map((m) => m[1]));
  assert.ok(declared.size > 100, `the sprite did not parse: ${declared.size} symbols`);
  for (const file of ['targets.js', 'target-wizard.js', 'format.js']) {
    const source = readFileSync(new URL(`./${file}`, import.meta.url), 'utf8');
    // Interpolated icons count too. `icon="${t.enabled ? 'pause' : 'play'}"`
    // is how the pause button of EVERY row and of the detail footer picks its
    // symbol, and the old regex — which required the whole attribute value to
    // be a literal name — saw none of them. Third pattern: any quoted
    // lowercase word inside an `icon="${…}"` interpolation.
    const used = [...source.matchAll(/sprite\('([a-z0-9-]+)'\)|icon="([a-z0-9-]+)"/g)]
      .map((m) => m[1] || m[2]);
    for (const m of source.matchAll(/icon="\$\{([^}]*)\}"/g)) {
      for (const lit of m[1].matchAll(/'([a-z0-9-]+)'/g)) used.push(lit[1]);
    }
    // `format.js` names no icon at all — it only re-exports `sprite` — so it
    // is allowed to contribute nothing. Every other file in the list draws,
    // and a scan that suddenly finds nothing there is a broken scan, not a
    // module that stopped using icons.
    if (file !== 'format.js') {
      assert.ok(used.length, `${file}: the icon scan found nothing, so it is asserting nothing`);
    }
    for (const name of used) {
      assert.ok(declared.has(name), `${file} draws #i-${name}, which the sprite does not declare`);
    }
  }
});

test('a session count nobody measured reads as a dash, and so does a missing field', () => {
  // The safe default is "unknown". A confident 0 in the delete dialog's blast
  // radius is the one number that costs a client its disk mid-write, and the
  // node's own field is `#[serde(default)]` — so a response that carries
  // nothing must not print a number either.
  assert.equal(sessionsCountLabel({ sessions: 3, sessionsKnown: true }), '3');
  assert.equal(sessionsCountLabel({ sessions: 0, sessionsKnown: true }), '0');
  assert.equal(sessionsCountLabel({ sessions: 0, sessionsKnown: false }), '—');
  assert.equal(sessionsCountLabel({ sessions: 2 }), '—', 'a missing field is not a measurement');
  // Against the STRINGS, not against each other: comparing two calls of the
  // same function passes for any implementation, including one that returns
  // '' for both.
  assert.equal(sessionsEmptyText({ sessions: 0, sessionsKnown: true }), 'Brak zalogowanych initiatorów.');
  assert.match(sessionsEmptyText({ sessionsKnown: false }), /debugfs/);
  // …and the missing-field case against the STRING as well, not only against
  // the other call: `assert.equal(f(a), f(b))` is true for any implementation
  // that returns the same thing for both, `''` included.
  assert.match(sessionsEmptyText({ sessions: 0 }), /debugfs/);
  assert.equal(sessionsEmptyText({ sessions: 0 }), sessionsEmptyText({ sessionsKnown: false }));
});

test('the allowlist parser drops blanks and duplicates, and the group states are named', () => {
  assert.deepEqual(parseInitiators('a\n b ;a,\n\nc'), ['a', 'b', 'c']);
  assert.deepEqual(parseInitiators(''), []);
  assert.equal(groupStateLabel('optimized'), 'Active/Optimized');
  assert.equal(groupStateLabel('non-optimized'), 'Active/Non-optimized');
  assert.equal(groupStateLabel('transitioning'), 'W trakcie zmiany');
  // An unknown state is shown AS IT IS. Falling back to "Active/Optimized"
  // would report the most optimistic possible reading of a path whose real
  // state this build cannot name.
  assert.equal(groupStateLabel('zzz'), 'zzz');
  assert.equal(groupStateLabel(''), '—');
});
