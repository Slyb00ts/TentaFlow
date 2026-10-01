// ============ File: project-attachment-worker.test.js — Native byte ranges and download backpressure over MessageChannel ============

import test, { afterEach } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { MessageChannel } from 'node:worker_threads';
import { createHash } from 'node:crypto';

const source = readFileSync(new URL('./project-attachment-worker.js', import.meta.url), 'utf8');
const TOKEN = '11111111-2222-4333-8444-555555555555';
const ports = [];
afterEach(() => { while (ports.length) ports.pop().close(); });

function worker(totalSize, { failAt = null, live = true } = {}) {
  const requests = [];
  const notices = [];
  const bytesFor = (offset, length) => Uint8Array.from({ length }, (_, index) => (offset + index) % 251);
  const client = { id: 'page', postMessage(data, transferred) {
    const port = transferred[0]; ports.push(port);
    assert.equal(data.token, TOKEN);
    port.on('message', (request) => {
      if (request.type !== 'read') { notices.push(request); return; }
      requests.push(request);
      if (failAt !== null && request.offset >= failAt) { port.postMessage({ requestId: request.requestId, error: 'Permission was revoked' }); return; }
      const bytes = bytesFor(request.offset, Math.min(request.maxBytes, totalSize - request.offset));
      port.postMessage({ requestId: request.requestId, bytes, eof: request.offset + bytes.length === totalSize }, [bytes.buffer]);
    });
    port.postMessage({ totalSize, mime: 'video/mp4', filename: 'Original recording.avi' });
  } };
  const self = { clients: { get: async () => live ? client : null, matchAll: async () => live ? [client] : [] } };
  const Channel = class extends MessageChannel { constructor() { super(); ports.push(this.port1, this.port2); } };
  runInNewContext(source, { self, MessageChannel: Channel, URL, Headers, Response, ReadableStream, Uint8Array, setTimeout, clearTimeout });
  const response = (range = null, download = false, method = 'GET') => self.ProjectAttachmentWorker.respond({ clientId: 'page', request: new Request(`http://localhost/__project_attachment/${TOKEN}/recording.avi${download ? '?download=1' : ''}`, { method, ...(range ? { headers: { Range: range } } : {}) }) });
  return { response, requests, notices, bytesFor, byteRange: self.ProjectAttachmentWorker.byteRange };
}

test('native middle and suffix seeks return exactly the selected range', async () => {
  const w = worker(9000000);
  const response = await w.response('bytes=4000000-4000031');
  assert.equal(response.status, 206);
  assert.equal(response.headers.get('Content-Range'), 'bytes 4000000-4000031/9000000');
  assert.equal(response.headers.get('Content-Length'), '32');
  assert.equal(w.requests.length, 0, 'no data is fetched before the native consumer requests it');
  assert.deepEqual(new Uint8Array(await response.arrayBuffer()), w.bytesFor(4000000, 32));
  const suffix = await w.response('bytes=-15');
  assert.deepEqual(new Uint8Array(await suffix.arrayBuffer()), w.bytesFor(8999985, 15));
  assert.ok(w.requests.every((request) => request.maxBytes <= 512 * 1024));
});

test('download over 64 MiB is streamed with one bounded chunk per pull and a complete hash', async () => {
  const size = 70 * 1024 * 1024 + 17;
  const w = worker(size);
  const response = await w.response(null, true);
  assert.equal(response.status, 200);
  assert.match(response.headers.get('Content-Disposition'), /attachment; filename\*=UTF-8''Original%20recording.avi/);
  assert.equal(w.requests.length, 0);
  const reader = response.body.getReader();
  const hash = createHash('sha256');
  let total = 0;
  let peak = 0;
  for (;;) {
    const chunk = await reader.read();
    if (chunk.done) break;
    hash.update(chunk.value); total += chunk.value.length; peak = Math.max(peak, chunk.value.length);
    assert.equal(w.requests.length, Math.ceil(total / (512 * 1024)), 'backpressure prevents speculative future chunks');
  }
  const expected = createHash('sha256');
  for (let start = 0; start < size; start += 1024 * 1024) expected.update(w.bytesFor(start, Math.min(1024 * 1024, size - start)));
  assert.equal(total, size);
  assert.equal(peak, 512 * 1024);
  assert.equal(hash.digest('hex'), expected.digest('hex'));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(w.notices.at(-1).type, 'download-complete');
});

test('invalid or multiple ranges get 416, HEAD gets metadata and no data', async () => {
  const w = worker(20);
  for (const range of ['bytes=20-', 'bytes=5-3', 'bytes=0-1,4-5', 'bytes=-0', 'bytes=9007199254740992-']) {
    const response = await w.response(range);
    assert.equal(response.status, 416);
    assert.equal(response.headers.get('Content-Range'), 'bytes */20');
  }
  const head = await w.response('bytes=2-5', false, 'HEAD');
  assert.equal(head.status, 206);
  assert.equal(head.body, null);
  assert.equal(w.requests.length, 0);
});

test('cancel closes a download channel and authorization failure errors the native stream', async () => {
  const w = worker(5 * 1024 * 1024);
  const response = await w.response(null, true);
  const reader = response.body.getReader();
  await reader.read();
  await reader.cancel();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(w.requests.length, 1);
  assert.equal(w.notices.at(-1).type, 'download-canceled');
  const denied = worker(5 * 1024 * 1024, { failAt: 512 * 1024 });
  const deniedReader = (await denied.response(null, true)).body.getReader();
  await deniedReader.read();
  await assert.rejects(deniedReader.read(), /Permission was revoked/);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(denied.notices.at(-1).type, 'download-error');
});

test('a worker restart reconnects the owning page while a closed page gives a clear 410', async () => {
  const first = worker(100);
  assert.equal((await first.response(null, false, 'HEAD')).status, 200);
  const restarted = worker(100);
  const seek = await restarted.response('bytes=90-99');
  assert.deepEqual(new Uint8Array(await seek.arrayBuffer()), restarted.bytesFor(90, 10));
  const unavailable = worker(100, { live: false });
  assert.equal((await unavailable.response()).status, 410);
  assert.equal(unavailable.requests.length, 0);
});
