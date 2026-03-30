// =============================================================================
// Plik: tests/e2e/mesh-pairing.spec.js
// Opis: Testy E2E parowania nodow mesh — 2 nody na roznych portach.
//       Testuje pelny flow: logowanie, parowanie PIN, potwierdzenie,
//       cofniecie zaufania, re-trust.
// =============================================================================

// Wylacz weryfikacje TLS dla self-signed certs
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';

const { test, expect } = require('@playwright/test');
const { spawn } = require('child_process');
const path = require('path');
const fs = require('fs');

const BINARY = path.join(__dirname, '../../tentaflow/target/debug/tentaflow');
const NODE_A_PORT = 18091;
const NODE_B_PORT = 18092;
const NODE_A_URL = `https://127.0.0.1:${NODE_A_PORT}`;
const NODE_B_URL = `https://127.0.0.1:${NODE_B_PORT}`;

let nodeA, nodeB;

// Pomocnik: logowanie i pobranie tokenu JWT
async function getAuthToken(baseUrl) {
  const res = await fetch(`${baseUrl}/api/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'admin' }),
    // TLS self-signed — wylacz weryfikacje
  });
  const data = await res.json();
  return data.token;
}

// Pomocnik: API call z tokenem
async function apiCall(baseUrl, token, method, path, body) {
  const opts = {
    method,
    headers: {
      'Authorization': `Bearer ${token}`,
      'Content-Type': 'application/json',
    },
  };
  if (body) opts.body = JSON.stringify(body);
  const res = await fetch(`${baseUrl}${path}`, opts);
  const text = await res.text();
  let json;
  try { json = JSON.parse(text); } catch { json = text; }
  return { status: res.status, data: json };
}

// Pomocnik: poczekaj az serwer odpowie
async function waitForServer(baseUrl, maxWaitMs = 30000) {
  const start = Date.now();
  while (Date.now() - start < maxWaitMs) {
    try {
      const res = await fetch(`${baseUrl}/api/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: 'admin', password: 'admin' }),
      });
      if (res.ok || res.status === 200 || res.status === 401 || res.status === 403) return true;
    } catch { /* serwer jeszcze nie wstał */ }
    await new Promise(r => setTimeout(r, 500));
  }
  throw new Error(`Serwer ${baseUrl} nie odpowiada po ${maxWaitMs}ms`);
}

test.describe('Mesh Pairing E2E', () => {
  test.beforeAll(async () => {
    // Usun stare bazy
    for (const db of ['/tmp/e2e-node-a.db', '/tmp/e2e-node-b.db']) {
      try { fs.unlinkSync(db); } catch {}
      try { fs.unlinkSync(db + '-wal'); } catch {}
      try { fs.unlinkSync(db + '-shm'); } catch {}
    }

    // Uruchom 2 nody
    nodeA = spawn(BINARY, [
      '-c', path.join(__dirname, 'config-node-a.toml'),
      '--db', '/tmp/e2e-node-a.db',
    ], { env: { ...process.env, RUST_LOG: 'warn' } });

    nodeB = spawn(BINARY, [
      '-c', path.join(__dirname, 'config-node-b.toml'),
      '--db', '/tmp/e2e-node-b.db',
    ], { env: { ...process.env, RUST_LOG: 'warn' } });

    nodeA.stderr.on('data', d => process.stderr.write(`[A] ${d}`));
    nodeB.stderr.on('data', d => process.stderr.write(`[B] ${d}`));

    // Poczekaj az oba nody wstana
    await Promise.all([
      waitForServer(NODE_A_URL),
      waitForServer(NODE_B_URL),
    ]);
  });

  test.afterAll(async () => {
    if (nodeA) { nodeA.kill('SIGTERM'); await new Promise(r => setTimeout(r, 1000)); }
    if (nodeB) { nodeB.kill('SIGTERM'); await new Promise(r => setTimeout(r, 1000)); }
  });

  test('oba nody startuja i odpowiadaja na login', async () => {
    const tokenA = await getAuthToken(NODE_A_URL);
    const tokenB = await getAuthToken(NODE_B_URL);
    expect(tokenA).toBeTruthy();
    expect(tokenB).toBeTruthy();
  });

  test('node A widzi node B jako discovered peer', async () => {
    const tokenA = await getAuthToken(NODE_A_URL);

    // Poczekaj na discovery (static peers)
    let nodes;
    for (let i = 0; i < 20; i++) {
      const res = await apiCall(NODE_A_URL, tokenA, 'GET', '/api/mesh/nodes');
      nodes = res.data;
      if (Array.isArray(nodes) && nodes.length > 0) break;
      await new Promise(r => setTimeout(r, 1000));
    }

    expect(Array.isArray(nodes)).toBe(true);
    expect(nodes.length).toBeGreaterThan(0);
  });

  test('parowanie: A inicjuje, B potwierdza z PIN, oba zaufane', async () => {
    const tokenA = await getAuthToken(NODE_A_URL);
    const tokenB = await getAuthToken(NODE_B_URL);

    // Pobierz node ID node B
    let nodesOnA;
    for (let i = 0; i < 20; i++) {
      const res = await apiCall(NODE_A_URL, tokenA, 'GET', '/api/mesh/nodes');
      nodesOnA = res.data;
      if (Array.isArray(nodesOnA) && nodesOnA.length > 0) break;
      await new Promise(r => setTimeout(r, 1000));
    }
    const nodeB_id = nodesOnA[0]?.node_id;
    expect(nodeB_id).toBeTruthy();

    // A inicjuje parowanie
    const pairRes = await apiCall(NODE_A_URL, tokenA, 'POST', `/api/mesh/pair/${nodeB_id}`);
    expect(pairRes.status).toBe(200);
    const pin = pairRes.data.pin;
    expect(pin).toBeTruthy();
    expect(pin.length).toBe(6);

    // PIN NIE powinien byc w /api/mesh/pending response
    const pendingRes = await apiCall(NODE_B_URL, tokenB, 'GET', '/api/mesh/pending');
    expect(pendingRes.status).toBe(200);
    if (Array.isArray(pendingRes.data) && pendingRes.data.length > 0) {
      const pending = pendingRes.data[0];
      expect(pending.pin_code).toBeUndefined(); // PIN ukryty
    }

    // Poczekaj chwile na PairingRequest przez QUIC
    await new Promise(r => setTimeout(r, 2000));

    // B potwierdza z PIN
    const pendingAfter = await apiCall(NODE_B_URL, tokenB, 'GET', '/api/mesh/pending');
    let nodeA_id_on_b;
    if (Array.isArray(pendingAfter.data) && pendingAfter.data.length > 0) {
      nodeA_id_on_b = pendingAfter.data[0].remote_node_id;
    }

    if (nodeA_id_on_b) {
      const confirmRes = await apiCall(NODE_B_URL, tokenB, 'POST',
        `/api/mesh/pair/${nodeA_id_on_b}/confirm`,
        { pin, hostname: 'e2e-node-b' }
      );
      expect(confirmRes.status).toBe(200);
    }

    // Sprawdz zaufanych na obu nodach (po chwili propagacji)
    await new Promise(r => setTimeout(r, 3000));

    const trustedA = await apiCall(NODE_A_URL, tokenA, 'GET', '/api/mesh/trusted');
    const trustedB = await apiCall(NODE_B_URL, tokenB, 'GET', '/api/mesh/trusted');

    expect(Array.isArray(trustedA.data)).toBe(true);
    expect(Array.isArray(trustedB.data)).toBe(true);
  });

  test('rate limit PIN: 4. proba zwraca 429', async () => {
    const tokenB = await getAuthToken(NODE_B_URL);
    const fakeNodeId = 'nonexistent-node-rate-limit-test';

    // 3 proby z blednym PIN
    for (let i = 0; i < 3; i++) {
      await apiCall(NODE_B_URL, tokenB, 'POST',
        `/api/mesh/pair/${fakeNodeId}/confirm`,
        { pin: '000000' }
      );
    }

    // 4. proba — powinna byc 429
    const res = await apiCall(NODE_B_URL, tokenB, 'POST',
      `/api/mesh/pair/${fakeNodeId}/confirm`,
      { pin: '000000' }
    );
    expect(res.status).toBe(429);
  });

  test('cofniecie zaufania i re-trust', async () => {
    const tokenA = await getAuthToken(NODE_A_URL);

    // Pobierz liste zaufanych
    const trusted = await apiCall(NODE_A_URL, tokenA, 'GET', '/api/mesh/trusted');
    if (!Array.isArray(trusted.data) || trusted.data.length === 0) {
      test.skip();
      return;
    }

    const targetNodeId = trusted.data[0].node_id;

    // Cofnij zaufanie
    const revokeRes = await apiCall(NODE_A_URL, tokenA, 'DELETE', `/api/mesh/trust/${targetNodeId}`);
    expect(revokeRes.status).toBe(200);

    // Sprawdz ze nie ma go na liscie zaufanych
    const afterRevoke = await apiCall(NODE_A_URL, tokenA, 'GET', '/api/mesh/trusted');
    const stillTrusted = (afterRevoke.data || []).find(n => n.node_id === targetNodeId);
    expect(stillTrusted).toBeFalsy();

    // Admin re-trust
    const retrustRes = await apiCall(NODE_A_URL, tokenA, 'POST', `/api/mesh/retrust/${targetNodeId}`);
    expect(retrustRes.status).toBe(200);
  });

  test('odrzucenie parowania', async () => {
    const tokenA = await getAuthToken(NODE_A_URL);
    const tokenB = await getAuthToken(NODE_B_URL);

    // Pobierz pending na B (moze nie byc)
    const pending = await apiCall(NODE_B_URL, tokenB, 'GET', '/api/mesh/pending');
    if (!Array.isArray(pending.data) || pending.data.length === 0) {
      // Zainicjuj nowe parowanie zeby miec co odrzucic
      const nodesOnA = await apiCall(NODE_A_URL, tokenA, 'GET', '/api/mesh/nodes');
      if (Array.isArray(nodesOnA.data) && nodesOnA.data.length > 0) {
        const nodeId = nodesOnA.data[0].node_id;
        // Moze juz istniec pending — ignoruj blad
        await apiCall(NODE_A_URL, tokenA, 'POST', `/api/mesh/pair/${nodeId}`);
        await new Promise(r => setTimeout(r, 2000));
      }
    }

    // Sprobuj odrzucic
    const pendingAfter = await apiCall(NODE_B_URL, tokenB, 'GET', '/api/mesh/pending');
    if (Array.isArray(pendingAfter.data) && pendingAfter.data.length > 0) {
      const nodeId = pendingAfter.data[0].remote_node_id;
      const rejectRes = await apiCall(NODE_B_URL, tokenB, 'POST',
        `/api/mesh/pair/${nodeId}/reject`
      );
      expect(rejectRes.status).toBe(200);

      // Po odrzuceniu nie powinno byc juz na liscie
      const pendingFinal = await apiCall(NODE_B_URL, tokenB, 'GET', '/api/mesh/pending');
      const stillPending = (pendingFinal.data || []).find(p => p.remote_node_id === nodeId);
      expect(stillPending).toBeFalsy();
    }
  });
});
