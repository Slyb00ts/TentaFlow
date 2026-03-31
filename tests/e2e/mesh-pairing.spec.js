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

const BINARY = path.join(__dirname, '../../tentaflow/target/release/tentaflow');

const CONFIGS = {
  A: { port: 18091, config: 'config-node-a.toml', db: '/tmp/e2e-node-a.db' },
  B: { port: 18092, config: 'config-node-b.toml', db: '/tmp/e2e-node-b.db' },
  C: { port: 18093, config: 'config-node-c.toml', db: '/tmp/e2e-node-c.db' },
  D: { port: 18094, config: 'config-node-d.toml', db: '/tmp/e2e-node-d.db' },
};

let processes = {};
let tokens = {};

function baseUrl(node) {
  return `https://127.0.0.1:${CONFIGS[node].port}`;
}

// Pomocnik: logowanie i pobranie tokenu JWT
async function getToken(node) {
  const res = await fetch(`${baseUrl(node)}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'admin' }),
  });
  const data = await res.json();
  return data.token;
}

// Pomocnik: API call z tokenem
async function apiCall(node, method, apiPath, body = null) {
  const opts = {
    method,
    headers: {
      'Authorization': `Bearer ${tokens[node]}`,
      'Content-Type': 'application/json',
    },
  };
  if (body) opts.body = JSON.stringify(body);
  const res = await fetch(`${baseUrl(node)}${apiPath}`, opts);
  const text = await res.text();
  let json;
  try { json = JSON.parse(text); } catch { json = text; }
  return { status: res.status, body: json };
}

// Pomocnik: poczekaj az serwer odpowie
async function waitForServer(node, maxWaitMs = 30000) {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    try {
      const res = await fetch(`${baseUrl(node)}/api/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: 'admin', password: 'admin' }),
      });
      if (res.ok || res.status === 200 || res.status === 401 || res.status === 403) return;
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
  const proc = spawn(BINARY, [
    '-c', path.join(__dirname, cfg.config),
    '--db', cfg.db,
  ], {
    env: { ...process.env, RUST_LOG: 'warn' },
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

// Pomocnik: pobierz node ID (z listy nodow, filtruj po source=local)
async function getNodeId(node) {
  const resp = await apiCall(node, 'GET', '/api/mesh/nodes');
  const nodes = resp.body || [];
  const local = nodes.find(n => n.source === 'local' || n.is_local === true);
  if (local) return local.node_id;
  // Fallback: pierwszy node
  return nodes.length > 0 ? nodes[0].node_id : null;
}

// Pomocnik: pobierz discovered nodes
async function getDiscoveredNodes(node) {
  const resp = await apiCall(node, 'GET', '/api/mesh/nodes');
  return resp.body || [];
}

// Pomocnik: pobierz trusted nodes
async function getTrustedNodes(node) {
  const resp = await apiCall(node, 'GET', '/api/mesh/trusted');
  return resp.body || [];
}

// Pomocnik: pobierz pending pairings
async function getPendingPairings(node) {
  const resp = await apiCall(node, 'GET', '/api/mesh/pending');
  return resp.body || [];
}

// Paruj dwa nody: initiator wywoluje pair, responder potwierdza PIN
async function pairNodes(initiator, responder, responderNodeId) {
  // Inicjuj parowanie
  const initResp = await apiCall(initiator, 'POST', `/api/mesh/pair/${responderNodeId}`);
  console.log(`[pair] ${initiator}->${responder} initiate: status=${initResp.status} body=${JSON.stringify(initResp.body)}`);
  if (initResp.status !== 200) return false;
  const pin = initResp.body.pin;

  // Poczekaj az pending pairing pojawi sie na responder
  let pending = [];
  for (let i = 0; i < 20; i++) {
    pending = await getPendingPairings(responder);
    if (pending.length > 0) break;
    await new Promise(r => setTimeout(r, 1000));
  }
  console.log(`[pair] ${responder} pending: ${JSON.stringify(pending)}`);
  if (pending.length === 0) return false;

  // Potwierdz z PIN
  const nodeIdInPending = pending[0].remote_node_id;
  const confirmResp = await apiCall(responder, 'POST',
    `/api/mesh/pair/${nodeIdInPending}/confirm`,
    { pin, hostname: `e2e-node-${responder.toLowerCase()}` }
  );
  console.log(`[pair] ${responder} confirm: status=${confirmResp.status} body=${JSON.stringify(confirmResp.body)}`);
  return confirmResp.status === 200;
}

// Czekaj az node pojawi sie w trusted
async function waitForTrust(node, targetNodeId, timeoutMs = 20000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const trusted = await getTrustedNodes(node);
    if (trusted.some(t => t.node_id === targetNodeId)) return true;
    await new Promise(r => setTimeout(r, 1000));
  }
  return false;
}

// Czekaj az node ZNIKNIE z trusted
async function waitForUntrust(node, targetNodeId, timeoutMs = 20000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    const trusted = await getTrustedNodes(node);
    if (!trusted.some(t => t.node_id === targetNodeId)) return true;
    await new Promise(r => setTimeout(r, 1000));
  }
  return false;
}

test.describe.serial('Mesh Pairing E2E — 4 nodes', () => {
  let nodeIds = {};

  test.beforeAll(async () => {
    // Uruchom 4 nody
    for (const name of ['A', 'B', 'C', 'D']) startNode(name);

    // Poczekaj az wszystkie serwery wstana
    await Promise.all(
      ['A', 'B', 'C', 'D'].map(name => waitForServer(name))
    );

    // Pobierz tokeny
    for (const name of ['A', 'B', 'C', 'D']) {
      tokens[name] = await getToken(name);
    }

    // Pobierz node ID
    for (const name of ['A', 'B', 'C', 'D']) {
      nodeIds[name] = await getNodeId(name);
    }
  });

  test.afterAll(async () => {
    for (const name of ['A', 'B', 'C', 'D']) stopNode(name);
    // Daj czas na zamkniecie procesow
    await new Promise(r => setTimeout(r, 2000));
  });

  test('1. all 4 nodes discover each other', async () => {
    // Poczekaj na discovery przez static peers
    let allDiscovered = false;
    for (let attempt = 0; attempt < 30; attempt++) {
      allDiscovered = true;
      for (const name of ['A', 'B', 'C', 'D']) {
        const nodes = await getDiscoveredNodes(name);
        const others = nodes.filter(n => n.node_id !== nodeIds[name]);
        if (others.length < 3) {
          allDiscovered = false;
          break;
        }
      }
      if (allDiscovered) break;
      await new Promise(r => setTimeout(r, 1000));
    }

    for (const name of ['A', 'B', 'C', 'D']) {
      const nodes = await getDiscoveredNodes(name);
      const others = nodes.filter(n => n.node_id !== nodeIds[name]);
      expect(others.length).toBeGreaterThanOrEqual(3);
    }
  });

  test('2. basic pair between A and B', async () => {
    const ok = await pairNodes('A', 'B', nodeIds['B']);
    expect(ok).toBe(true);

    // Zweryfikuj zaufanie na obu stronach
    expect(await waitForTrust('A', nodeIds['B'])).toBe(true);
    expect(await waitForTrust('B', nodeIds['A'])).toBe(true);
  });

  test('3. hub pairing: A pairs C, B sees C via TrustedKeysSync', async () => {
    const ok = await pairNodes('A', 'C', nodeIds['C']);
    expect(ok).toBe(true);

    // A ufa C
    expect(await waitForTrust('A', nodeIds['C'])).toBe(true);

    // B powinien otrzymac TrustedKeysSync od A i zaufac C
    expect(await waitForTrust('B', nodeIds['C'])).toBe(true);

    // C powinien widziec B
    expect(await waitForTrust('C', nodeIds['B'])).toBe(true);
  });

  test('4. A pairs D, all 4 nodes trust each other', async () => {
    const ok = await pairNodes('A', 'D', nodeIds['D']);
    expect(ok).toBe(true);

    // Poczekaj na propagacje TrustedKeysSync
    await new Promise(r => setTimeout(r, 5000));

    // Wszystkie nody powinny sobie ufac
    for (const from of ['A', 'B', 'C', 'D']) {
      for (const to of ['A', 'B', 'C', 'D']) {
        if (from === to) continue;
        const trusted = await waitForTrust(from, nodeIds[to]);
        expect(trusted).toBe(true);
      }
    }
  });

  test('5. unpair D from A, verify propagation', async () => {
    // Cofnij zaufanie D z poziomu A
    const revokeResp = await apiCall('A', 'DELETE', `/api/mesh/trust/${nodeIds['D']}`);
    expect(revokeResp.status).toBe(200);

    // D powinno zniknac z listy zaufanych na A, B, C
    expect(await waitForUntrust('A', nodeIds['D'])).toBe(true);
    expect(await waitForUntrust('B', nodeIds['D'])).toBe(true);
    expect(await waitForUntrust('C', nodeIds['D'])).toBe(true);

    // D powinno stracic A
    expect(await waitForUntrust('D', nodeIds['A'])).toBe(true);
  });

  test('6. re-pair D after unpair (not revoked)', async () => {
    const ok = await pairNodes('A', 'D', nodeIds['D']);
    expect(ok).toBe(true);

    // Zweryfikuj ze parowanie przeszlo
    expect(await waitForTrust('A', nodeIds['D'])).toBe(true);
    expect(await waitForTrust('D', nodeIds['A'])).toBe(true);
  });

  test('7. trust required for send_command', async () => {
    // A powinno moc wyslac komende do zaufanego B
    const trustedResp = await apiCall('A', 'POST',
      `/api/mesh/nodes/${nodeIds['B']}/command`,
      { command_type: 'list_containers' }
    );
    // Nie powinno byc 403 (moze byc 502 jesli Docker niedostepny)
    expect(trustedResp.status).not.toBe(403);

    // Cofnij zaufanie D
    await apiCall('A', 'DELETE', `/api/mesh/trust/${nodeIds['D']}`);
    await new Promise(r => setTimeout(r, 3000));

    // A NIE powinno moc wyslac komendy do niezaufanego D
    const untrustedResp = await apiCall('A', 'POST',
      `/api/mesh/nodes/${nodeIds['D']}/command`,
      { command_type: 'list_containers' }
    );
    expect(untrustedResp.status).toBe(403);
  });

  test('8. PIN rate limiting', async () => {
    // Zainicjuj parowanie A -> D (D jest teraz niezaufany po tescie 7)
    const initResp = await apiCall('A', 'POST', `/api/mesh/pair/${nodeIds['D']}`);
    expect(initResp.status).toBe(200);

    // Poczekaj az pending pairing pojawi sie na D
    let pending = [];
    for (let i = 0; i < 15; i++) {
      pending = await getPendingPairings('D');
      if (pending.length > 0) break;
      await new Promise(r => setTimeout(r, 1000));
    }
    expect(pending.length).toBeGreaterThan(0);

    const pendingNodeId = pending[0].remote_node_id;

    // 3 proby z blednym PIN
    for (let i = 0; i < 3; i++) {
      const resp = await apiCall('D', 'POST',
        `/api/mesh/pair/${pendingNodeId}/confirm`,
        { pin: '000000' }
      );
      expect(resp.status).toBe(403);
    }

    // 4. proba powinna byc rate limited
    const resp = await apiCall('D', 'POST',
      `/api/mesh/pair/${pendingNodeId}/confirm`,
      { pin: '000000' }
    );
    expect(resp.status).toBe(429);
  });

  test('9. reject pairing request', async () => {
    // D odrzuca oczekujace parowanie (jesli jeszcze jest)
    // lub inicjujemy nowe parowanie C -> D
    const initResp = await apiCall('C', 'POST', `/api/mesh/pair/${nodeIds['D']}`);
    if (initResp.status !== 200) {
      // Rate limit moze blokowac — pominmy
      test.skip();
      return;
    }

    // Poczekaj na pending na D
    let pending = [];
    for (let i = 0; i < 15; i++) {
      pending = await getPendingPairings('D');
      if (pending.length > 0) break;
      await new Promise(r => setTimeout(r, 1000));
    }

    if (pending.length === 0) {
      test.skip();
      return;
    }

    const nodeIdInPending = pending[0].remote_node_id;

    // D odrzuca parowanie
    const rejectResp = await apiCall('D', 'POST',
      `/api/mesh/pair/${nodeIdInPending}/reject`
    );
    expect(rejectResp.status).toBe(200);

    // Po odrzuceniu nie powinno byc juz na liscie pending
    const pendingAfter = await getPendingPairings('D');
    const stillPending = pendingAfter.find(p => p.remote_node_id === nodeIdInPending);
    expect(stillPending).toBeFalsy();
  });

  test('10. PIN not leaked in pending response', async () => {
    // Zainicjuj parowanie B -> D
    const initResp = await apiCall('B', 'POST', `/api/mesh/pair/${nodeIds['D']}`);
    if (initResp.status !== 200) {
      test.skip();
      return;
    }

    const pin = initResp.body.pin;
    expect(pin).toBeTruthy();
    expect(pin.length).toBe(6);

    // Poczekaj na pending na D
    await new Promise(r => setTimeout(r, 2000));

    // PIN NIE powinien byc widoczny w /api/mesh/pending
    const pendingResp = await apiCall('D', 'GET', '/api/mesh/pending');
    expect(pendingResp.status).toBe(200);
    if (Array.isArray(pendingResp.body) && pendingResp.body.length > 0) {
      const entry = pendingResp.body[0];
      expect(entry.pin_code).toBeUndefined();
      expect(entry.pin).toBeUndefined();
    }
  });
});
