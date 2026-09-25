// =============================================================================
// File: protocol/mesh-node-codec.test.js
// Description: The Mesh screen's node list, pinned at the codec. The registry
//       strip (device kind select + operator toggle) and the environment
//       grouping read `node_kind`, `operator` and `environment` off the decoded
//       MeshNodeInfo. When the WASM decoder dropped them, every refresh reset the
//       select to 'unknown' and the toggle to off right after a successful save.
//       The response bytes are hand-built CBOR as the server writes them, so the
//       test proves agreement with tentaflow-protocol, not with our own encoder.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));

// wasm_glue_bg.wasm is gitignored and produced by the tentaflow-core build; a
// tree that was never built skips with the reason shown in the reporter.
const wasmPath = join(here, 'wasm_glue_bg.wasm');
const skip = existsSync(wasmPath)
  ? false
  : `${wasmPath} is absent — build tentaflow-core once to generate the glue`;

const wasm = skip ? null : await import('./wasm_glue.js');
if (!skip) await wasm.default({ module_or_path: readFileSync(wasmPath) });

function head(major, value) {
  const n = BigInt(value);
  if (n < 24n) return [Number((BigInt(major) << 5n) | n)];
  const base = major << 5;
  if (n < 0x100n) return [base | 24, Number(n)];
  if (n < 0x10000n) return [base | 25, Number(n >> 8n) & 0xff, Number(n) & 0xff];
  if (n < 0x100000000n) {
    return [base | 26, Number((n >> 24n) & 0xffn), Number((n >> 16n) & 0xffn),
      Number((n >> 8n) & 0xffn), Number(n & 0xffn)];
  }
  const out = [base | 27];
  for (let shift = 56n; shift >= 0n; shift -= 8n) out.push(Number((n >> shift) & 0xffn));
  return out;
}

function enc(value) {
  if (value === null) return [0xf6];
  if (value === true) return [0xf5];
  if (value === false) return [0xf4];
  if (typeof value === 'number' || typeof value === 'bigint') {
    const n = BigInt(value);
    return n < 0n ? head(1, -n - 1n) : head(0, n);
  }
  if (typeof value === 'string') {
    const bytes = new TextEncoder().encode(value);
    return head(3, bytes.length).concat([...bytes]);
  }
  if (Array.isArray(value)) {
    return value.reduce((acc, item) => acc.concat(enc(item)), head(4, value.length));
  }
  const entries = Object.entries(value);
  return entries.reduce((acc, [k, v]) => acc.concat(enc(k), enc(v)), head(5, entries.length));
}

function nodeInfo(overrides) {
  return {
    node_id: 'node-a',
    hostname: 'rig24',
    ip: null,
    source: 'trusted',
    is_local: false,
    uptime_secs: null,
    gpus: [],
    network_interfaces: [],
    cpu_count: null,
    cpu_usage_percent: null,
    ram_total_mb: null,
    ram_used_mb: null,
    vram_total_mb: null,
    vram_used_mb: null,
    gpu_load_percent: null,
    models: [],
    containers: [],
    last_seen_epoch: null,
    route: null,
    platform: 'linux',
    connection: null,
    nsys_available: false,
    nsys_version: '',
    profiling_collectors_available: [],
    gpu_links: [],
    ...overrides,
  };
}

function decodeList(nodes) {
  const body = wasm.decodeMessageBody(Uint8Array.from(enc({
    MeshNodeListResponseBody: { nodes },
  })));
  assert.equal(body.variant, 'MeshNodeListResponse');
  return body.nodes;
}

test('node list carries the registry profile and environment mesh.js renders', { skip }, () => {
  const [node] = decodeList([nodeInfo({ node_kind: 'server', operator: true, environment: 'test' })]);
  assert.equal(node.node_kind, 'server');
  assert.equal(node.nodeKind, 'server');
  assert.equal(node.operator, true);
  assert.equal(node.environment, 'test');
});

test('an operator flag switched off arrives as false, not as a missing field', { skip }, () => {
  const [node] = decodeList([nodeInfo({ node_kind: 'phone', operator: false, environment: 'prod' })]);
  assert.equal(node.node_kind, 'phone');
  assert.equal(Object.hasOwn(node, 'operator'), true);
  assert.equal(node.operator, false);
});

test('a peer that predates the registry decodes as unknown kind, no operator, unknown env', { skip }, () => {
  const [node] = decodeList([nodeInfo({})]);
  // serde's String default; mesh.js renders an empty kind as 'unknown'.
  assert.equal(node.node_kind, '');
  assert.equal(node.operator, false);
  assert.equal(Object.hasOwn(node, 'environment'), false);
});

test('node detail carries the same registry fields', { skip }, () => {
  const body = wasm.decodeMessageBody(Uint8Array.from(enc({
    MeshNodeDetailResponseBody: { node: nodeInfo({ node_kind: 'desktop', operator: true }) },
  })));
  assert.equal(body.variant, 'MeshNodeDetailResponse');
  assert.equal(body.node.node_kind, 'desktop');
  assert.equal(body.node.operator, true);
});
