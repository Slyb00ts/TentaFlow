// ===== File: modules/tentabus/payload.js — reading a record's bytes for the screen: text or hex, headers, a reference to an attached file =====
//
// Pure functions shared by the message preview and the unprocessed-message
// list. A payload that is valid UTF-8 is shown as text; anything else as a
// bounded hex dump, never as replacement-character soup.

/**
 * `bytes` without a UTF-8 sequence left unfinished at its end. A preview the
 * server (or this screen) cut at a byte count can stop inside a character;
 * that is a cut, not invalid text.
 */
export function withoutPartialCharacter(bytes) {
  let lead = bytes.length - 1;
  while (lead >= 0 && lead >= bytes.length - 4 && (bytes[lead] & 0xc0) === 0x80) lead -= 1;
  if (lead < 0) return bytes;
  const b = bytes[lead];
  // Only 0xC2-0xF4 start a sequence; 0xC0, 0xC1 and 0xF5-0xFF are invalid anywhere and are left for the decoder to refuse.
  let need = 1;
  if (b >= 0xc2 && b <= 0xdf) need = 2;
  else if (b >= 0xe0 && b <= 0xef) need = 3;
  else if (b >= 0xf0 && b <= 0xf4) need = 4;
  return bytes.length - lead < need ? bytes.subarray(0, lead) : bytes;
}

/** A record's bytes as text (valid UTF-8) or a hex dump, cut at `maxBytes` with "…". `cut` = the bytes already are a prefix. */
export function bytesToPreviewText(bytes, maxBytes = 512, cut = false) {
  if (!bytes || bytes.length === 0) return '';
  const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let slice = arr.subarray(0, maxBytes);
  if (cut || arr.length > maxBytes) slice = withoutPartialCharacter(slice);
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(slice);
    return text + (arr.length > maxBytes || cut ? '…' : '');
  } catch {
    let hex = '';
    for (let i = 0; i < slice.length; i += 1) {
      hex += slice[i].toString(16).padStart(2, '0');
      if (i < slice.length - 1) hex += ' ';
    }
    return hex + (arr.length > maxBytes ? ' …' : '');
  }
}

export function findHeader(headers, key) {
  return (Array.isArray(headers) ? headers : []).find((h) => h.key === key) || null;
}

/** A header's value as text, or `null` when the record does not carry it. */
export function headerText(headers, key) {
  const h = findHeader(headers, key);
  return h ? bytesToPreviewText(h.value, 4096) : null;
}

/**
 * The attached-file reference a payload carries instead of the file itself
 * (`flow_engine::blob_store::BlobRef`: id, size_bytes, mime, sha256), or
 * `null` for an ordinary payload.
 */
export function parseBlobRefJson(bytes) {
  try {
    const obj = JSON.parse(bytesToPreviewText(bytes, 8192));
    if (obj && typeof obj === 'object'
      && typeof obj.id === 'string'
      && typeof obj.size_bytes === 'number'
      && typeof obj.mime === 'string'
      && typeof obj.sha256 === 'string') {
      return obj;
    }
  } catch {
    // Not JSON: an ordinary payload.
  }
  return null;
}

/**
 * Whether the bytes are something other than text a person can read: not
 * valid UTF-8, or text holding control characters (a Thrift or Protobuf
 * message that happens to be valid UTF-8 still is not text).
 */
export function isBinaryPayload(bytes, cut = false) {
  if (!bytes || bytes.length === 0) return false;
  let arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  if (cut) arr = withoutPartialCharacter(arr);
  let text;
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(arr);
  } catch {
    return true;
  }
  // eslint-disable-next-line no-control-regex
  return /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(text);
}

/** A hex view of the first `maxBytes` bytes: rows of `perRow` (16) as `{ offset, hex, ascii }` (offset in hex, `.` for a byte that is not printable ASCII). */
export function hexRows(bytes, maxBytes = 512, perRow = 16) {
  const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes || []);
  const slice = arr.subarray(0, maxBytes);
  const rows = [];
  for (let at = 0; at < slice.length; at += perRow) {
    const chunk = slice.subarray(at, at + perRow);
    const hex = Array.from(chunk, (b) => b.toString(16).padStart(2, '0'));
    rows.push({
      offset: at.toString(16).padStart(4, '0'),
      hex: `${hex.slice(0, 8).join(' ')}  ${hex.slice(8).join(' ')}`.trimEnd(),
      ascii: Array.from(chunk, (b) => (b >= 0x20 && b < 0x7f ? String.fromCharCode(b) : '.')).join(''),
    });
  }
  return rows;
}
