// =============================================================================
// File: tests/e2e/code-studio-remote-sandbox.spec.js
// Description: A workspace's execution mode is chosen for a NODE, so a mode a
//              peer can deliver must be offered on that peer. The regression
//              this suite pins: the node picker only ever learned whether the
//              LOCAL node can isolate a process — every remote row arrived
//              without a value and the dashboard read that absence as a
//              refusal, which made `process_sandbox` unchoosable on every peer.
//
//              Both isolation modes are read off the SAME remote row: the
//              process-sandbox card and the container card are asserted
//              together, plus the egress fact the node list prints for that
//              node, so a row cannot be right about one mode and silently wrong
//              about the other.
//
//              Two REAL nodes are started here and paired over the real mesh
//              (iroh LAN discovery + the PIN handshake from the binary
//              protocol), and the assertions read the wizard's own DOM: each
//              card of the remote node carries the state its OWN probe
//              advertised, and a peer that reports nothing is refused with the
//              sentence about the owner — never with a fabricated cause.
//
//              TF_E2E_PEER_BINARY selects the peer's build and TF_E2E_PEER_EXPECT
//              (advertises|nothing) says what that build is known to report, so
//              the same suite covers both sides of the wire: a peer from this
//              tree advertises its probe, an older peer reports no field at all.
//
//              `advertises` is the CI mode. `nothing` needs an older build and
//              is therefore a DOCUMENTED MANUAL run, not a second binary wired
//              into CI:
//
//                TF_E2E_PEER_BINARY=/path/to/old/tentaflow \
//                TF_E2E_PEER_EXPECT=nothing \
//                npx playwright test tests/e2e/code-studio-remote-sandbox.spec.js
//
//              Both modes are also run by hand when the wire fields change,
//              because only an old binary can show what a pre-field peer sees.
// =============================================================================

const { test, expect } = require('@playwright/test');
const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');

const {
  BINARY,
  CONFIG_TEMPLATE,
  binaryExists,
  startBinary,
  stopBinary,
  waitForServer,
} = require('./helpers/spawn');
const { loginAsAdmin } = require('./helpers/auth');
const { api, ensureCodeStudioApp } = require('./helpers/code-studio');

const A_PORT = 18131;
const PEER_PORT = 18132;
const A_DB = '/tmp/e2e-cs-remote-sandbox-a.db';
const PEER_DB = '/tmp/e2e-cs-remote-sandbox-peer.db';
const RUNTIME = path.join(__dirname, '../../.runtime/code-studio-remote-sandbox');
const ARTIFACTS = path.join(RUNTIME, 'artifacts');

// The peer under test is the binary itself unless a run names another build.
const PEER_BINARY = process.env.TF_E2E_PEER_BINARY || BINARY;
const PEER_EXPECT = process.env.TF_E2E_PEER_EXPECT ?? 'advertises';

let nodeA;
let peer;
const peerLog = [];

function renderPairConfig(port) {
  const out = `/tmp/e2e-cs-remote-${port}.toml`;
  const cfg = fs
    .readFileSync(CONFIG_TEMPLATE, 'utf8')
    .replace(/"0\.0\.0\.0:18099"/g, `"0.0.0.0:${port}"`)
    .replace(/^port = 18099$/m, `port = ${port}`)
    .replace(/^health_check_bind = "0\.0\.0\.0:19889"$/m, `health_check_bind = "0.0.0.0:${port + 1000}"`)
    // Two nodes on one host find each other over real LAN discovery: the
    // `static_peers` config key is read by nothing at runtime, so mDNS is the
    // only path an operator has either.
    .replace('[mesh]\nenabled = false', '[mesh]\nenabled = true')
    .replace('mdns_enabled = false', 'mdns_enabled = true');
  fs.writeFileSync(out, cfg);
  return out;
}

function startPeer() {
  for (const suffix of ['', '-wal', '-shm']) {
    try { fs.unlinkSync(PEER_DB + suffix); } catch { /* absent */ }
  }
  const home = path.join(RUNTIME, 'peer-home');
  fs.mkdirSync(home, { recursive: true });
  const proc = spawn(PEER_BINARY, ['-c', renderPairConfig(PEER_PORT), '--db', PEER_DB], {
    env: { ...process.env, RUST_LOG: 'warn', TENTAFLOW_HOME: home },
  });
  proc.stderr.on('data', (d) => peerLog.push(d.toString()));
  proc.stdout.on('data', (d) => peerLog.push(d.toString()));
  return proc;
}

// The dashboard's own translation of a picker note, so the assertion compares
// against the shipped bundle rather than against a copy of the sentence. A note
// that names a node is resolved with the same `{node}` placeholder the module
// passes, so the comparison still exercises the real interpolation.
async function noteText(page, key, vars = null) {
  return page.evaluate(async ([k, v]) => {
    const { I18n } = await import('/js/i18n.js');
    return I18n.t(`code_studio.${k}`, v);
  }, [key, vars]);
}

// Waits for a value of a protocol listing, so a slow discovery or a slow
// pairing is a wait rather than a race.
async function poll(fn, { timeoutMs = 90_000, what = 'condition' } = {}) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    last = await fn().catch((e) => ({ error: String(e) }));
    if (last) return last;
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(`timed out waiting for ${what}; last value: ${JSON.stringify(last)}`);
}

function idOf(entry) {
  return String(entry?.nodeId ?? entry?.node_id ?? '');
}

async function selectNode(page, nodeId) {
  // tf-select keeps the real <select> in its shadow root; Playwright pierces it.
  await page.selectOption('#cs-wz-node select', nodeId);
}

async function readCard(page, mode) {
  return page.evaluate((value) => {
    const el = document.querySelector(`#cs-wz-modes [value=${value}]`);
    // `tf-choice-card` renders into its own light DOM (`_build` appends the
    // card element as a child), so the note paragraph is reached without a
    // shadow root.
    const note = el?.querySelector('.tf-choice-card__note');
    return {
      disabled: !!el?.disabled,
      note: String(el?.note ?? ''),
      rendered: note ? note.textContent : '',
      title: String(el?.title ?? ''),
    };
  }, mode);
}

// The node list is the wizard's own summary of a node: it prints the container
// fact and the egress enforcement this Core would apply there. The option text
// is `<name> — <fact> · <fact> · <fact>`, so the facts are returned as separate
// segments: a substring test could not tell "container runtime" from
// "no container runtime".
async function readNodeLabel(page, nodeId) {
  return page.evaluate((id) => {
    const select = document.querySelector('#cs-wz-node select');
    const option = Array.from(select?.options ?? []).find((o) => o.value === id);
    if (!option) return { text: '', facts: [] };
    const text = option.textContent;
    const parts = text.split(' · ');
    return { text, head: parts[0], facts: parts.slice(1) };
  }, nodeId);
}

test.describe.configure({ mode: 'serial' });

test.describe('Code Studio — process_sandbox on a REMOTE node', () => {
  test.beforeAll(async () => {
    if (!binaryExists()) {
      test.skip(true, 'tentaflow binary not built — run cargo build');
    }
    fs.mkdirSync(ARTIFACTS, { recursive: true });
    nodeA = startBinary({
      port: A_PORT,
      db: A_DB,
      home: path.join(RUNTIME, 'a-home'),
      configFile: renderPairConfig(A_PORT),
    });
    peer = startPeer();
    await Promise.all([waitForServer(A_PORT), waitForServer(PEER_PORT)]);
  });

  test.afterAll(async () => {
    stopBinary(nodeA);
    stopBinary(peer);
    fs.writeFileSync(path.join(ARTIFACTS, 'peer.log'), peerLog.join(''));
    await new Promise((r) => setTimeout(r, 1500));
  });

  test('two nodes trust each other after the real pairing handshake', async ({ browser }) => {
    const pageA = await browser.newPage();
    const pagePeer = await browser.newPage();
    await loginAsAdmin(pageA, { port: A_PORT });
    await loginAsAdmin(pagePeer, { port: PEER_PORT });

    const identityA = await api(pageA, 'meshIdentityRequest');
    const identityB = await api(pagePeer, 'meshIdentityRequest');
    const aId = String(identityA?.nodeId ?? identityA?.node_id ?? '');
    const bId = String(identityB?.nodeId ?? identityB?.node_id ?? '');
    expect(aId, 'the dashboard node has no mesh identity').toHaveLength(64);
    expect(bId, 'the peer node has no mesh identity').toHaveLength(64);

    // Discovery is the peer's OWN announcement — the catalog is built from the
    // peer store, so nothing below can pass without it.
    await poll(async () => {
      const list = await api(pageA, 'meshNodeListRequest');
      const ids = (list?.nodes ?? []).map(idOf);
      return ids.includes(bId) ? ids : null;
    }, { what: `node ${bId.slice(0, 12)} to be discovered by the dashboard node` });

    const started = await api(pageA, 'meshPairingStartRequest', {
      remoteAddress: bId,
      pinHint: '',
      remotePublicKey: '',
      remoteAddresses: [],
      remoteRelayUrl: '',
      remoteHostname: '',
    });
    const pin = String(started?.pin ?? '');
    expect(pin, 'pairing produced no PIN').toHaveLength(6);

    // The peer sees the pending request that arrived over the wire and answers
    // it with the PIN, exactly like an operator reading the code aloud.
    const pending = await poll(async () => {
      const body = await api(pagePeer, 'meshPendingListRequest');
      const row = (body?.pending ?? []).find((p) => String(p.remoteNodeId ?? p.remote_node_id) === aId);
      return row ?? null;
    }, { what: 'the pending pairing to reach the peer' });

    const confirmed = await api(pagePeer, 'meshPairingConfirmRequest', {
      pairId: String(pending.remoteNodeId ?? pending.remote_node_id),
      pin,
    });
    expect(confirmed?.ok ?? confirmed?.trustedNodeId ?? confirmed?.trusted_node_id).toBeTruthy();

    await poll(async () => {
      const body = await api(pageA, 'meshTrustedListRequest');
      const trusted = (body?.trusted ?? []).map(idOf);
      return trusted.includes(bId) ? trusted : null;
    }, { what: 'the peer to become trusted on the dashboard node' });

    await pageA.close();
    await pagePeer.close();
  });

  test('the wizard offers both isolation modes the remote node reports, and refuses the ones it does not', async ({ page }) => {
    await loginAsAdmin(page, { port: A_PORT });
    await ensureCodeStudioApp(page);

    // Workspace creation is gated by a per-user grant, which a fresh database
    // does not carry — without it the wizard button renders disabled.
    const me = await api(page, 'authMeRequest');
    const users = await api(page, 'usersListRequest');
    const mine = (users?.users ?? []).find((u) => (u.username ?? '') === (me?.username ?? 'admin'));
    const userId = mine?.id ?? mine?.userId ?? mine?.user_id ?? '';
    expect(userId, 'could not resolve the admin user id').toBeTruthy();
    await api(page, 'codeStudioWorkspaceCreatorGrantSetRequest', { userId, granted: true });

    await page.goto(`https://127.0.0.1:${A_PORT}/#/code-studio`);
    await page.locator('#cs-new, #cs-empty-new, #cs-table-host').first().waitFor({ timeout: 30_000 });

    const body = await api(page, 'codeStudioWorkspacesListRequest', { includeArchived: false });
    const nodes = body?.nodes ?? [];
    const remote = nodes.find((n) => !(n.isLocal ?? n.is_local));
    expect(remote, 'the paired peer did not reach the node catalog').toBeTruthy();
    const advertised = remote.supportsProcessSandbox ?? remote.supports_process_sandbox ?? null;
    const wireCause = remote.processSandboxCause ?? remote.process_sandbox_cause ?? null;
    const advertisedContainer = remote.supportsContainer ?? remote.supports_container ?? null;
    const wireEgress = String(remote.egressEnforcement ?? remote.egress_enforcement ?? '');

    if (PEER_EXPECT === 'advertises') {
      expect(advertised, 'the peer built from this tree did not advertise its probe').toBe(true);
      expect(wireCause, 'a node that advertises support must not carry a cause').toBeNull();
      // The container half of the same question travels in the same payload:
      // a peer from this tree always reports a boolean, never silence. WHICH
      // boolean depends on whether that build actually found a runtime socket,
      // so the assertion below is against the advertised value, not against
      // `true` — the defect being pinned is the absence of an answer.
      expect(typeof advertisedContainer,
        'the peer built from this tree did not advertise a container runtime answer').toBe('boolean');
      expect(wireEgress, 'a peer that can contain workspaces must be reported as enforcing egress there')
        .toBe(advertisedContainer === true ? 'namespace' : 'unrestricted');
    } else {
      expect(advertised, 'a peer without the field must stay unknown, never become support').toBeNull();
      expect(advertisedContainer,
        'a peer without the container field must stay unknown, never become a runtime').toBeNull();
      expect(wireEgress, 'an unknown container answer must not be reported as enforcement').toBe('unrestricted');
    }

    await page.locator('#cs-new, #cs-empty-new').first().click();
    await page.locator('#cs-wz-name input').first().fill(`remote-sandbox-${Date.now().toString(36)}`);
    await selectNode(page, idOf(remote));

    // The node list belongs to step 1, so its row for the node under test is
    // read HERE, before the step that carries the mode cards replaces it. The
    // container fact and the egress enforcement are read off this one line, so
    // the two rows cannot disagree about the same node.
    const remoteLabel = await readNodeLabel(page, idOf(remote));
    const containerFact = await noteText(page, advertisedContainer === true ? 'node_container_yes'
      : advertisedContainer === false ? 'node_container_no' : 'node_container_unknown');
    const otherFacts = await Promise.all((advertisedContainer === true
      ? ['node_container_no', 'node_container_unknown']
      : advertisedContainer === false ? ['node_container_yes', 'node_container_unknown']
        : ['node_container_yes', 'node_container_no']).map((k) => noteText(page, k)));
    const egressFact = await noteText(page, `enforcement_${wireEgress}`);

    await page.locator('[data-action="next"]').first().click();

    const card = await readCard(page, 'process_sandbox');
    const containerCard = await readCard(page, 'container');
    const expectedKey = advertised === true ? '' : PEER_EXPECT === 'advertises' ? 'mode_process_unavailable' : 'mode_process_remote';
    const expectedText = expectedKey ? await noteText(page, expectedKey) : '';
    const containerKey = advertisedContainer === true ? ''
      : advertisedContainer === false ? 'mode_container_unavailable' : 'mode_container_remote';
    const containerText = containerKey
      ? await noteText(page, containerKey, containerKey === 'mode_container_unavailable'
        ? { node: String(remote.name ?? '') } : null)
      : '';

    const localNode = nodes.find((n) => (n.isLocal ?? n.is_local));
    const localAdvertisedContainer = localNode?.supportsContainer ?? localNode?.supports_container ?? null;
    const localSteps = await (async () => {
      await page.locator('[data-action="back"]').first().click();
      await selectNode(page, idOf(localNode));
      await page.locator('[data-action="next"]').first().click();
      return {
        sandbox: await readCard(page, 'process_sandbox'),
        container: await readCard(page, 'container'),
      };
    })();

    fs.writeFileSync(
      path.join(ARTIFACTS, 'evidence.json'),
      JSON.stringify({
        peerBinary: PEER_BINARY,
        peerExpect: PEER_EXPECT,
        remote: {
          nodeId: idOf(remote), name: remote.name, advertised, wireCause, advertisedContainer, wireEgress,
        },
        remoteCard: card,
        remoteContainerCard: containerCard,
        remoteLabel,
        local: {
          nodeId: idOf(localNode),
          advertised: localNode?.supportsProcessSandbox ?? localNode?.supports_process_sandbox ?? null,
          cause: localNode?.processSandboxCause ?? localNode?.process_sandbox_cause ?? null,
          advertisedContainer: localAdvertisedContainer,
        },
        localCards: localSteps,
        expectedKey,
        expectedText,
        containerKey,
        containerText,
        containerFact,
        egressFact,
      }, null, 2),
    );
    await page.screenshot({ path: path.join(ARTIFACTS, 'wizard-remote-node.png'), fullPage: true });

    // The remote row carries the PEER's own answer: enabled when it reported a
    // working sandbox, and a refusal chosen from what it reported when it did
    // not — never this node's probe standing in for the peer's. The card's note
    // IS the translated sentence (`processSandboxNote` resolves the key through
    // `t`), so the comparison is against the shipped bundle for `expectedKey` —
    // a raw English probe sentence would not match it.
    expect(card.disabled, 'the sandbox card of the remote node').toBe(advertised !== true);
    expect(card.note, 'the sentence the picker chose for this node').toBe(expectedText);
    expect(card.rendered, 'the sentence the user actually reads').toBe(expectedText);
    expect(card.title, 'the probe sentence belongs to the node that produced it').toBe('');

    // The container card follows the SAME advertised row: a peer that answered
    // yes is choosable, and one that answered no or not at all is refused with
    // the sentence it earns. A silence must not be printed as a missing runtime.
    expect(containerCard.disabled, 'the container card of the remote node').toBe(advertisedContainer !== true);
    expect(containerCard.note, 'the container sentence the picker chose').toBe(containerText);
    expect(containerCard.rendered, 'the container sentence the user actually reads').toBe(containerText);
    if (advertisedContainer === null) {
      expect(containerCard.rendered, 'a peer that reported nothing must not be told to install a runtime')
        .not.toMatch(/Docker|OCI/);
    }

    // Both facts of the node list, for the node under test, compared as whole
    // segments so a "no container runtime" can never satisfy a "container
    // runtime" expectation.
    expect(remoteLabel.head, 'the node list lost the remote node name').toContain(String(remote.name ?? ''));
    expect(remoteLabel.facts, 'the node list lost the container fact').toContain(containerFact);
    expect(remoteLabel.facts, 'the node list lost the egress enforcement').toContain(egressFact);
    for (const other of otherFacts) {
      expect(remoteLabel.facts, 'the node list printed a container fact the node did not report').not.toContain(other);
    }

    // The local row is still measured HERE — the peer's report cannot turn this
    // node's own refusal into support, nor its silence into a runtime.
    const localValue = localNode?.supportsProcessSandbox ?? localNode?.supports_process_sandbox ?? null;
    expect(typeof localValue, 'the local row lost its measured value').toBe('boolean');
    expect(localSteps.sandbox.disabled).toBe(localValue !== true);
    expect(typeof localAdvertisedContainer,
      'the local row lost its measured container value').toBe('boolean');
    expect(localSteps.container.disabled).toBe(localAdvertisedContainer !== true);
    expect(localSteps.container.note, 'a value measured here needs no "ask the owner" sentence')
      .toBe(localAdvertisedContainer === true ? '' : await noteText(page, 'mode_container_unavailable',
        { node: String(localNode?.name ?? '') }));
  });
});
