// =============================================================================
// File: modules/tentabus/payload.test.js
// Description: Reading a record's bytes for the screen: UTF-8 as text with a
// cut marker, anything else as hex, headers by key, and the attached-file
// reference recognised only in its exact shape.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { bytesToPreviewText, findHeader, headerText, parseBlobRefJson } from './payload.js';

const enc = (s) => new TextEncoder().encode(s);

test('valid UTF-8 reads as text, cut with an ellipsis past the limit', () => {
  assert.equal(bytesToPreviewText(enc('{"ok":true}')), '{"ok":true}');
  assert.equal(bytesToPreviewText(enc('MSH|abcdef'), 4), 'MSH|…');
  assert.equal(bytesToPreviewText(new Uint8Array(0)), '');
});

test('anything else reads as a hex dump', () => {
  assert.equal(bytesToPreviewText(new Uint8Array([0xff, 0xfe, 0x00, 0x01])), 'ff fe 00 01');
});

test('headers are found by key and decoded', () => {
  const headers = [{ key: 'dlq.reason', value: enc('consumer_error') }];
  assert.equal(findHeader(headers, 'dlq.reason').key, 'dlq.reason');
  assert.equal(findHeader(headers, 'missing'), null);
  assert.equal(headerText(headers, 'dlq.reason'), 'consumer_error');
  assert.equal(headerText(headers, 'missing'), null);
});

test('an attached-file reference is recognised only in its exact shape', () => {
  const blobRef = { id: 'blob-1', size_bytes: 2048, mime: 'application/dicom', sha256: 'abc123' };
  assert.deepEqual(parseBlobRefJson(enc(JSON.stringify(blobRef))), blobRef);
  assert.equal(parseBlobRefJson(enc(JSON.stringify({ hello: 'world' }))), null);
  assert.equal(parseBlobRefJson(new Uint8Array([0xff, 0x00])), null);
});
