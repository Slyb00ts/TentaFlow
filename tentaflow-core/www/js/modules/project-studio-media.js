// ============ File: project-studio-media.js — Durable binary uploads and bounded native attachment streams ============

import { ApiBinary } from '/js/protocol/api-binary-shim.js';

// CBOR metadata shares the transport's 1 MiB frame budget with the file bytes.
export const ATTACHMENT_CHUNK_BYTES = 512 * 1024;
const STORAGE_KEY = 'ps.attachment.uploads';
const sessions = new Map();
let workerListener = false;

function uploadRecords() {
  const json = localStorage.getItem(STORAGE_KEY);
  return json ? JSON.parse(json) : [];
}

function storeUpload(record) {
  const rows = uploadRecords().filter((row) => row.uploadId !== record.uploadId);
  rows.push(record);
  localStorage.setItem(STORAGE_KEY, JSON.stringify(rows));
}

function removeUpload(uploadId) {
  localStorage.setItem(STORAGE_KEY, JSON.stringify(uploadRecords().filter((row) => row.uploadId !== uploadId)));
}

export function pendingAttachmentUploads(projectId, userId) {
  return uploadRecords().filter((row) => row.projectId === projectId && row.userId === userId);
}

function checkAborted(signal) {
  if (signal?.aborted) throw new DOMException('Upload paused', 'AbortError');
}

export async function hashAttachmentFile(file, { signal, onProgress = () => {}, createHasher } = {}) {
  const make = createHasher || (() => import('/js/protocol/codec.js').then(async ({ codecReady }) => {
    const wasm = await codecReady;
    return new wasm.ProjectStudioSha256();
  }));
  const hasher = await make();
  try {
    for (let offset = 0; offset < file.size; offset += ATTACHMENT_CHUNK_BYTES) {
      checkAborted(signal);
      const bytes = new Uint8Array(await file.slice(offset, Math.min(offset + ATTACHMENT_CHUNK_BYTES, file.size)).arrayBuffer());
      checkAborted(signal);
      hasher.update(bytes);
      onProgress({ phase: 'hashing', offset: Math.min(offset + bytes.length, file.size), total: file.size });
    }
    return hasher.digestHex();
  } finally { hasher.free(); }
}

export async function uploadProjectAttachment(file, { projectId, userId, signal, onProgress = () => {}, createHasher, resumeUploadId = null } = {}) {
  const sha256 = await hashAttachmentFile(file, { signal, onProgress, createHasher });
  checkAborted(signal);
  const records = pendingAttachmentUploads(projectId, userId);
  let record = resumeUploadId ? records.find((row) => row.uploadId === resumeUploadId) : records.find((row) => row.sha256 === sha256 && row.totalSize === file.size && row.filename === file.name);
  if (resumeUploadId && (!record || record.sha256 !== sha256 || record.totalSize !== file.size || record.filename !== file.name)) throw new Error('attachment_resume_file_mismatch');
  const resume = !!record;
  if (!record) {
    record = { projectId, userId, uploadId: crypto.randomUUID(), filename: file.name, mime: file.type || 'application/octet-stream', sha256, totalSize: file.size, nextOffset: 0, complete: false, expiresAt: null };
    storeUpload(record);
  }
  onProgress({ phase: 'uploading', offset: record.nextOffset, total: file.size, uploadId: record.uploadId });
  if (resume) {
    const status = await ApiBinary.one('projectStudioAttachmentUploadStatusRequest', { projectId, uploadId: record.uploadId });
    if (status.sha256 !== sha256 || Number(status.total_size) !== file.size || status.filename !== file.name || status.mime !== record.mime) throw new Error('Upload metadata differs from the selected file');
    record.nextOffset = Number(status.next_offset); record.complete = status.complete; record.expiresAt = status.expires_at;
    storeUpload(record);
  }
  while (!record.complete) {
    checkAborted(signal);
    const offset = record.nextOffset;
    if (!Number.isSafeInteger(offset) || offset < 0 || offset > file.size) throw new Error('Server upload offset is outside the selected file');
    const bytes = new Uint8Array(await file.slice(offset, Math.min(offset + ATTACHMENT_CHUNK_BYTES, file.size)).arrayBuffer());
    checkAborted(signal);
    const status = await ApiBinary.one('projectStudioAttachmentUploadChunkRequest', { projectId, uploadId: record.uploadId, filename: record.filename, mime: record.mime, sha256, totalSize: file.size, offset, bytes });
    const next = Number(status.next_offset);
    if (status.sha256 !== sha256 || Number(status.total_size) !== file.size || status.filename !== record.filename || !Number.isSafeInteger(next) || next !== offset + bytes.length || !status.complete && next === offset) throw new Error('Server returned an invalid upload acknowledgement');
    record.nextOffset = next; record.complete = status.complete; record.expiresAt = status.expires_at;
    if (record.complete && next !== file.size) throw new Error('Server completed an incomplete upload');
    storeUpload(record);
    onProgress({ phase: record.complete ? 'complete' : 'uploading', offset: next, total: file.size, uploadId: record.uploadId });
  }
  // The completed record remains recoverable until the owning resource save succeeds.
  return { sha256, name: record.filename, size_bytes: record.totalSize, mime: record.mime };
}

export function acknowledgeAttachmentUploads(projectId, userId, attachments) {
  const hashes = new Set(attachments.map((attachment) => attachment.sha256));
  for (const record of pendingAttachmentUploads(projectId, userId)) if (record.complete && hashes.has(record.sha256)) removeUpload(record.uploadId);
}

export async function cancelAttachmentUpload(projectId, uploadId) {
  await ApiBinary.one('projectStudioAttachmentUploadCancelRequest', { projectId, uploadId });
  removeUpload(uploadId);
}

export async function refreshAttachmentUpload(record) {
  const status = await ApiBinary.one('projectStudioAttachmentUploadStatusRequest', { projectId: record.projectId, uploadId: record.uploadId });
  const updated = { ...record, nextOffset: Number(status.next_offset), complete: status.complete, expiresAt: status.expires_at };
  storeUpload(updated);
  return updated;
}

export async function readAttachmentChunk(owner, attachment, offset, maxBytes = ATTACHMENT_CHUNK_BYTES, preview = false) {
  const response = await ApiBinary.one('projectStudioAttachmentGetRequest', { ...owner, sha256: attachment.sha256, offset, maxBytes, preview });
  const bytes = response.bytes instanceof Uint8Array ? response.bytes : new Uint8Array(response.bytes);
  return { ...response, bytes, total_size: Number(response.total_size) };
}

function installWorkerListener() {
  if (workerListener) return;
  workerListener = true;
  navigator.serviceWorker.addEventListener('message', (event) => {
    if (event.data?.type !== 'project-attachment-connect' || !event.ports[0]) return;
    const port = event.ports[0];
    const session = sessions.get(event.data.token);
    if (!session) { port.postMessage({ error: 'Attachment session is closed' }); port.close(); return; }
    session.ports.add(port);
    port.onmessage = async ({ data }) => {
      if (data.type === 'close') { session.ports.delete(port); port.close(); return; }
      if (['download-complete', 'download-canceled', 'download-error'].includes(data.type)) {
        sessions.delete(event.data.token);
        for (const channel of session.ports) channel.close();
        session.ports.clear();
        if (data.type === 'download-complete') session.onComplete?.();
        else if (data.type === 'download-canceled') session.onCancel?.();
        else session.onError?.(new Error(data.error));
        return;
      }
      if (data.type !== 'read') return;
      try {
        if (!sessions.has(event.data.token)) throw new Error('Attachment session is closed');
        if (!Number.isSafeInteger(data.offset) || data.offset < 0 || !Number.isSafeInteger(data.maxBytes) || data.maxBytes < 1 || data.maxBytes > ATTACHMENT_CHUNK_BYTES) throw new Error('Invalid attachment byte range');
        const response = await readAttachmentChunk(session.owner, session.attachment, data.offset, data.maxBytes, session.preview);
        if (response.total_size !== session.totalSize || response.bytes.length > data.maxBytes || data.offset + response.bytes.length > session.totalSize) throw new Error('Attachment bytes do not match the stream metadata');
        const bytes = response.bytes.byteOffset === 0 && response.bytes.byteLength === response.bytes.buffer.byteLength ? response.bytes : response.bytes.slice();
        const received = bytes.length;
        port.postMessage({ requestId: data.requestId, bytes, eof: response.eof }, [bytes.buffer]);
        session.onProgress?.({ offset: data.offset + received, total: session.totalSize });
      } catch (error) { port.postMessage({ requestId: data.requestId, error: error.message }); }
    };
    port.start();
    port.postMessage({ totalSize: session.totalSize, mime: session.mime, filename: session.filename });
  });
  window.addEventListener('pagehide', () => {
    for (const session of sessions.values()) for (const port of session.ports) { port.postMessage({ type: 'revoked' }); port.close(); }
    sessions.clear();
  });
}

export async function openAttachmentStream(owner, attachment, { preview = false } = {}) {
  if (!navigator.serviceWorker?.controller) throw new Error('attachment_stream_reload');
  installWorkerListener();
  const first = await readAttachmentChunk(owner, attachment, 0, 1, preview);
  if (!Number.isSafeInteger(first.total_size) || first.total_size < 0 || first.bytes.length !== Math.min(1, first.total_size)) throw new Error('Invalid attachment size');
  const token = crypto.randomUUID();
  const session = { owner: { ...owner }, attachment, preview, totalSize: first.total_size, mime: first.mime, filename: first.filename, ports: new Set() };
  sessions.set(token, session);
  const url = new URL(`/__project_attachment/${token}/${encodeURIComponent(first.filename || attachment.name)}`, location.origin);
  return { url: url.href, totalSize: first.total_size, mime: first.mime, filename: first.filename,
    set onComplete(callback) { session.onComplete = callback; },
    set onProgress(callback) { session.onProgress = callback; },
    set onError(callback) { session.onError = callback; },
    set onCancel(callback) { session.onCancel = callback; },
    close() {
      sessions.delete(token);
      for (const port of session.ports) { port.postMessage({ type: 'revoked' }); port.close(); }
      session.ports.clear();
    },
  };
}

export async function downloadProjectAttachment(owner, attachment) {
  const stream = await openAttachmentStream(owner, attachment);
  const anchor = document.createElement('a');
  anchor.href = `${stream.url}?download=1`;
  // Content-Disposition starts the download through the controlled Service Worker.
  document.body.appendChild(anchor); anchor.click(); anchor.remove();
  // A download has its own lifetime; revoking the card's playback must not truncate it.
  return stream;
}
