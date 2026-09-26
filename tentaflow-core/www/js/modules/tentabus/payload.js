// ===== File: modules/tentabus/payload.js — reading a record's bytes for the screen: text or hex, headers, a reference to an attached file =====
//
// Pure functions shared by the message preview and the unprocessed-message
// list. A payload that is valid UTF-8 is shown as text; anything else as a
// bounded hex dump, never as replacement-character soup.

/** A record's bytes as text (valid UTF-8) or a hex dump, cut at `maxBytes` with "…". */
export function bytesToPreviewText(bytes, maxBytes = 512) {
  if (!bytes || bytes.length === 0) return '';
  const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const slice = arr.subarray(0, maxBytes);
  try {
    const text = new TextDecoder('utf-8', { fatal: true }).decode(slice);
    return text + (arr.length > maxBytes ? '…' : '');
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
