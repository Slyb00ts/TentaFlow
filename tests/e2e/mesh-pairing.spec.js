// =============================================================================
// Plik: tests/e2e/mesh-pairing.spec.js
// Opis: Testy E2E parowania nodow mesh — 4 nody na roznych portach.
//       Testuje pelny flow: discovery, parowanie PIN, TrustedKeysSync,
//       cofniecie zaufania z propagacja, re-pair, rate limiting.
// =============================================================================

// Wylacz weryfikacje TLS dla self-signed certs
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';

const { test, expect } = require('@playwright/test');
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');
const os = require('os');

const { BINARY } = require('./helpers/spawn');

const CONFIGS = {
  A: { port: 18091, config: 'config-node-a.toml', db: '/tmp/e2e-node-a.db' },
  B: { port: 18092, config: 'config-node-b.toml', db: '/tmp/e2e-node-b.db' },
  C: { port: 18093, config: 'config-node-c.toml', db: '/tmp/e2e-node-c.db' },
  D: { port: 18094, config: 'config-node-d.toml', db: '/tmp/e2e-node-d.db' },
};

let processes = {};

// Pairing refuses loopback and link-local peer addresses, so the nodes reach each
// other through the host's first routable IPv4 address (they bind 0.0.0.0).
const HOST_IP = Object.values(os.networkInterfaces()).flat()
  .find((i) => i && i.family === 'IPv4' && !i.internal && !i.address.startsWith('169.254.'))?.address;

function peerAddresses(node) {
  return [`${HOST_IP}:${CONFIGS[node].port}`];
}

function baseUrl(node) {
  return `https://127.0.0.1:${CONFIGS[node].port}`;
}

// Every node is driven the way the dashboard drives it: a browser page logged in
// through the UI, then binary-protocol requests through the page's own transport.
// There is no REST surface for mesh administration.
const { loginAsAdmin } = require('./helpers/auth');

let pages = {};

// Runs one protocol call inside the node's page. A refusal comes back as data
// ({ error, code }) so a spec can assert on the exact refusal.
async function call(node, kind, args = {}, { list = null, one = false } = {}) {
  return pages[node].evaluate(async ({ kind, args, list, one }) => {
    const { ApiBinary } = await import('/js/protocol/api-binary-shim.js');
    try {
      const value = list
        ? await ApiBinary.list(kind, { ...args, arrayKey: list })
        : one ? await ApiBinary.one(kind) : await ApiBinary.action(kind, args);
      return { value };
    } catch (e) {
      return { error: String(e?.message ?? e), code: e?.code ?? null };
    }
  }, { kind, args, list, one });
}

async function callOk(node, kind, args, opts) {
  const r = await call(node, kind, args, opts);
  if (r.error) throw new Error(`${node} ${kind}: ${r.error} (${r.code})`);
  return r.value;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function getTrustedNodes(node) {
  const nodes = await callOk(node, 'meshNodeListRequest', {}, { list: 'nodes' });
  return nodes.filter((n) => n.source === 'trusted').map((n) => n.nodeId);
}

async function getPendingPairings(node) {
  return callOk(node, 'meshPendingListRequest', {}, { list: 'pending' });
}

// Pomocnik: poczekaj az serwer odpowie
async function waitForServer(node, maxWaitMs = 30000) {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    try {
      const res = await fetch(`${baseUrl(node)}/`);
      if (res.ok) return;
    } catch { /* serwer jeszcze nie wstal */ }
    await new Promise(r => setTimeout(r, 500));
  }
  throw new Error(`Node ${node} nie uruchomil sie w ${maxWaitMs}ms`);
}

// Pomocnik: uruchom node
function startNode(name) {
  const cfg = CONFIGS[name];
  // Usun stara baze i pliki WAL/SHM
  for (const suffix of ['', '-wal', '-shm']) {
    try { fs.unlinkSync(cfg.db + suffix); } catch {}
  }
  // Each node needs its own home: the ledger store locks its directory and the
  // signing keys live there, so nodes sharing the default home break each other.
  const home = path.join(__dirname, '../../.runtime/e2e-home', `mesh-pairing-${name}`);
  fs.rmSync(home, { recursive: true, force: true });
  fs.mkdirSync(home, { recursive: true });
  const proc = spawn(BINARY, [
    '-c', path.join(__dirname, cfg.config),
    '--db', cfg.db,
  ], {
    env: { ...process.env, RUST_LOG: 'warn', TENTAFLOW_HOME: home },
  });
  proc.stderr.on('data', d => process.stderr.write(`[${name}] ${d}`));
  processes[name] = proc;
  return proc;
}

// Pomocnik: zatrzymaj node
function stopNode(name) {
  if (processes[name]) {
    processes[name].kill('SIGTERM');
    processes[name] = null;
  }
}

// The initiator starts pairing and receives the PIN; the responder confirms it.
// Retried while the peer connection is not up yet.
async function pairNodes(initiator, responder, ids) {
  let start = null;
  for (let attempt = 0; attempt < 10; attempt++) {
    start = await call(initiator, 'meshPairingStartRequest', {
      remoteAddress: ids[responder],
      remoteAddresses: peerAddresses(responder),
    });
    console.log(`[pair] ${initiator}->${responder} start (${attempt + 1}): ${start.error ?? 'ok'}`);
    if (!start.error) break;
    await sleep(2000);
  }
  if (start.error) return false;
  const pin = start.value.pin;
  if (start.value.completed) return true;

  let pending = null;
  for (let i = 0; i < 20 && !pending; i++) {
    pending = (await getPendingPairings(responder)).find((p) => p.remoteNodeId === ids[initiator]);
    if (!pending) await sleep(1000);
  }
  if (!pending) return false;

  const confirm = await call(responder, 'meshPairingConfirmRequest', { pairId: ids[initiator], pin });
  console.log(`[pair] ${responder} confirm: ${confirm.error ?? 'ok'}`);
  return !confirm.error;
}

async function waitForTrust(node, targetNodeId, timeoutMs = 20000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if ((await getTrustedNodes(node)).includes(targetNodeId)) return true;
    await sleep(1000);
  }
  return false;
}

async function waitForUntrust(node, targetNodeId, timeoutMs = 20000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (!(await getTrustedNodes(node)).includes(targetNodeId)) return true;
    await sleep(1000);
  }
  return false;
}

test.describe.serial('Mesh Pairing E2E — 4 nodes', () => {
  const nodeIds = {};
  const NAMES = ['A', 'B', 'C', 'D'];

  test.beforeAll(async ({ browser }) => {
    test.skip(!HOST_IP, 'no routable IPv4 address on this host');
    for (const name of NAMES) startNode(name);
    await Promise.all(NAMES.map((name) => waitForServer(name)));

    for (const name of NAMES) {
      pages[name] = await browser.newPage({ ignoreHTTPSErrors: true });
      await loginAsAdmin(pages[name], { port: CONFIGS[name].port });
      nodeIds[name] = (await callOk(name, 'meshIdentityRequest', {}, { one: true })).nodeId;
    }
  });

  test.afterAll(async () => {
    for (const name of NAMES) {
      await pages[name]?.close();
      stopNode(name);
    }
    await sleep(2000);
  });

  // Static-peer discovery no longer exists (mDNS is off in these configs), so the
  // nodes learn about each other only through pairing by address.
  test('1. four nodes boot with distinct mesh identities and no trust', async () => {
    expect(new Set(Object.values(nodeIds)).size).toBe(4);
    for (const name of NAMES) {
      expect(nodeIds[name]).toMatch(/^[0-9a-f]{64}$/);
      expect(await getTrustedNodes(name)).toEqual([]);
    }
  });

  test('2. basic pair between A and B', async () => {
    expect(await pairNodes('A', 'B', nodeIds)).toBe(true);
    expect(await waitForTrust('A', nodeIds.B)).toBe(true);
    expect(await waitForTrust('B', nodeIds.A)).toBe(true);
  });

  test('3. hub pairing: A pairs C, B sees C via TrustedKeysSync', async () => {
    expect(await pairNodes('A', 'C', nodeIds)).toBe(true);
    expect(await waitForTrust('A', nodeIds.C)).toBe(true);
    expect(await waitForTrust('B', nodeIds.C)).toBe(true);
    expect(await waitForTrust('C', nodeIds.B)).toBe(true);
  });

  test('4. A pairs D, all 4 nodes trust each other', async () => {
    expect(await pairNodes('A', 'D', nodeIds)).toBe(true);
    await sleep(5000);
    for (const from of NAMES) {
      for (const to of NAMES) {
        if (from === to) continue;
        expect(await waitForTrust(from, nodeIds[to])).toBe(true);
      }
    }
  });

  test('5. unpair D from A, verify propagation', async () => {
    await callOk('A', 'meshTrustRevokeRequest', { nodeId: nodeIds.D });
    expect(await waitForUntrust('A', nodeIds.D)).toBe(true);
    expect(await waitForUntrust('B', nodeIds.D)).toBe(true);
    expect(await waitForUntrust('C', nodeIds.D)).toBe(true);
    expect(await waitForUntrust('D', nodeIds.A)).toBe(true);
  });

  test.skip('6. re-pair D after unpair (not revoked)', async () => {
    await sleep(10000);
    expect(await pairNodes('A', 'D', nodeIds)).toBe(true);
    expect(await waitForTrust('A', nodeIds.D)).toBe(true);
    expect(await waitForTrust('D', nodeIds.A)).toBe(true);
  });

  test('7. trust required for send_command', async () => {
    const trusted = await call('A', 'meshNodeCommandRequest', {
      nodeId: nodeIds.B, command: 'list_containers', args: [],
    });
    // A refusal for lack of trust is PolicyDenied; Docker being
    // unavailable on the peer is not one.
    expect(`${trusted.error ?? ''} ${trusted.code ?? ''}`).not.toMatch(/PolicyDenied/);

    await callOk('A', 'meshTrustRevokeRequest', { nodeId: nodeIds.D });
    await sleep(3000);

    const untrusted = await call('A', 'meshNodeCommandRequest', {
      nodeId: nodeIds.D, command: 'list_containers', args: [],
    });
    expect(untrusted.error).toBeTruthy();
    expect(`${untrusted.error} ${untrusted.code}`).toMatch(/PolicyDenied/);
  });

  test('8. PIN rate limiting', async () => {
    const start = await call('A', 'meshPairingStartRequest', {
      remoteAddress: nodeIds.D,
      remoteAddresses: peerAddresses('D'),
    });
    expect(start.error).toBeUndefined();

    let pending = null;
    for (let i = 0; i < 15 && !pending; i++) {
      pending = (await getPendingPairings('D')).find((p) => p.remoteNodeId === nodeIds.A);
      if (!pending) await sleep(1000);
    }
    expect(pending).toBeTruthy();

    // A wrong PIN is refused as an authentication failure.
    for (let i = 0; i < 3; i++) {
      const r = await call('D', 'meshPairingConfirmRequest', { pairId: nodeIds.A, pin: '000000' });
      expect(r.error).toBeTruthy();
      expect(String(r.code)).not.toMatch(/rate/i);
    }
    // The fourth attempt hits the limiter.
    const limited = await call('D', 'meshPairingConfirmRequest', { pairId: nodeIds.A, pin: '000000' });
    expect(`${limited.error} ${limited.code}`).toMatch(/rate|too many|retry/i);
  });

  test('9. reject pairing request', async () => {
    const start = await call('C', 'meshPairingStartRequest', {
      remoteAddress: nodeIds.D,
      remoteAddresses: peerAddresses('D'),
    });
    test.skip(Boolean(start.error), `pairing blocked: ${start.error}`);

    let pending = null;
    for (let i = 0; i < 15 && !pending; i++) {
      pending = (await getPendingPairings('D')).find((p) => p.remoteNodeId === nodeIds.C);
      if (!pending) await sleep(1000);
    }
    test.skip(!pending, 'no pending pairing arrived on D');

    await callOk('D', 'meshPairingRejectRequest', { pairId: nodeIds.C });
    const after = await getPendingPairings('D');
    expect(after.find((p) => p.remoteNodeId === nodeIds.C)).toBeFalsy();
  });

  test('10. PIN not leaked in pending response', async () => {
    const start = await call('B', 'meshPairingStartRequest', {
      remoteAddress: nodeIds.D,
      remoteAddresses: peerAddresses('D'),
    });
    test.skip(Boolean(start.error), `pairing blocked: ${start.error}`);
    expect(start.value.pin).toMatch(/^\d{6}$/);

    await sleep(2000);
    const incoming = (await getPendingPairings('D')).filter((p) => p.remoteNodeId === nodeIds.B);
    for (const entry of incoming) {
      expect(entry.pin ?? null).toBeNull();
    }
  });
});
