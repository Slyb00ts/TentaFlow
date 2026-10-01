// ============ File: protocol/project-task-wire.test.js — P1 task and media CBOR contract ============

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';

const artifact = new URL('./wasm_glue_bg.wasm', import.meta.url);
const skip = existsSync(artifact) ? false : 'Build tentaflow-core to generate the Wasm codec';
const wasm = skip ? null : await import('./wasm_glue.js');
let codec;
if (!skip) {
  await wasm.default({ module_or_path: readFileSync(artifact) });
  codec = await import('./codec.js');
  await codec.codecReady;
}

function request(name, payload) {
  const envelope = wasm.decodeEnvelope(codec.encode[name](17, payload, 1));
  try {
    return wasm.decodeMessageBody(envelope.body);
  } finally {
    envelope.free();
  }
}

test('task writes carry key-related fields and canonical mention IDs', { skip }, () => {
  const saved = request('projectStudioTaskSaveRequest', {
    projectId: 'p1', taskType: 'subtask', title: 'Child', priority: 'medium', status: 'todo',
    parentTaskId: 'parent',
  });
  assert.equal(saved.variant, 'ProjectStudioTaskSaveRequest');
  assert.equal(saved.parentTaskId, 'parent');
  const comment = request('projectStudioTaskCommentAddRequest', {
    projectId: 'p1', taskId: 't1', bodyMd: 'Review this', mentionUserIds: ['u2', 'u3'],
  });
  assert.deepEqual(comment.mentionUserIds, ['u2', 'u3']);
  const archive = request('projectStudioTaskArchiveRequest', { projectId: 'p1', taskId: 't1', archived: true });
  assert.equal(archive.archived, true);
  const list = request('projectStudioTasksListRequest', { projectId: 'p1', includeArchived: true });
  assert.equal(list.includeArchived, true);
  const handover = request('projectStudioTaskHandoverRequest', {
    projectId: 'p1', taskId: 't1', assignedTo: 'u2', noteMd: 'Current context', mentionUserIds: ['u2'],
  });
  assert.equal(handover.assignedTo, 'u2');
  assert.deepEqual(handover.mentionUserIds, ['u2']);
});

test('task catalogue, history cursor and dependency preserve typed fields', { skip }, () => {
  const type = request('projectStudioTaskTypeSaveRequest', {
    projectId: 'p1', typeId: 'incident', name: 'Incident', description: 'Response', sortOrder: 75, active: true,
  });
  assert.equal(type.typeId, 'incident');
  assert.equal(type.sortOrder, 75);
  const events = request('projectStudioTaskEventsRequest', { projectId: 'p1', taskId: 't1', beforeId: 123, limit: 30 });
  assert.equal(events.beforeId, 123);
  const link = request('projectStudioTaskLinkSaveRequest', {
    projectId: 'p1', sourceTaskId: 't1', targetTaskId: 't2', kind: 'fs', lagDays: 2,
  });
  assert.equal(link.kind, 'fs');
  assert.equal(link.lagDays, 2);
});

test('media requests keep offsets and raw bytes without a whole-file array', { skip }, () => {
  const chunk = request('projectStudioAttachmentUploadChunkRequest', {
    projectId: 'p1', uploadId: 'up1', filename: 'clip.mp4', mime: 'video/mp4',
    sha256: 'a'.repeat(64), totalSize: 100_000_000, offset: 4_194_304,
    bytes: new Uint8Array([0, 1, 2, 255]),
  });
  assert.equal(chunk.offset, 4_194_304);
  assert.deepEqual([...chunk.bytes], [0, 1, 2, 255]);
  const read = request('projectStudioAttachmentGetRequest', {
    projectId: 'p1', ownerKind: 'task', ownerId: 't1', sha256: 'a'.repeat(64), offset: 4_194_304,
    maxBytes: 4_194_304, preview: true,
  });
  assert.equal(read.ownerKind, 'task');
  assert.equal(read.ownerId, 't1');
  assert.equal(read.offset, 4_194_304);
  assert.equal(read.preview, true);
  const usage = request('projectStudioAttachmentUsageRequest', { projectId: 'p1', offset: 10, limit: 25 });
  assert.equal(usage.offset, 10);
  assert.equal(usage.limit, 25);
});

test('incremental WASM SHA-256 matches a known vector', { skip }, () => {
  const hasher = new wasm.ProjectStudioSha256();
  try {
    hasher.update(new TextEncoder().encode('ab'));
    hasher.update(new TextEncoder().encode('c'));
    assert.equal(hasher.digestHex(), 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  } finally {
    hasher.free();
  }
});
