// =============================================================================
// File: protocol/binary-ws-client.reconnect.test.js
// Description: What the client does when the node disappears for a long time.
//       Three failures kept a tab from ever coming back: a token that expired
//       during the outage made every upgrade fail with a 401 the browser does
//       not report, a socket that went half-open (sleep, network change) never
//       fired `close`, and a node that accepted sockets but failed the
//       handshake reset the backoff to one second. The socket is a scripted
//       fake; the handshake is replaced because it needs a live node.
// =============================================================================

import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const wasmUrl = new URL('./wasm_glue_bg.wasm', import.meta.url);
const skip = existsSync(wasmUrl) ? false : 'wasm_glue_bg.wasm is absent — build tentaflow-core once to generate the glue';

let BinaryWsClient = null;
if (!skip) {
  const wasm = await import('./wasm_glue.js');
  await wasm.default({ module_or_path: readFileSync(wasmUrl) });
  await (await import('./codec.js')).codecReady;
  ({ BinaryWsClient } = await import('./binary-ws-client.js'));
}

const opened = [];

class FakeSocket {
  static OPEN = 1;
  constructor(url, protocols) {
    this.url = url;
    this.protocols = protocols ?? [];
    this.readyState = 0;
    opened.push(this);
    queueMicrotask(() => {
      if (FakeSocket.refuse) {
        this.onerror?.({});
      } else {
        this.readyState = 1;
        this.onopen?.({});
      }
    });
  }
  send() {}
  close() {
    this.readyState = 3;
    this.closed = true;
  }
}

function jwt(expSeconds) {
  const part = (o) => Buffer.from(JSON.stringify(o)).toString('base64url');
  return `${part({ alg: 'HS256' })}.${part({ exp: expSeconds })}.sig`;
}

function install() {
  opened.length = 0;
  FakeSocket.refuse = false;
  globalThis.WebSocket = FakeSocket;
  globalThis.window = {
    location: { origin: 'https://node.test', host: 'node.test', protocol: 'https:' },
    addEventListener() {},
    removeEventListener() {},
  };
  globalThis.document = { hidden: false, querySelectorAll: () => [], addEventListener() {}, removeEventListener() {} };
  globalThis.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
}

function client(opts) {
  const c = new BinaryWsClient('wss://node.test/ws/api', { heartbeatIntervalMs: 0, ...opts });
  c._handshake = async () => {};
  return c;
}

// The page-level shim starts retrying once the codec is ready; it is closed so the run ends.
after(async () => {
  if (skip) return;
  (await (await import('./api-binary-shim.js')).initTransport())?.close();
});

test('an expired token is dropped before the upgrade is attempted', { skip }, async () => {
  install();
  let rejected = null;
  const c = client({ jwtToken: jwt(1), onAuthRejected: (info) => { rejected = info; } });
  await c.connect();
  assert.equal(rejected?.reason, 'expired');
  assert.equal(c.jwtToken, null);
  assert.deepEqual(opened[0].protocols, []);
  c.close();
});

test('a valid token is sent as the bearer subprotocol', { skip }, async () => {
  install();
  const token = jwt(Math.floor(Date.now() / 1000) + 3600);
  const c = client({ jwtToken: token });
  await c.connect();
  assert.deepEqual(opened[0].protocols, [`bearer.${token}`]);
  c.close();
});

test('a silent socket is replaced instead of trusted', { skip }, async () => {
  install();
  const c = client({ heartbeatIntervalMs: 1000 });
  await c.connect();
  let disconnected = null;
  c.onDisconnected = (info) => { disconnected = info; };
  c._lastInboundAt = Date.now() - 60_000;
  c._checkAlive();
  assert.equal(c.connected, false);
  assert.equal(disconnected?.reason, 'heartbeat timeout');
  assert.ok(c._reconnectTimer, 'a reconnect is scheduled');
  c.close();
});

test('a socket that still hears the node is left alone', { skip }, async () => {
  install();
  const c = client({ heartbeatIntervalMs: 1000 });
  await c.connect();
  c._lastInboundAt = Date.now() - 500;
  c._checkAlive();
  assert.equal(c.connected, true);
  c.close();
});

test('a failed handshake does not reset the backoff', { skip }, async () => {
  install();
  const c = client();
  c._handshake = async () => { throw new Error('handshake timeout'); };
  c.reconnectAttempt = 4;
  await assert.rejects(c.connect(), /handshake timeout/);
  assert.equal(c.reconnectAttempt, 5);
  c.close();
});

test('coming back online restarts a waiting client at once', { skip }, async () => {
  install();
  const c = client();
  c.connected = false;
  c.reconnectAttempt = 6;
  let restarted = 0;
  c.reconnectNow = () => { restarted += 1; };
  c._wake({ type: 'online' });
  assert.equal(restarted, 1);
  c.close();
});
