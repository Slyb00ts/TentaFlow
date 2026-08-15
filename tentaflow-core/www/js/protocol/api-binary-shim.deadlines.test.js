// =============================================================================
// File: protocol/api-binary-shim.deadlines.test.js
// Description: What the shim promises about SETTLING. A file write issued while
//       the server was disappearing never resolved and never rejected: the
//       client's timer starts after the frame is encoded, so a transport stuck
//       in `connect` left the page waiting forever, while a request that got as
//       far as the socket timed out normally. The two paths are pinned here as
//       one rule — every call through the shim ends — together with the two
//       failures that used to arrive as a bare TypeError from `encode[kind]`.
//       The modules pull the wasm codec in at import time, so the functions
//       under test are cut out of the real files and evaluated: the code tested
//       here is the code that ships.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const shimSource = readFileSync(join(here, 'api-binary-shim.js'), 'utf8');
const transportSource = readFileSync(join(here, 'transport.js'), 'utf8');
const clientSource = readFileSync(join(here, 'binary-ws-client.js'), 'utf8');
const codecSource = readFileSync(join(here, 'codec.js'), 'utf8');

/** Source of one balanced `{...}` block starting at `start`. */
function cutBalanced(src, start) {
  let depth = 0;
  let i = src.indexOf('{', start);
  for (; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) break;
    }
  }
  return src.slice(start, i + 1);
}

function cutFn(src, name) {
  const start = src.search(new RegExp(`^(async )?function ${name}\\(`, 'm'));
  if (start < 0) throw new Error(`no definition: ${name}`);
  return cutBalanced(src, start);
}

function cutMethod(src, name) {
  const start = src.search(new RegExp(`^  ${name}\\(`, 'm'));
  if (start < 0) throw new Error(`no method: ${name}`);
  return cutBalanced(src, start);
}

function cutConst(src, name) {
  const line = src.match(new RegExp(`^const ${name} = .*$`, 'm'));
  if (!line) throw new Error(`no constant: ${name}`);
  return line[0];
}

/** `dispatch` + its helpers, with `getClient` supplied by the test. */
function loadShim(getClient) {
  const body = [
    cutConst(shimSource, 'CALL_DEADLINE_MS'),
    cutFn(shimSource, 'withDeadline'),
    cutFn(shimSource, 'requestedTimeout'),
    cutFn(shimSource, 'dispatch'),
    'return { CALL_DEADLINE_MS, withDeadline, requestedTimeout, dispatch };',
  ].join('\n');
  return new Function('getClient', body)(getClient);
}

const never = () => new Promise(() => {});

test('a call whose transport never connects rejects instead of hanging', async () => {
  const { dispatch } = loadShim(never);
  const started = Date.now();
  await assert.rejects(
    dispatch('codeStudioFileWriteRequest', { path: 'a.txt' }, {
      _isRequestOptions: true,
      timeoutMs: 60,
    }),
    (err) => {
      assert.match(err.message, /codeStudioFileWriteRequest timed out after 60ms/);
      return true;
    },
  );
  assert.ok(Date.now() - started < 5000, 'the deadline did not fire on its own schedule');
});

test('a call whose answer never arrives rejects on the same deadline', async () => {
  const { dispatch } = loadShim(async () => ({ request: never }));
  await assert.rejects(
    dispatch('codeStudioExecOutputRequest', { execId: 'op-1' }, {
      _isRequestOptions: true,
      timeoutMs: 60,
    }),
    /codeStudioExecOutputRequest timed out after 60ms/,
  );
});

test('the deadline is the default when the caller names none', async () => {
  const { dispatch, requestedTimeout, CALL_DEADLINE_MS } = loadShim(never);
  assert.equal(CALL_DEADLINE_MS, 30_000);
  assert.equal(requestedTimeout([{ path: 'a.txt' }]), CALL_DEADLINE_MS);
  assert.equal(
    requestedTimeout([{}, { _isRequestOptions: true, timeoutMs: 5 }]),
    5,
    'a caller-supplied timeout was ignored',
  );
  // Reading the options must not consume them — the client pops the same
  // argument for its own timer.
  const args = [{}, { _isRequestOptions: true, timeoutMs: 5 }];
  requestedTimeout(args);
  assert.equal(args.length, 2);
  // And a successful call still answers with the body.
  const ok = loadShim(async () => ({
    request: async () => ({ envelope: { isError: false }, body: { variant: 'Ok', lines: ['a'] } }),
  }));
  assert.deepEqual(await ok.dispatch('codeStudioExecOutputRequest', {}), {
    variant: 'Ok',
    lines: ['a'],
  });
});

test('a websocket that neither opens nor fails is given up on', async () => {
  const sockets = [];
  class StalledSocket {
    constructor() {
      this.readyState = 0;
      this.closed = null;
      sockets.push(this);
    }
    close(code, reason) {
      this.closed = { code, reason };
    }
  }
  const openWebSocket = new Function(
    'window',
    'WebSocket',
    `${cutFn(transportSource, 'openWebSocket')}\nreturn openWebSocket;`,
  )({ location: { host: 'localhost:8090' } }, StalledSocket);

  await assert.rejects(
    openWebSocket('https://localhost:8090', null, 40),
    /WebSocket connect timed out after 40ms/,
  );
  assert.equal(sockets.length, 1);
  assert.deepEqual(sockets[0].closed, { code: 1000, reason: 'connect timeout' });
});

test('a request kind the codec does not know is named, not thrown at', async () => {
  const request = new Function(
    'encode',
    `return ({ ${cutMethod(clientSource, 'request')} });`,
  )({ codeStudioExecStartRequest: () => new Uint8Array() }).request;

  await assert.rejects(
    request.call({}, 'codeStudioNoSuchRequest', {}),
    (err) => {
      assert.match(err.message, /unknown request kind 'codeStudioNoSuchRequest'/);
      assert.doesNotMatch(err.message, /apply/, 'the raw TypeError reached the caller');
      return true;
    },
  );
});

test('the codec can encode a request for the output of a command', () => {
  // The kind that raised "apply was called on undefined": the shim had no
  // encoder for it because the request did not exist. It does now, and it
  // carries the line cursor the caller pages with.
  const at = codecSource.search(/^ {2}codeStudioExecOutputRequest\(correlationId/m);
  assert.ok(at >= 0, 'the codec has no encoder for the command-output request');
  // The signature carries `payload = {}`, so the body starts at the brace of
  // `) {` rather than at the first brace after the name.
  const encoder = cutBalanced(codecSource, codecSource.indexOf(') {', at));
  assert.match(encoder, /exec_id:/);
  assert.match(encoder, /after_seq:/);
  assert.match(encoder, /limit:/);
  assert.match(encoder, /_wasm\.encodeCodeStudioExecOutputRequest/);
});
