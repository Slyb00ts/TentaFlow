// =============================================================================
// File: modules/tentabus/payload.test.js
// Description: Reading a record's bytes for the screen: UTF-8 as text with a
// cut marker, anything else as hex, headers by key, and the attached-file
// reference recognised only in its exact shape.
// =============================================================================

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { bytesToPreviewText, findHeader, headerText, parseBlobRefJson, isBinaryPayload, hexRows, withoutPartialCharacter } from './payload.js';

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

test('text is not binary; invalid UTF-8 and text with control characters are', () => {
  assert.equal(isBinaryPayload(enc('{"ok":true}\n')), false);
  assert.equal(isBinaryPayload(enc('MSH|^~\\&|LIS\rPID|1')), false);
  assert.equal(isBinaryPayload(new Uint8Array(0)), false);
  assert.equal(isBinaryPayload(new Uint8Array([0xff, 0xfe])), true);
  // A Thrift record: valid UTF-8 (all bytes under 0x80) but made of control bytes.
  assert.equal(isBinaryPayload(new Uint8Array([0x0a, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07, 0x00])), true);
});

test('a hex view is rows of 16: offset, hex in two groups of eight, printable ASCII', () => {
  const bytes = new Uint8Array([...enc('Order#1'), 0x00, 0x01, 0xff, ...Array(20).fill(0x41)]);
  const rows = hexRows(bytes);
  assert.equal(rows.length, 2);
  assert.deepEqual(rows[0], { offset: '0000', hex: '4f 72 64 65 72 23 31 00  01 ff 41 41 41 41 41 41', ascii: 'Order#1...AAAAAA' });
  assert.equal(rows[1].offset, '0010');
  assert.equal(hexRows(new Uint8Array(1000), 512).length, 32, 'cut at 512 bytes');
});

test('a preview cut inside a character is text, not binary: the unfinished character is dropped only when the bytes are a cut', () => {
  // 4095 ASCII bytes and "ą" (2 bytes) from offset 4095: the preview of 4096 bytes ends in the middle of it.
  const text = new TextEncoder().encode(`${'a'.repeat(4095)}ąb`);
  const preview = text.subarray(0, 4096);
  assert.equal(isBinaryPayload(preview), true, 'whole, an unfinished character is invalid UTF-8');
  assert.equal(isBinaryPayload(preview, true), false, 'a cut preview is not');
  assert.equal(bytesToPreviewText(preview, 8192, true), `${'a'.repeat(4095)}…`);
  for (const [char, size] of [['ą', 2], ['€', 3], ['😀', 4]]) {
    const bytes = new TextEncoder().encode(`xy${char}`);
    for (let cut = 3; cut < 2 + size; cut += 1) {
      assert.equal(withoutPartialCharacter(bytes.subarray(0, cut)).length, 2, `${char} cut after ${cut - 2} byte(s)`);
    }
    assert.equal(withoutPartialCharacter(bytes).length, 2 + size, `${char} whole`);
  }
  // Binary stays binary however it is cut.
  assert.equal(isBinaryPayload(new Uint8Array([0x0a, 0x00, 0x01, 0x00, 0xc4]), true), true);
  // The text view of a long text cut by the screen keeps whole characters too.
  assert.equal(bytesToPreviewText(new TextEncoder().encode('aąb'), 2), 'a…');
});

test('a hex view can be rows of eight', () => {
  const rows = hexRows(new Uint8Array(20).fill(0x41), 512, 8);
  assert.equal(rows.length, 3);
  assert.deepEqual(rows[0], { offset: '0000', hex: '41 41 41 41 41 41 41 41', ascii: 'AAAAAAAA' });
  assert.equal(rows[2].offset, '0010');
});

test('only 0xC2-0xF4 start a character: a stray invalid byte at the cut is left alone, a real unfinished start is dropped', () => {
  for (const stray of [0xc0, 0xc1, 0xf5, 0xf8, 0xff]) {
    const bytes = new Uint8Array([0x61, 0x62, stray]);
    assert.equal(withoutPartialCharacter(bytes).length, 3, `0x${stray.toString(16)} stays`);
    assert.equal(isBinaryPayload(bytes, true), true, `0x${stray.toString(16)} is not text`);
  }
  assert.equal(withoutPartialCharacter(new Uint8Array([0x61, 0xf5, 0x80])).length, 3, 'a continuation after an invalid lead stays too');
  for (const lead of [0xc2, 0xdf, 0xe0, 0xef, 0xf0, 0xf4]) {
    assert.equal(withoutPartialCharacter(new Uint8Array([0x61, lead])).length, 1, `0x${lead.toString(16)} alone is an unfinished start`);
  }
});

test('text cut by the server ends with an ellipsis; whole text does not', () => {
  assert.equal(bytesToPreviewText(enc('abc'), 512, true), 'abc…');
  assert.equal(bytesToPreviewText(enc('abc'), 512, false), 'abc');
  assert.equal(bytesToPreviewText(enc('abcdef'), 4, false), 'abcd…');
});

test('a cut preview that falls back to hex keeps the bytes of the unfinished character and ends with the marker', () => {
  const bytes = new Uint8Array([0xff, 0x61, 0xc5]);
  assert.equal(bytesToPreviewText(bytes, 512, true), 'ff 61 c5 …');
  assert.equal(bytesToPreviewText(bytes, 512, false), 'ff 61 c5');
});
