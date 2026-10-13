// ===== File: modules/tentabus/payload-view.js — a record's body as the screen shows it: text, or a labelled hex view when it is binary =====
//
// Shared by the message preview and the "Pokaż" window of an unprocessed
// message, so a Thrift, Protobuf or Avro body is never printed as broken
// characters.

import { escapeHtml } from '/js/utils.js';
import { T, fmtCount } from '/js/modules/tentabus/format.js';
import { bytesToPreviewText, hexRows, isBinaryPayload } from '/js/modules/tentabus/payload.js';

const HEX_BYTES = 512;

const rowsText = (rows, hexWidth) => rows.map((r) => `${r.offset}  ${r.hex.padEnd(hexWidth, ' ')} |${r.ascii}|`).join('\n');

/**
 * The markup of one body. `emptyText` is said for no bytes at all; text is
 * cut at `textBytes`; a binary body shows the first 512 bytes as offset, hex
 * and ASCII under "Treść binarna, N bajtów". `truncated` = the bytes are only
 * the start of a longer message (the wire does not carry the real length, so
 * the size is "co najmniej"). The hex view comes in two widths, 16 bytes a row
 * and 8; a media query on the viewport (not on the window) picks the one that
 * fits, and `data-role="payload"` sits on the wrapper that holds both.
 */
export function payloadHtml(bytes, { textBytes, emptyText, role = '', truncated = false }) {
  const attr = role ? ` data-role="${role}"` : '';
  if (!bytes || bytes.length === 0) return `<pre class="tb-payload"${attr}>${escapeHtml(emptyText)}</pre>`;
  if (!isBinaryPayload(bytes, truncated)) {
    return `<pre class="tb-payload"${attr}>${escapeHtml(bytesToPreviewText(bytes, textBytes, truncated))}</pre>`;
  }
  const size = bytes.length;
  const title = T(truncated ? 'fmt.payload_binary_atleast' : 'fmt.payload_binary', { count: fmtCount(size), n: size });
  const note = truncated
    ? T('fmt.payload_cut_more', { shown: fmtCount(Math.min(size, HEX_BYTES)) })
    : size > HEX_BYTES ? T('fmt.payload_cut', { shown: fmtCount(HEX_BYTES), count: fmtCount(size), n: size }) : '';
  // The note comes first, so a narrow window never leaves it below the fold.
  return `<div class="tb-binary-head" data-role="payload-binary">${escapeHtml(title)}</div>`
    + (note ? `<div class="muted tb-binary-note" data-role="payload-cut">${escapeHtml(note)}</div>` : '')
    + `<div class="tb-hex-view"${attr}>`
    + `<pre class="tb-payload tb-hex tb-hex--wide" data-role="payload-wide">${escapeHtml(rowsText(hexRows(bytes, HEX_BYTES, 16), 49))}</pre>`
    + `<pre class="tb-payload tb-hex tb-hex--narrow" data-role="payload-narrow">${escapeHtml(rowsText(hexRows(bytes, HEX_BYTES, 8), 23))}</pre>`
    + '</div>';
}
