// =============================================================================
// File: wasm-fallback.test.js — the wasm2js twin must decode like the wasm
// =============================================================================
// Runs the real protocol glue with the WebAssembly global removed, which is the
// state of a WKWebView under iOS Lockdown Mode. The twin is a build artefact
// (needs binaryen's wasm2js), so a tree without it skips and names the reason.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { initWithFallback } from './wasm-fallback.js';

const glueUrl = new URL('./protocol/wasm_glue.js', import.meta.url);
const twinUrl = new URL('./protocol/wasm_glue_asm.js', import.meta.url);
const skip = existsSync(fileURLToPath(twinUrl))
  ? false
  : `${fileURLToPath(twinUrl)} is absent — build tentaflow-core with wasm2js installed`;

test('glue initialises and decodes without a WebAssembly global', { skip }, async () => {
  const native = globalThis.WebAssembly;
  delete globalThis.WebAssembly;
  try {
    const glue = await import(glueUrl.href);
    await initWithFallback(glue.default, twinUrl);
    assert.equal(typeof globalThis.WebAssembly, 'undefined', 'the shim must not leak past init');
    const ping = Uint8Array.of(0xa1, 0x64, 0x50, 0x69, 0x6e, 0x67, 0xf6);
    assert.deepEqual(JSON.parse(JSON.stringify(glue.decodeMessageBody(ping))), { _variant: 'Ping', value: null });
    // u64 above 2^53 crosses the i64 boundary and must arrive as BigInt.
    const big = Uint8Array.of(0xa1, 0x64, 0x50, 0x69, 0x6e, 0x67, 0x1b, ...new Array(8).fill(0xff));
    assert.equal(glue.decodeMessageBody(big).value, 18446744073709551615n);
  } finally {
    globalThis.WebAssembly = native;
  }
});

test('i64 arguments and results match the native module', { skip }, async () => {
  // Query strings give each run its own glue instance, so native and twin do not
  // share module state.
  const native = await import(`${glueUrl.href}?native`);
  await native.default({ module_or_path: readFileSync(new URL('./protocol/wasm_glue_bg.wasm', import.meta.url)) });
  const body = Uint8Array.of(0xa1, 0x64, 0x50, 0x69, 0x6e, 0x67, 0xf6);
  const expected = native.encodeEnvelopeDirect(0x1234567890abcdefn, 42n, 0, body);

  const saved = globalThis.WebAssembly;
  delete globalThis.WebAssembly;
  try {
    const twin = await import(`${glueUrl.href}?twin`);
    await initWithFallback(twin.default, twinUrl);
    const got = twin.encodeEnvelopeDirect(0x1234567890abcdefn, 42n, 0, body);
    assert.deepEqual([...got], [...expected]);
    // u64 getters come back through setTempRet0.
    const view = twin.decodeEnvelope(got);
    assert.equal(view.correlation_id, 0x1234567890abcdefn);
    assert.equal(view.sequence, 42n);
  } finally {
    globalThis.WebAssembly = saved;
  }
});

test('encodeBody grows memory and reports errors exactly like the native module', { skip }, async () => {
  // Encoding allocates through the module's realloc, which grows memory: the
  // glue only notices when the old buffer is detached, as native wasm does.
  const run = async (query, fallback) => {
    const glue = await import(`${glueUrl.href}?${query}`);
    if (fallback) await initWithFallback(glue.default, twinUrl);
    else await glue.default({ module_or_path: readFileSync(new URL('./protocol/wasm_glue_bg.wasm', import.meta.url)) });
    const encoded = [...glue.encodeBody('', 'MetaSchemaVersionCheck', '{"client_version":32}')];
    let message = null;
    try {
      glue.encodeBody('', 'MetaSchemaVersionCheck', '{"client_version":32.5}');
    } catch (e) {
      message = e.message;
    }
    return { encoded, message };
  };
  const expected = await run('enc-native', false);
  const saved = globalThis.WebAssembly;
  delete globalThis.WebAssembly;
  try {
    const got = await run('enc-twin', true);
    assert.deepEqual(got, expected);
    assert.match(got.message, /expected an unsigned integer/);
  } finally {
    globalThis.WebAssembly = saved;
  }
});
