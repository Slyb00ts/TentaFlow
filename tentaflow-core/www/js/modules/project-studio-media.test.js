// ============ File: project-studio-media.test.js — Bounded hashing, durable offsets and explicitly scoped attachment reads ============

import test, { beforeEach, after } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { MessageChannel } from 'node:worker_threads';
import { window } from '../sdk-runtime/_dom-test-harness.js';
import { ApiBinary } from '../protocol/api-binary-shim.js';
import { codecReady, encode } from '../protocol/codec.js';
import { ATTACHMENT_CHUNK_BYTES, hashAttachmentFile, uploadProjectAttachment, pendingAttachmentUploads, acknowledgeAttachmentUploads, cancelAttachmentUpload, readAttachmentChunk, openAttachmentStream, downloadProjectAttachment } from './project-studio-media.js';

globalThis.localStorage = window.localStorage;
globalThis.location = window.location;
let onWorkerMessage;
const serviceWorker = { controller: {}, addEventListener: (kind, listener) => { if (kind === 'message') onWorkerMessage = listener; } };
Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { serviceWorker } });
const ports = [];
after(async () => { for (const port of ports) port.close(); await window.happyDOM.close(); });
beforeEach(() => { localStorage.clear(); serviceWorker.controller = {}; });

function boundedFile(size, { name = 'recording.avi', salt = 0 } = {}) {
  const slices = [];
  return { name, type: 'video/x-msvideo', size, slices,
    arrayBuffer() { throw new Error('Full file reads are forbidden'); },
    slice(start, end) {
      assert.ok(end - start <= ATTACHMENT_CHUNK_BYTES);
      slices.push([start, end]);
      const bytes = new Uint8Array(end - start);
      for (let i = 0; i < bytes.length; i += 1) bytes[i] = (start + i + salt) % 251;
      return { arrayBuffer: async () => bytes.buffer };
    },
  };
}

function hasherFactory(freed = []) {
  return () => {
    const hash = createHash('sha256');
    return { update: (bytes) => hash.update(bytes), digestHex: () => hash.digest('hex'), free: () => freed.push(true) };
  };
}

test('a file over 64 MiB is incrementally hashed without reading the whole file', async () => {
  const file = boundedFile(67 * 1024 * 1024 + 17);
  const expected = createHash('sha256');
  for (let start = 0; start < file.size; start += ATTACHMENT_CHUNK_BYTES) expected.update(new Uint8Array(await file.slice(start, Math.min(start + ATTACHMENT_CHUNK_BYTES, file.size)).arrayBuffer()));
  file.slices.length = 0;
  const freed = [];
  const digest = await hashAttachmentFile(file, { createHasher: hasherFactory(freed) });
  assert.equal(digest, expected.digest('hex'));
  assert.equal(file.slices.length, Math.ceil(file.size / ATTACHMENT_CHUNK_BYTES));
  assert.equal(Math.max(...file.slices.map(([start, end]) => end - start)), ATTACHMENT_CHUNK_BYTES);
  assert.equal(freed.length, 1);
});

test('the final encoded upload envelope fits the actual WebSocket frame budget including metadata', async () => {
  const server = readFileSync(new URL('../../../src/api/dashboard/ws_binary.rs', import.meta.url), 'utf8');
  const frameLimit = Number(server.match(/const MAX_FRAME_SIZE: usize = ([\d_]+);/)[1].replaceAll('_', ''));
  await codecReady;
  const payload = {
    projectId: '11111111-2222-4333-8444-555555555555', uploadId: '66666666-7777-4888-9999-000000000000',
    filename: `${'界'.repeat(255)}.avi`, mime: 'video/x-msvideo', sha256: 'a'.repeat(64),
    totalSize: Number.MAX_SAFE_INTEGER, offset: Number.MAX_SAFE_INTEGER - ATTACHMENT_CHUNK_BYTES,
    bytes: new Uint8Array(ATTACHMENT_CHUNK_BYTES),
  };
  const frame = encode.projectStudioAttachmentUploadChunkRequest(18446744073709551615n, payload, 18446744073709551615n);
  assert.ok(frame.byteLength > payload.bytes.byteLength, 'the measured frame includes CBOR fields and the envelope');
  assert.ok(frame.byteLength <= frameLimit, `${frame.byteLength} exceeds the server frame limit ${frameLimit}`);
  assert.ok(frameLimit - frame.byteLength > 500000, 'metadata retains substantial headroom');
  const previousSize = encode.projectStudioAttachmentUploadChunkRequest(1, { ...payload, bytes: new Uint8Array(frameLimit) });
  assert.ok(previousSize.byteLength > frameLimit, 'a raw 1 MiB chunk cannot fit after real encoding');
});

test('pause preserves the confirmed offset and resume obtains the server offset before writing', async () => {
  const file = boundedFile(ATTACHMENT_CHUNK_BYTES * 2 + 17);
  const requests = [];
  let status;
  ApiBinary.one = async (kind, payload) => {
    requests.push({ kind, payload });
    if (kind === 'projectStudioAttachmentUploadStatusRequest') return status;
    assert.equal(kind, 'projectStudioAttachmentUploadChunkRequest');
    assert.ok(payload.bytes.length <= ATTACHMENT_CHUNK_BYTES);
    assert.equal(payload.offset, status?.next_offset || 0);
    status = { upload_id: payload.uploadId, filename: payload.filename, mime: payload.mime, sha256: payload.sha256,
      total_size: payload.totalSize, next_offset: payload.offset + payload.bytes.length, complete: payload.offset + payload.bytes.length === payload.totalSize, expires_at: '2030-01-01T00:00:00Z' };
    return status;
  };
  const controller = new AbortController();
  await assert.rejects(uploadProjectAttachment(file, { projectId: 'project', userId: 'user', createHasher: hasherFactory(), signal: controller.signal,
    onProgress: (progress) => { if (progress.phase === 'uploading' && progress.offset === ATTACHMENT_CHUNK_BYTES) controller.abort(); },
  }), { name: 'AbortError' });
  const record = pendingAttachmentUploads('project', 'user')[0];
  assert.equal(record.nextOffset, ATTACHMENT_CHUNK_BYTES);
  assert.equal(record.complete, false);
  const start = requests.length;
  const attachment = await uploadProjectAttachment(file, { projectId: 'project', userId: 'user', resumeUploadId: record.uploadId, createHasher: hasherFactory() });
  assert.equal(requests[start].kind, 'projectStudioAttachmentUploadStatusRequest');
  assert.deepEqual(requests.slice(start + 1).map((request) => request.payload.offset), [ATTACHMENT_CHUNK_BYTES, ATTACHMENT_CHUNK_BYTES * 2]);
  assert.equal(attachment.size_bytes, file.size);
  assert.equal(pendingAttachmentUploads('project', 'user')[0].complete, true, 'a completed file is recoverable before resource save');
  assert.deepEqual(pendingAttachmentUploads('project', 'other-user'), []);
  acknowledgeAttachmentUploads('project', 'user', [attachment]);
  assert.deepEqual(pendingAttachmentUploads('project', 'user'), []);
});

test('resume rejects another file and cancellation removes metadata only after success', async () => {
  localStorage.setItem('ps.attachment.uploads', JSON.stringify([{ projectId: 'p', userId: 'u', uploadId: 'upload', filename: 'recording.avi', sha256: 'a'.repeat(64), totalSize: 30, nextOffset: 10, mime: 'video/x-msvideo' }]));
  let calls = 0;
  ApiBinary.one = async () => { calls += 1; throw new Error('Transport unavailable'); };
  await assert.rejects(uploadProjectAttachment(boundedFile(30), { projectId: 'p', userId: 'u', resumeUploadId: 'upload', createHasher: hasherFactory() }), /attachment_resume_file_mismatch/);
  assert.equal(calls, 0);
  await assert.rejects(cancelAttachmentUpload('p', 'upload'), /Transport unavailable/);
  assert.equal(pendingAttachmentUploads('p', 'u').length, 1);
  ApiBinary.one = async () => ({ ok: true });
  await cancelAttachmentUpload('p', 'upload');
  assert.equal(pendingAttachmentUploads('p', 'u').length, 0);
});

test('aborted hashing always frees its Wasm state and does not upload', async () => {
  const freed = [];
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(hashAttachmentFile(boundedFile(10), { createHasher: hasherFactory(freed), signal: controller.signal }), { name: 'AbortError' });
  assert.equal(freed.length, 1);
});

test('each chunk keeps the actual owner and step index, and readiness failure does not issue reads', async () => {
  const owner = { projectId: 'project', ownerKind: 'run_step', ownerId: 'execution', stepIndex: 4 };
  const requests = [];
  ApiBinary.one = async (kind, payload) => { requests.push({ kind, payload }); return { bytes: [1, 2], total_size: 10, mime: 'image/png', filename: 'step.png', eof: false }; };
  const result = await readAttachmentChunk(owner, { sha256: 'hash' }, 2, 2);
  assert.deepEqual(requests[0], { kind: 'projectStudioAttachmentGetRequest', payload: { ...owner, sha256: 'hash', offset: 2, maxBytes: 2, preview: false } });
  assert.deepEqual([...result.bytes], [1, 2]);
  serviceWorker.controller = null;
  await assert.rejects(openAttachmentStream(owner, { sha256: 'hash' }), /attachment_stream_reload/);
  assert.equal(requests.length, 1);
});

test('native stream channels use the registered scope and reject unbounded read requests', async () => {
  const owner = { projectId: 'project', ownerKind: 'task', ownerId: 'task', stepIndex: null };
  const requests = [];
  ApiBinary.one = async (kind, payload) => {
    requests.push({ kind, payload });
    return { bytes: new Uint8Array(Math.min(payload.maxBytes, 10 - payload.offset)), total_size: 10, mime: 'video/mp4', filename: 'movie.mp4', eof: payload.offset + payload.maxBytes >= 10 };
  };
  const stream = await openAttachmentStream(owner, { sha256: 'source-hash' }, { preview: true });
  const progress = [];
  stream.onProgress = (value) => progress.push(value);
  const token = new URL(stream.url).pathname.split('/')[2];
  const channel = new MessageChannel();
  ports.push(channel.port1, channel.port2);
  const message = () => new Promise((resolve) => { channel.port1.once('message', resolve); });
  let next = message();
  onWorkerMessage({ data: { type: 'project-attachment-connect', token }, ports: [channel.port2] });
  assert.equal((await next).filename, 'movie.mp4');
  next = message(); channel.port1.postMessage({ type: 'read', requestId: 1, offset: 3, maxBytes: 3, ownerId: 'foreign' });
  assert.equal((await next).bytes.length, 3);
  assert.deepEqual(progress, [{ offset: 6, total: 10 }], 'progress retains the length after the ArrayBuffer is transferred');
  assert.equal(requests.at(-1).payload.ownerId, 'task');
  assert.equal(requests.at(-1).payload.preview, true);
  next = message(); channel.port1.postMessage({ type: 'read', requestId: 2, offset: 0, maxBytes: ATTACHMENT_CHUNK_BYTES + 1 });
  assert.match((await next).error, /Invalid attachment byte range/);
  assert.equal(requests.length, 2);
  stream.close();
});

test('original downloads navigate through the worker and keep their own channel after playback closes', async () => {
  const owner = { projectId: 'project', ownerKind: 'task', ownerId: 'task', stepIndex: null };
  const requests = [];
  ApiBinary.one = async (kind, payload) => {
    requests.push({ kind, payload });
    return { bytes: new Uint8Array(Math.min(payload.maxBytes, 10 - payload.offset)), total_size: 10,
      mime: 'video/x-msvideo', filename: 'original.avi', eof: payload.offset + payload.maxBytes >= 10 };
  };
  const playback = await openAttachmentStream(owner, { sha256: 'source-hash' }, { preview: true });
  const clicks = [];
  const click = window.HTMLAnchorElement.prototype.click;
  window.HTMLAnchorElement.prototype.click = function () {
    clicks.push({ url: this.href, connected: this.isConnected, downloadAttribute: this.hasAttribute('download') });
  };
  let download;
  try { download = await downloadProjectAttachment(owner, { sha256: 'source-hash', name: 'original.avi' }); }
  finally { window.HTMLAnchorElement.prototype.click = click; }
  assert.equal(clicks.length, 1);
  assert.equal(clicks[0].connected, true);
  assert.equal(clicks[0].downloadAttribute, false, 'Content-Disposition must initiate the controlled worker navigation');
  assert.equal(new URL(clicks[0].url).searchParams.get('download'), '1');
  assert.notEqual(download.url, playback.url);
  assert.equal(document.querySelector(`a[href="${clicks[0].url}"]`), null);

  const token = new URL(download.url).pathname.split('/')[2];
  const channel = new MessageChannel(); ports.push(channel.port1, channel.port2);
  const message = () => new Promise((resolve) => { channel.port1.once('message', resolve); });
  let next = message();
  onWorkerMessage({ data: { type: 'project-attachment-connect', token }, ports: [channel.port2] });
  assert.equal((await next).filename, 'original.avi');
  playback.close();
  next = message(); channel.port1.postMessage({ type: 'read', requestId: 1, offset: 4, maxBytes: 6 });
  assert.equal((await next).bytes.length, 6);
  assert.equal(requests.at(-1).payload.preview, false);
  assert.equal(requests.at(-1).payload.ownerId, owner.ownerId);
  const completed = new Promise((resolve) => { download.onComplete = resolve; });
  channel.port1.postMessage({ type: 'download-complete' });
  await completed;
});
