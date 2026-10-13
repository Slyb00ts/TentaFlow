// ===== File: modules/tentabus/payload-view.js — a record's body as the screen shows it: text, or a labelled hex view when it is binary =====
//
// Shared by the message preview and the "Pokaż" window of an unprocessed
// message, so a Thrift, Protobuf or Avro body is never printed as broken
// characters.

import { escapeHtml } from '/js/utils.js';
import { T, fmtCount } from '/js/modules/tentabus/format.js';
import { bytesToPreviewText, hexRows, isBinaryPayload } from '/js/modules/tentabus/payload.js';

const HEX_BYTES = 512;

/**
 * The markup of one body. `emptyText` is said for no bytes at all; text is
 * cut at `textBytes`, a binary body shows the first 512 bytes as offset, hex
 * and ASCII columns under "Treść binarna, N bajtów".
 */
export function payloadHtml(bytes, { textBytes, emptyText, role = '' }) {
  const attr = role ? ` data-role="${role}"` : '';
  if (!bytes || bytes.length === 0) return `<pre class="tb-payload"${attr}>${escapeHtml(emptyText)}</pre>`;
  if (!isBinaryPayload(bytes)) {
    return `<pre class="tb-payload"${attr}>${escapeHtml(bytesToPreviewText(bytes, textBytes))}</pre>`;
  }
  const size = bytes.length;
  const rows = hexRows(bytes, HEX_BYTES)
    .map((r) => `${r.offset}  ${r.hex.padEnd(49, ' ')} |${r.ascii}|`)
    .join('\n');
  const cut = size > HEX_BYTES
    ? `<div class="muted" data-role="payload-cut">${escapeHtml(T('fmt.payload_cut', { shown: fmtCount(HEX_BYTES), count: fmtCount(size), n: size }))}</div>`
    : '';
  return `<div class="tb-binary-head" data-role="payload-binary">${escapeHtml(T('fmt.payload_binary', { count: fmtCount(size), n: size }))}</div>`
    + `<pre class="tb-payload tb-hex"${attr}>${escapeHtml(rows)}</pre>${cut}`;
}
